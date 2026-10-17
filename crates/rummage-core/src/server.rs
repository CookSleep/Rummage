//! 连接处理：每个连接一个 tokio 任务，先握手（协议版本、令牌，限时 5 秒、单行 4 KiB），再逐行处理请求，同时转发广播事件。
//! 握手后收到无法识别的请求时，能读出 `id` 就回 `bad_request` 并保持连接，读不出才断开。
//! `Shutdown` 的回复写出后通知主循环正常退出。
//! 参数：启动时用索引数据库 `params` 表中的值覆盖参数表的默认值（通不过校验的丢弃）；`SetParam` 先写库，成功后再更新内存并广播。
//! 影响索引的参数改了值、或开关了文件类型时，再让流水线把不符合新设置的文件放回重建（`Pipeline::requeue`）；
//! 视频的设置看实际生效的值（`pipeline::video_opts`）。流水线未启动时不放回，启动时会检查。
//! 数据库打开失败时核心照常运行，需要数据库的请求返回 `unavailable`，消息是失败原因。
//! 同一连接上的只读请求（`Request::is_readonly`）并发处理，修改类请求与 `Shutdown` 按收到的顺序依次处理；只读请求等在它之前
//! 收到的修改类请求处理完才执行（先写后读）。回复按请求 ID 对应、可能乱序。请求在阻塞线程池中处理，读写数据库不占用异步执行器。
//! 扫描线程见 `scan`：`AddRoot` 成功后统计该根目录的文件数，统计完之前 `ListRoots` 中的 `scanning` 为真。
//! 实时监听、离线根目录的检查与定期对账见 `watch`：监听到的变化以 `ScanJob::Dirs` 排进同一个扫描线程做局部对账，
//! 有变化的根目录也广播 `ScanProgress`（`done` 为真）。每个扫描任务结束后按根目录的增删与离线状态启停监听。
//! 添加、移除根目录或修改排除规则后广播 `RootsChanged`，命令行与 MCP 改的也能让已打开的设置页刷新。
//! `RemoveRoot` 先让正在扫描该根目录的任务取消（扫描在每批写入前于数据库锁内检查），再删除；索引流水线中该根目录下
//! 正在处理的文件，写库前核对文件已不存在，结果丢弃。
//! 索引流水线（`pipeline`）：`start_pipeline` 之后运行，扫描结束、`AddRoot` 后唤醒它；`IndexStatus` 返回进度，
//! 进度事件 `IndexProgress` 由流水线广播。
//! 内存向量索引（`rummage_index::VIndex`）在打开数据库后、启动流水线前载入，载入失败按数据库不可用处理；`Search` 见 `search`。
//! 暂停建索引（手动暂停、自动暂停的检测线程）与睡眠唤醒见 `pause`。
//! 按文件 ID 的请求（文件信息、缩略图、片段正文）见 `files`；失败文件的列表与重试见 `failures`。
//! 设置页的请求（排除规则、外观、通知）见 `settings`；数据库状态、清空索引、缩略图的自动清理与搜索历史见 `data`。
//! 参数 `write_log_file` 在启动（`apply_log_setting`，由 main 调用）和修改时生效。
//! 首次使用引导（引导状态、模型下载与导入、代理、设备自检、预估）见 `setup`；`configure` 在启动流水线前交给它运行库与模型的位置。
use std::collections::BTreeMap;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, mpsc as std_mpsc};

use rummage_index::{Db, DbError, Filter, Root, RootError, VIndex, normalize_root};
use rummage_proto::{
    ApiError, ClientMsg, CoreMsg, Event, Hello, PROTOCOL_VERSION, ParamValue, Reply, Request,
    RootInfo, Welcome, decode, encode, params,
};
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{Notify, Semaphore, broadcast, mpsc, watch as tokio_watch};

use crate::pipeline::{self, Config, DisabledGpus, Pipeline};
use crate::resource::{self, MANUAL_KEY, System};
use crate::{HANDSHAKE_TIMEOUT, MAX_HELLO, MAX_LINE, lock, read_line, token_eq, write_line};

mod data;
mod failures;
mod files;
mod pause;
mod scan;
mod search;
mod settings;
mod setup;
mod watch;

use scan::ScanJob;

/// 每个连接同时处理的只读请求数上限，也是排队的修改类请求数上限
const MAX_INFLIGHT: usize = 16;

/// 数据库，与随它一起载入的内存向量索引、它所在的数据目录
struct Core {
    db: Arc<Mutex<Db>>,
    index: Arc<Mutex<VIndex>>,
    dir: PathBuf,
}

pub struct State {
    token: String,
    /// 打开或读取失败时为给用户看的原因
    db: Result<Core, String>,
    /// 数据库不可用、且清除后重建能解决（损坏、从迁移备份恢复失败）
    db_rebuildable: bool,
    /// 查询向量缓存
    queries: Mutex<search::Cache>,
    params: Arc<Mutex<BTreeMap<String, ParamValue>>>,
    events: broadcast::Sender<Event>,
    shutdown: Notify,
    /// 扫描线程的任务队列，`start_scanner` 之后才有
    scan_tx: OnceLock<std_mpsc::Sender<ScanJob>>,
    /// 正在扫描的根目录（局部对账可能涉及多个）及其取消标志
    scanning: Mutex<Option<(Vec<i64>, Arc<AtomicBool>)>>,
    /// 已排队或正在统计文件数的根目录，值为排队的统计任务数（`scan`）
    counting: Mutex<BTreeMap<i64, usize>>,
    /// 实时监听，`start_scanner` 之后才有
    watches: OnceLock<watch::Watches>,
    /// `start_pipeline` 之后才有
    pipeline: OnceLock<Pipeline>,
    /// 检测系统状态与读取时间的方式
    sys: System,
    /// 未到期的手动暂停，与 `meta` 表一致：外层为空表示没有，内层为到期时间（Unix 毫秒）。
    /// 判定并设置暂停原因的全过程持有这把锁，先于数据库锁获取
    manual: Mutex<Option<Option<i64>>>,
    /// Android 前台服务今日的后台额度已用完（`BackgroundQuota`），不持久化
    quota: AtomicBool,
    /// `configure` 之后才有
    config: OnceLock<Config>,
    setup: setup::Setup,
    /// 已排队、还没扫描完的扫描任务数
    scan_pending: AtomicUsize,
    /// 本次运行中出错停用的显卡，自检与流水线共用（`setup`）
    disabled: DisabledGpus,
}

impl State {
    /// 会读数据库，在阻塞线程中调用
    pub fn new(token: String, db: Result<Db, DbError>) -> Arc<Self> {
        Self::with_system(token, db, System::real())
    }

    pub(crate) fn with_system(token: String, db: Result<Db, DbError>, sys: System) -> Arc<Self> {
        let mut values = params::defaults();
        let mut manual = None;
        let db = db.and_then(|db| {
            if let Some(raw) = db.meta_get(MANUAL_KEY)? {
                manual = resource::decode_manual(&raw);
                if manual.is_none() {
                    log::warn!("忽略并删除格式不正确的手动暂停记录：{raw}");
                    if let Err(err) = db.meta_delete(MANUAL_KEY) {
                        log::warn!("无法删除格式不正确的手动暂停记录：{err}");
                    }
                }
            }
            for (key, raw) in db.params()? {
                let value = serde_json::from_value::<ParamValue>(raw)
                    .map_err(|err| err.to_string())
                    .and_then(|v| params::validate(&key, &v).map_err(|err| err.to_string()));
                match value {
                    Ok(v) => {
                        values.insert(key, v);
                    }
                    Err(err) => log::warn!("丢弃数据库中的参数 {key}：{err}"),
                }
            }
            let index = VIndex::load(&db)?;
            Ok((db, index))
        });
        let db_rebuildable = matches!(
            db,
            Err(DbError::Corrupt | DbError::Restore(_) | DbError::RestoreFailed { .. })
        );
        let db = db
            .map(|(db, index)| Core {
                dir: db.dir().to_path_buf(),
                db: Arc::new(Mutex::new(db)),
                index: Arc::new(Mutex::new(index)),
            })
            .map_err(|err| {
                log::error!("索引数据库不可用：{err}");
                err.to_string()
            });
        Arc::new(Self {
            token,
            db,
            db_rebuildable,
            queries: Mutex::new(search::Cache::default()),
            params: Arc::new(Mutex::new(values)),
            events: broadcast::channel(64).0,
            shutdown: Notify::new(),
            scan_tx: OnceLock::new(),
            scanning: Mutex::new(None),
            counting: Mutex::new(BTreeMap::new()),
            watches: OnceLock::new(),
            pipeline: OnceLock::new(),
            sys,
            manual: Mutex::new(manual),
            quota: AtomicBool::new(false),
            config: OnceLock::new(),
            setup: setup::Setup::default(),
            scan_pending: AtomicUsize::new(0),
            disabled: DisabledGpus::default(),
        })
    }

    /// 启动索引流水线；数据库不可用时不启动。会读写数据库，在阻塞线程中调用
    pub fn start_pipeline(&self, opts: pipeline::Options) {
        let Ok(core) = &self.db else {
            return;
        };
        if self.pipeline.get().is_some() {
            return;
        }
        // 启动前就按暂停原因决定是否领取作业，不会在手动暂停中先领出几个
        let mut manual = lock(&self.manual);
        let (paused, slow) = self.pause_reason(&mut manual);
        let p = Pipeline::start(
            core.db.clone(),
            core.index.clone(),
            self.params.clone(),
            self.events.clone(),
            opts,
            self.disabled.clone(),
            paused,
        );
        p.set_slow(slow);
        let _ = self.pipeline.set(p);
    }

    /// 正常退出前调用，处理中的作业放回待处理
    pub fn stop_pipeline(&self) {
        if let Some(p) = self.pipeline.get() {
            p.stop();
        }
    }

    /// 测试结束前调用：停止流水线，等各线程（流水线、设备检测等）释放数据库，临时目录才能删掉
    #[cfg(test)]
    pub(crate) fn close(self: Arc<Self>) {
        self.stop_pipeline();
        let db = self.db.as_ref().ok().map(|c| Arc::downgrade(&c.db));
        drop(self);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while db.as_ref().is_some_and(|w| w.strong_count() > 0) {
            assert!(
                std::time::Instant::now() < deadline,
                "等待各线程释放数据库超时"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// 立即计算需要更新的分数校准背景分布（评测在跑查询前调用），返回更新的个数；流水线未启动时报错
    pub fn calibrate(&self) -> Result<usize, String> {
        match self.pipeline.get() {
            Some(p) => p.calibrate(),
            None => Err("索引尚未启动，请稍后再试。".into()),
        }
    }

    fn wake_pipeline(&self) {
        if let Some(p) = self.pipeline.get() {
            p.wake();
        }
    }

    /// 等到有客户端请求退出
    pub async fn shutdown_requested(&self) {
        self.shutdown.notified().await
    }

    /// 在调用线程处理一个请求，与经管道收到的请求走同一条路径；无界面运行的 `eval`、`bench` 用。
    /// 会读写数据库、可能推理，在阻塞线程中调用
    pub fn request(self: &Arc<Self>, body: Request) -> Result<Reply, ApiError> {
        respond(body, self)
    }

    /// `start_pipeline` 之后才有
    pub fn pipeline(&self) -> Option<&Pipeline> {
        self.pipeline.get()
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    pub fn params(&self) -> BTreeMap<String, ParamValue> {
        lock(&self.params).clone()
    }
}

fn root_error(err: RootError) -> ApiError {
    let code = match err {
        RootError::Db(_) => ApiError::UNAVAILABLE,
        _ => ApiError::BAD_REQUEST,
    };
    api_error(code, err.to_string())
}

fn root_info(r: Root, state: &State) -> RootInfo {
    RootInfo {
        scanning: state.counting(r.id),
        id: r.id,
        path: r.path,
        offline: r.offline,
        files: r.files,
        excludes: r.excludes,
    }
}

fn api_error(code: &str, message: impl Into<String>) -> ApiError {
    ApiError {
        code: code.into(),
        message: message.into(),
    }
}

pub async fn handle<S>(stream: S, state: Arc<State>)
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    match session(stream, &state).await {
        // 对端关闭管道是正常断开
        Err(err) if err.kind() != ErrorKind::BrokenPipe => log::warn!("IPC 连接异常断开：{err}"),
        _ => {}
    }
}

async fn session<S>(stream: S, state: &Arc<State>) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (rd, mut wr) = tokio::io::split(stream);
    let mut rd = BufReader::new(rd);
    let mut buf = Vec::new();
    let line = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_line(&mut rd, &mut buf, MAX_HELLO))
        .await
        .map_err(|_| io::Error::new(ErrorKind::TimedOut, "握手超时"))??;
    let Some(line) = line else {
        return Ok(());
    };
    let reply = match decode::<Hello>(&line) {
        Err(_) => Err(api_error(ApiError::BAD_REQUEST, "握手消息格式不正确。")),
        Ok(hello) if hello.protocol != PROTOCOL_VERSION => {
            Err(api_error(ApiError::PROTOCOL_MISMATCH, {
                log::warn!(
                    "界面与核心进程的协议版本不一致：{} 与 {}",
                    hello.protocol,
                    PROTOCOL_VERSION
                );
                "Rummage 的组件版本不一致，请重新安装 Rummage。"
            }))
        }
        Ok(hello) if !token_eq(&hello.token, &state.token) => Err(api_error(
            ApiError::UNAUTHORIZED,
            "连接令牌不正确，请重新启动 Rummage。",
        )),
        Ok(_) => Ok(Welcome {
            protocol: PROTOCOL_VERSION,
            core_version: env!("CARGO_PKG_VERSION").into(),
        }),
    };
    write_line(&mut wr, &encode(&reply)).await?;
    if let Err(err) = reply {
        log::warn!("拒绝了一个 IPC 连接：{}", err.code);
        return Ok(());
    }

    // 回复和事件都经由同一个通道交给写入任务；通道有上限，对端不读时请求处理也会停下
    let (tx, mut rx) = mpsc::channel::<String>(64);
    let writer = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if write_line(&mut wr, &line).await.is_err() {
                break;
            }
        }
    });
    let mut events = state.events.subscribe();
    let event_tx = tx.clone();
    let forward = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(body) => {
                    if event_tx
                        .send(encode(&CoreMsg::Event { body }))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                // ParamsChanged 带完整的参数值，漏掉中间几条不影响结果；漏掉的 ScanProgress 可用 ListRoots 补上
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            }
        }
    });

    // 只读请求（`Request::is_readonly`）彼此并发：慢请求（搜索、预估）不挡住后面的请求。修改类请求与 `Shutdown`
    // 交给一个任务按收到的顺序依次处理，快速连发的设置不会乱序生效；`Shutdown` 之后不再读新请求。
    // 先写后读：修改类请求按收到的顺序编号，处理完一条就把 `done` 更新为它的编号；只读请求记下收到时已收到的
    // 修改类请求数，等这些都处理完才执行，读到的是修改后的状态。
    // 回复带请求 ID，整行经 `tx` 交给写入任务，不会交错
    let (wtx, mut wrx) = mpsc::channel::<(u64, Request)>(MAX_INFLIGHT);
    let (done_tx, done) = tokio_watch::channel(0u64);
    let mut received = 0u64;
    let (s, w) = (state.clone(), tx.clone());
    let serial = tokio::spawn(async move {
        while let Some((id, body)) = wrx.recv().await {
            let result = process(body, &s).await;
            done_tx.send_modify(|n| *n += 1);
            if w.send(encode(&CoreMsg::Response { id, result }))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let inflight = Arc::new(Semaphore::new(MAX_INFLIGHT));
    let mut stop = false;
    let result = loop {
        let line = match read_line(&mut rd, &mut buf, MAX_LINE).await {
            Ok(Some(line)) => line,
            Ok(None) => break Ok(()),
            Err(err) => break Err(err),
        };
        let (id, result) = match decode::<ClientMsg>(&line) {
            Ok(ClientMsg::Request { id, body }) if !body.is_readonly() => {
                stop = body == Request::Shutdown;
                received += 1;
                // 队列满时先不读新请求；处理任务已退出（对端断开）时结束
                if wtx.send((id, body)).await.is_err() || stop {
                    break Ok(());
                }
                continue;
            }
            Ok(ClientMsg::Request { id, body }) => {
                // 处理中的请求达到上限时先不读新请求，对端不读回复时也会停下
                let permit = inflight
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("信号量不会被关闭");
                let state = state.clone();
                let tx = tx.clone();
                let mut done = done.clone();
                let before = received;
                tokio::spawn(async move {
                    // 处理任务已退出时 `wait_for` 返回错误，此时也不会再有修改，照常执行
                    let _ = done.wait_for(|n| *n >= before).await;
                    let result = process(body, &state).await;
                    let _ = tx.send(encode(&CoreMsg::Response { id, result })).await;
                    drop(permit);
                });
                continue;
            }
            // 类型未知或字段不对：能读出 id 就针对这条请求回错误，连接保持
            Err(err) => match serde_json::from_str::<serde_json::Value>(&line)
                .ok()
                .and_then(|v| v.get("id")?.as_u64())
            {
                Some(id) => (
                    id,
                    Err(api_error(
                        ApiError::BAD_REQUEST,
                        format!("无法识别的请求：{err}"),
                    )),
                ),
                None => break Err(io::Error::new(ErrorKind::InvalidData, err)),
            },
        };
        if tx
            .send(encode(&CoreMsg::Response { id, result }))
            .await
            .is_err()
        {
            break Ok(());
        }
    };
    forward.abort();
    // 等排队的修改类请求处理完、处理中的请求回复、已排队的回复写完再断开
    drop(wtx);
    let _ = serial.await;
    drop(tx);
    let _ = writer.await;
    if stop {
        state.shutdown.notify_one();
    }
    result
}

/// 在阻塞线程池中处理一个请求
async fn process(body: Request, state: &Arc<State>) -> Result<Reply, ApiError> {
    let state = state.clone();
    tokio::task::spawn_blocking(move || respond(body, &state))
        .await
        .unwrap_or_else(|err| {
            log::error!("处理请求时出错：{err}");
            Err(api_error(
                ApiError::INTERNAL,
                "后台服务处理时出错，请重试。",
            ))
        })
}

/// 数据库不可用时返回 `unavailable`
fn opened(state: &State) -> Result<&Core, ApiError> {
    state
        .db
        .as_ref()
        .map_err(|msg| api_error(ApiError::UNAVAILABLE, msg.clone()))
}

/// 需要数据库的请求先取得数据库；不可用时返回 `unavailable`
fn db(state: &State) -> Result<MutexGuard<'_, Db>, ApiError> {
    opened(state).map(|c| lock(&c.db))
}

/// 读写数据库等失败：错误本身是给用户看的整句，原样返回
fn unavailable(err: impl std::fmt::Display) -> ApiError {
    api_error(ApiError::UNAVAILABLE, err.to_string())
}

fn respond(body: Request, state: &Arc<State>) -> Result<Reply, ApiError> {
    match body {
        Request::Ping => Ok(Reply::Pong),
        Request::GetParams => {
            // 数据库不可用时内存中只有默认值，不能当作用户的设置返回
            opened(state)?;
            Ok(Reply::Params {
                values: state.params(),
            })
        }
        Request::SetParam { key, value } => {
            let value = params::validate(&key, &value)
                .map_err(|err| api_error(ApiError::BAD_REQUEST, err.to_string()))?;
            // 持有数据库锁直到广播，保证库中、内存中的值与广播顺序一致
            let db = db(state)?;
            // 值没变（从未保存过的参数按默认值算）：不重新扫描、不重建引擎、不重建索引
            if crate::pipeline::param(&state.params(), &key) == value {
                return Ok(Reply::Done);
            }
            let json = serde_json::to_value(value)
                .expect("ParamValue 只含布尔、整数、浮点数，序列化不会失败");
            // 原始错误已由数据库模块写入日志
            db.set_param(&key, &json).map_err(|err| {
                log::error!("无法保存参数 {key}：{err}");
                api_error(
                    ApiError::UNAVAILABLE,
                    "设置未能保存：索引数据库写入失败，请检查磁盘空间后重试。",
                )
            })?;
            let mut values = lock(&state.params);
            // 大小、时长、像素上限或开启的类型改了，重新扫描以按新设置判定未完成索引的文件
            let rescan = [
                "max_doc_mb",
                "max_image_mb",
                "max_audio_min",
                "max_image_mp",
                "index_documents",
                "index_images",
                "index_audio",
                "index_video",
            ]
            .contains(&key.as_str());
            let idle = key == "engine_idle_min";
            // 是否处理图片、音频、视频以 mmproj 可用且 index_images / index_audio / index_video 开启为准；推理线程数、
            // 推理进程的内存上限、启动时限与单次推理时限在创建引擎时读取。改了就重建引擎（预处理线程数在启动流水线时决定，重启后才变）
            let reload = [
                "index_images",
                "index_audio",
                "index_video",
                "index_threads",
                "worker_mem_gb",
                "worker_init_timeout_s",
                "infer_timeout_s",
            ]
            .contains(&key.as_str());
            // 影响索引的参数改了值就重建受影响的部分；值没变的已在前面返回
            let def = params::def(&key).expect("validate 已确认参数存在");
            let mut affected = def.affects_index;
            let void = pipeline::void_kinds(&key);
            // 视频的高级项只在预设为“自定义”时生效：实际设置没变就不重建
            let video_before = pipeline::video_opts(&values);
            let pause = [
                "pause_on_battery",
                "pause_on_fullscreen",
                "min_free_disk_gb",
            ]
            .contains(&key.as_str());
            let log = key == "write_log_file";
            let toggle = [
                "index_documents",
                "index_images",
                "index_audio",
                "index_video",
            ]
            .contains(&key.as_str());
            values.insert(key, value);
            if def.index_kinds == ["video"] && pipeline::video_opts(&values) == video_before {
                affected = false;
            }
            if log {
                apply_log_setting(&values);
            }
            // 在锁内广播，保证各连接收到的事件顺序与修改顺序一致；没有订阅者时 send 返回错误，忽略
            let _ = state.events.send(Event::ParamsChanged {
                values: values.clone(),
            });
            if rescan {
                state.queue_scan(ScanJob::All);
            }
            // 资源占用的参数立即生效；判定暂停要取数据库锁和参数锁，先释放
            drop(values);
            drop(db);
            let Some(p) = state.pipeline.get() else {
                if pause {
                    state.refresh_pause();
                }
                return Ok(Reply::Done);
            };
            if idle {
                p.apply_engine_idle();
            }
            if reload {
                p.reload();
            }
            if pause {
                state.refresh_pause();
            }
            // 开启文件类型后，按当前设置检查这一类已建好的文件
            if !affected && !toggle {
                return Ok(Reply::Done);
            }
            // 正在处理的这几类作业按旧参数计算，结果作废；打开或关闭文件类型时正在处理的文档的内嵌媒体也按旧设置算的
            let active: &[&str] = if affected { &void } else { &["document"] };
            match p.requeue(active) {
                Ok(n) => {
                    if !n.is_empty() {
                        log::info!("参数修改后重建 {n:?} 个文件的索引。");
                    }
                    Ok(Reply::Done)
                }
                // 参数已保存，照常回复成功；下次启动时会再检查
                Err(err) => {
                    log::error!("参数已保存，但无法安排重建索引：{err}");
                    Ok(Reply::DoneWithWarning {
                        message: "设置已保存，但未能安排重建索引，已建好的文件仍按旧设置。重新启动 Rummage 后会再试。"
                            .into(),
                    })
                }
            }
        }
        Request::PauseIndexing { until } => state.set_manual_pause(Some(until)),
        Request::ResumeIndexing => state.set_manual_pause(None),
        Request::BackgroundQuota { exhausted } => {
            state.quota.store(exhausted, Ordering::Relaxed);
            state.refresh_pause();
            Ok(Reply::Done)
        }
        // 回复写出后由 session 通知主循环退出
        Request::Shutdown => Ok(Reply::Done),
        Request::ListRoots => {
            let roots = db(state)?
                .list_roots()
                .map_err(|err| api_error(ApiError::UNAVAILABLE, err.to_string()))?;
            Ok(Reply::Roots {
                roots: roots.into_iter().map(|r| root_info(r, state)).collect(),
            })
        }
        Request::AddRoot { path, excludes } => {
            // 读属性、规范化、卷检查要访问磁盘，不占数据库锁
            let root = normalize_root(Path::new(&path)).map_err(root_error)?;
            let root = {
                let db = db(state)?;
                let id = db.add_root(&root).map_err(root_error)?;
                // 排除规则要按根目录的路径解析，只能在添加之后保存；写法不对时撤回添加，扫描还没开始
                let excludes = match db.set_root_excludes(id, &excludes) {
                    Ok(excludes) => excludes,
                    Err(err) => {
                        if let Err(err) = db.remove_root(id) {
                            log::error!("无法撤回添加的文件夹：{err}");
                        }
                        return Err(root_error(err));
                    }
                };
                Root {
                    id,
                    path: root,
                    offline: false,
                    files: 0,
                    excludes,
                }
            };
            // 先开始监听：首次扫描可能很久，期间的变化攒到扫描结束后局部对账
            watch::sync(state);
            state.queue_count(vec![root.id]);
            state.wake_pipeline();
            let _ = state.events.send(Event::RootsChanged);
            Ok(Reply::Root {
                root: root_info(root, state),
            })
        }
        Request::RemoveRoot { id } => {
            // 先置取消标志再取数据库锁：扫描的下一批写入在锁内检查到它，不会再写入该根目录
            if let Some((cur, cancel)) = &*state.scanning()
                && cur.contains(&id)
            {
                cancel.store(true, Ordering::Relaxed);
            }
            db(state)?.remove_root(id).map_err(root_error)?;
            data::drop_orphan_thumbs(state);
            watch::sync(state);
            let _ = state.events.send(Event::RootsChanged);
            // 暂停或空闲时流水线不会自己广播，计数要在这里更新
            if let Some(p) = state.pipeline.get() {
                p.broadcast();
            }
            Ok(Reply::Done)
        }
        Request::IndexStatus => {
            opened(state)?;
            let Some(p) = state.pipeline.get() else {
                return Err(api_error(
                    ApiError::UNAVAILABLE,
                    "索引尚未启动，请稍后再试。",
                ));
            };
            p.status()
                .map(Reply::Index)
                .map_err(|err| api_error(ApiError::UNAVAILABLE, err.to_string()))
        }
        Request::Search {
            text,
            image_path,
            similar,
            kinds,
            root_ids,
            since,
            until,
            limit,
            snippets,
        } => {
            let filter = Filter {
                kinds,
                root_ids,
                since,
                until,
                exclude_file: None,
            };
            search::respond(state, text, image_path, similar, filter, limit, snippets)
        }
        Request::FailedFiles { after, limit } => failures::list(state, after, limit),
        Request::RetryFailed { file_id } => failures::retry(state, Some(file_id)),
        Request::RetryAllFailed => failures::retry(state, None),
        Request::IgnoreFailed { file_id } => failures::ignore(state, Some(file_id)),
        Request::IgnoreAllFailed => failures::ignore(state, None),
        Request::FileInfo { file_id } => files::info(state, file_id),
        Request::Thumbnail { file_id } => files::thumbnail(state, file_id),
        Request::SegmentText { segment_id } => files::segment_text(state, segment_id),
        Request::VideoFrame { file_id, ms } => files::video_frame(state, file_id, ms),
        Request::RegionThumb { segment_id } => files::region_thumb(state, segment_id),
        body @ (Request::SetupStatus
        | Request::FinishSetup
        | Request::SetOnboardingStep { .. }
        | Request::DownloadModels { .. }
        | Request::ImportModels { .. }
        | Request::CancelModelTask
        | Request::VerifyModel { .. }
        | Request::VerifyModels { .. }
        | Request::DeleteModel { .. }
        | Request::SetProxy { .. }
        | Request::RunSelftest { .. }
        | Request::SetPreferredDevice { .. }
        | Request::UnblockGpu { .. }
        | Request::Estimate) => setup::respond(state, body),
        body @ (Request::SetRootExcludes { .. }
        | Request::GetAppearance
        | Request::SetAppearance { .. }
        | Request::GetNotices
        | Request::DismissNotice { .. }) => settings::respond(state, body),
        body @ (Request::DbStatus
        | Request::ClearIndex
        | Request::GetHistory
        | Request::AddHistory { .. }
        | Request::RemoveHistory { .. }
        | Request::ClearHistory) => data::respond(state, body),
    }
}

/// 参数 `write_log_file`：只控制 core.log，stderr 的输出不受影响（日志的写入端见 main 的 `init_log`）
pub fn apply_log_setting(values: &BTreeMap<String, ParamValue>) {
    let on = params::flag(values, "write_log_file");
    crate::LOG_TO_FILE.store(on, Ordering::Relaxed);
}

#[cfg(test)]
mod tests;
