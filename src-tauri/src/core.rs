//! 与核心进程的连接和守护：
//! 用 `rummage_core::client::connect_or_start` 连接核心，连不上就拉起界面进程同目录下的 `rummage-core.exe`
//! （带 `--data-dir`，不显示窗口，不继承界面的句柄），最多等 10 秒管道出现；核心版本与界面不同时让它退出后拉起新的；
//! 之后维护请求 id 与回复的对应关系（请求取消或超时时清理），把核心事件以 Tauri 事件 `core-event` 转发给前端。
//! 守护只针对自己拉起的核心：它异常退出时把退出码和 `core.log` 最后 200 行写进 app.log（由 `connect_or_start` 记录）。
//! 无论核心由谁拉起，断开后都间隔 1 秒重新走“连接 → 拉起”流程；60 秒内失败 5 次就停止重启，报告 `failed`，
//! 界面的“重试”（`restart_core`）重新进入守护；守护仍在运行时不做任何事。
//! 状态以 `core-status` 推给前端。界面退出时不结束核心，核心要常驻；只有更改数据目录、从“索引数据库无法打开”退出时
//! 由 `stop` 请它退出，此后不再重启；托盘的“退出”、设置为关闭窗口时退出的关窗由 `quit` 连同核心一起结束，
//! 核心不按时退出就强制结束；安装更新前同样结束核心（`shutdown`），安装程序没能启动时由 `resume` 重新拉起。
//! 连上后和参数变化时把日志开关（`write_log_file`）交给 `diagnostics`；连上、断开和索引进度变化时更新托盘。
//! Android 上没有核心进程：后台服务在本进程中（`android::engine`），守护只负责连接，断开后间隔 1 秒重新连接同一个服务，
//! 不计失败次数
use std::collections::HashMap;
#[cfg(not(target_os = "android"))]
use std::collections::VecDeque;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(not(target_os = "android"))]
use std::time::Instant;

use rummage_core::client::Client;
use rummage_core::{MAX_LINE, Stream, read_line, write_line};
use rummage_proto::{ApiError, ClientMsg, CoreMsg, Event, Reply, Request, decode, encode};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::io::{BufReader, ReadHalf};
use tokio::sync::{mpsc, oneshot};

#[cfg(desktop)]
use crate::tray;
use crate::{diagnostics, platform};

/// 对应前端 `src/types.ts` 的 `CoreStatus`
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Starting,
    Running,
    Restarting,
    Failed,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Reply, ApiError>>>>>;

/// 当前连接：发往写入任务的通道，以及等待回复的请求
struct Link {
    tx: mpsc::Sender<String>,
    pending: Pending,
    /// 核心进程的 PID，退出时用来确认它已结束
    pid: u32,
}

/// 请求结束（收到回复、被取消或超时）时从 pending 中移除自己的 id；收到回复时已被移除，再移除一次无影响
struct PendingGuard {
    pending: Pending,
    id: u64,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.pending.lock().expect(LOCK).remove(&self.id);
    }
}

pub struct Core {
    status: Mutex<Status>,
    link: Mutex<Option<Link>>,
    next_id: AtomicU64,
    /// 界面即将重启或退出、已请核心退出（`stop`），守护不再重新拉起
    stopping: AtomicBool,
    /// 守护正在运行（`supervise`），同一时间只有一个
    supervising: AtomicBool,
}

impl Default for Core {
    fn default() -> Self {
        Self {
            status: Mutex::new(Status::Starting),
            link: Mutex::new(None),
            next_id: AtomicU64::new(1),
            stopping: AtomicBool::new(false),
            supervising: AtomicBool::new(false),
        }
    }
}

const LOCK: &str = "持锁期间只做读写字段，不会 panic";

#[tauri::command]
pub fn core_status(core: tauri::State<'_, Core>) -> Status {
    *core.status.lock().expect(LOCK)
}

/// 后台服务无法启动时界面的“重试”：重新进入守护；守护仍在运行（核心正在运行或重启中）、或正在退出时不做任何事。
/// 数据目录不可用时重试必然失败，返回请重新启动 Rummage 的错误
#[tauri::command]
pub fn restart_core<R: Runtime>(app: AppHandle<R>) -> Result<(), ApiError> {
    app.state::<crate::data::DataDir>().dir()?;
    if app.state::<Core>().stopping.load(Ordering::Relaxed) {
        return Ok(());
    }
    supervise(&app);
    Ok(())
}

fn set_status<R: Runtime>(app: &AppHandle<R>, status: Status) {
    *app.state::<Core>().status.lock().expect(LOCK) = status;
    if let Err(err) = app.emit("core-status", status) {
        log::warn!("无法推送核心进程状态：{err}");
    }
}

/// 与核心的连接断开（未连接）时的错误
pub fn unavailable() -> ApiError {
    ApiError {
        code: ApiError::UNAVAILABLE.into(),
        message: "后台服务未连接，请稍后再试。".into(),
    }
}

/// 向核心发出请求并等待对应 id 的回复；连接断开时返回 `unavailable`
pub async fn request<R: Runtime>(app: &AppHandle<R>, body: Request) -> Result<Reply, ApiError> {
    let core = app.state::<Core>();
    let id = core.next_id.fetch_add(1, Ordering::Relaxed);
    let (reply_tx, reply_rx) = oneshot::channel();
    let (tx, _guard) = {
        let link = core.link.lock().expect(LOCK);
        let Some(link) = link.as_ref() else {
            return Err(unavailable());
        };
        link.pending.lock().expect(LOCK).insert(id, reply_tx);
        let guard = PendingGuard {
            pending: link.pending.clone(),
            id,
        };
        (link.tx.clone(), guard)
    };
    if tx
        .send(encode(&ClientMsg::Request { id, body }))
        .await
        .is_err()
    {
        return Err(unavailable());
    }
    reply_rx.await.unwrap_or_else(|_| Err(unavailable()))
}

/// 只需要 `Done` 回复的请求
pub async fn done<R: Runtime>(app: &AppHandle<R>, body: Request) -> Result<(), ApiError> {
    match request(app, body).await? {
        Reply::Done => Ok(()),
        other => Err(crate::search::unexpected(&other)),
    }
}

/// 请核心退出且不再重新拉起：更改数据目录前（迁移要等核心释放数据目录）和退出界面时用；核心未连接时直接返回
pub async fn stop<R: Runtime>(app: &AppHandle<R>) {
    app.state::<Core>().stopping.store(true, Ordering::Relaxed);
    match tokio::time::timeout(Duration::from_secs(5), request(app, Request::Shutdown)).await {
        Ok(Ok(_)) => {}
        Ok(Err(err)) => log::warn!("请核心进程退出时出错：{}", err.message),
        Err(_) => log::warn!("核心进程 5 秒内没有回复退出请求。"),
    }
}

/// 托盘的“退出”、设置为关闭窗口时退出的关窗与“索引数据库无法打开”时的退出：请核心退出，5 秒内没有结束就强制结束，再退出界面进程。
/// “重启 Rummage 后再检测”等提示要求核心随之重启，不能只退出界面。等待期间先关窗口、藏图标，让退出立即可见
pub async fn quit<R: Runtime>(app: &AppHandle<R>) {
    if let Some(win) = app.get_webview_window("main")
        && let Err(err) = win.destroy()
    {
        log::warn!("退出时无法关闭主窗口：{err}");
    }
    #[cfg(desktop)]
    tray::hide(app);
    shutdown(app).await;
    app.exit(0);
}

/// 请核心退出并等它的进程结束，5 秒内没有结束就强制结束；此后守护不再重新拉起（退出界面与安装更新前）
pub async fn shutdown<R: Runtime>(app: &AppHandle<R>) {
    let pid = app
        .state::<Core>()
        .link
        .lock()
        .expect(LOCK)
        .as_ref()
        .map(|link| link.pid);
    stop(app).await;
    if let Some(pid) = pid {
        let exited = tauri::async_runtime::spawn_blocking(move || {
            rummage_core::wait_exit(pid, Duration::from_secs(5))
        })
        .await
        .unwrap_or(false);
        if !exited {
            log::warn!("核心进程 5 秒内没有退出，强制结束。");
            if let Err(err) = platform::kill(pid) {
                log::error!("无法强制结束核心进程：{err}");
            }
        }
    }
}

/// `shutdown` 之后界面不退出时（安装程序没能启动）重新拉起核心。
/// 守护可能还没发现核心已退出，等它结束后再进入；3 秒后仍在运行时它会自行重连
#[cfg(desktop)]
pub async fn resume<R: Runtime>(app: &AppHandle<R>) {
    let core = app.state::<Core>();
    core.stopping.store(false, Ordering::Relaxed);
    for _ in 0..60 {
        if !core.supervising.load(Ordering::Acquire) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    supervise(app);
}

/// 读取核心发来的消息直到断开：回复交给等待中的请求，事件转发给前端
async fn read_loop<R: Runtime>(
    app: AppHandle<R>,
    mut reader: BufReader<ReadHalf<Stream>>,
    pending: Pending,
) {
    let mut buf = Vec::new();
    loop {
        let line = match read_line(&mut reader, &mut buf, MAX_LINE).await {
            Ok(Some(line)) => line,
            Ok(None) => return,
            // 核心退出时管道断开，属于正常断开
            Err(err) if err.kind() == ErrorKind::BrokenPipe => return,
            Err(err) => {
                log::warn!("读取核心进程消息失败：{err}");
                return;
            }
        };
        match decode::<CoreMsg>(&line) {
            Ok(CoreMsg::Response { id, result }) => {
                if let Some(tx) = pending.lock().expect(LOCK).remove(&id) {
                    let _ = tx.send(result);
                }
            }
            Ok(CoreMsg::Event { body }) => {
                match &body {
                    Event::ParamsChanged { values } => diagnostics::apply_params(values),
                    #[cfg(desktop)]
                    Event::IndexProgress(status) => tray::update(Some(status)),
                    _ => {}
                }
                if let Err(err) = app.emit("core-event", body) {
                    log::warn!("无法转发核心进程事件：{err}");
                }
            }
            Err(err) => log::warn!("无法解析核心进程的消息：{err}"),
        }
    }
}

/// 处理一个已握手的连接，直到断开
async fn session<R: Runtime>(app: &AppHandle<R>, client: Client) {
    let mut writer = client.writer;
    let (tx, mut rx) = mpsc::channel::<String>(64);
    let writer_task = tauri::async_runtime::spawn(async move {
        while let Some(line) = rx.recv().await {
            if write_line(&mut writer, &line).await.is_err() {
                break;
            }
        }
    });
    let pending = Pending::default();
    let core = app.state::<Core>();
    *core.link.lock().expect(LOCK) = Some(Link {
        tx,
        pending: pending.clone(),
        pid: client.server_pid,
    });
    let reader_task = tauri::async_runtime::spawn(read_loop(app.clone(), client.reader, pending));
    // 握手之外再 Ping 一次，确认核心能正常处理请求后才报告 running
    match tokio::time::timeout(Duration::from_secs(5), request(app, Request::Ping)).await {
        Ok(Ok(Reply::Pong)) => {
            set_status(app, Status::Running);
            // 日志开关存在核心的参数中；数据库不可用时读不到，保持开启
            if let Ok(Reply::Params { values }) = request(app, Request::GetParams).await {
                diagnostics::apply_params(&values);
            }
            #[cfg(desktop)]
            tray::refresh(app).await;
            let _ = reader_task.await;
        }
        _ => {
            log::warn!("核心进程没有响应 Ping，断开重连。");
            reader_task.abort();
        }
    }
    // 丢弃连接后，等待中的请求收到 unavailable
    *core.link.lock().expect(LOCK) = None;
    writer_task.abort();
    #[cfg(desktop)]
    tray::update(None);
}

/// 在后台连接或拉起核心并守护它（启动时与“重试”时）；守护已在运行时不做任何事
pub fn supervise<R: Runtime>(app: &AppHandle<R>) {
    if app.state::<Core>().supervising.swap(true, Ordering::AcqRel) {
        return;
    }
    set_status(app, Status::Starting);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let failed = run(&app).await;
        let core = app.state::<Core>();
        // 在状态锁内报告失败并清掉标志：新的守护要先拿到这把锁才能报告 starting，不会被这里的 failed 覆盖；
        // 标志清掉后界面随即点“重试”就能重新进入守护
        let mut status = core.status.lock().expect(LOCK);
        if failed {
            *status = Status::Failed;
            if let Err(err) = app.emit("core-status", Status::Failed) {
                log::warn!("无法推送核心进程状态：{err}");
            }
        }
        core.supervising.store(false, Ordering::Release);
    });
}

/// 守护循环（Android，见模块说明）；后台服务无法启动时返回是，界面退出（`stop`）时返回否
#[cfg(target_os = "android")]
async fn run<R: Runtime>(app: &AppHandle<R>) -> bool {
    let Some(dir) = app
        .state::<crate::data::DataDir>()
        .dir()
        .ok()
        .map(PathBuf::from)
    else {
        log::error!("数据目录不可用，无法启动后台服务。");
        return true;
    };
    let state = match crate::android::engine(&dir).await {
        Ok(state) => state,
        Err(err) => {
            log::error!("无法启动后台服务：{err}");
            return true;
        }
    };
    let stopping = || app.state::<Core>().stopping.load(Ordering::Relaxed);
    loop {
        if stopping() {
            return false;
        }
        match rummage_core::client::connect_local(&state).await {
            Ok(client) => session(app, client).await,
            Err(err) => log::warn!("后台服务连接失败：{err}"),
        }
        if stopping() {
            return false;
        }
        set_status(app, Status::Restarting);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// 守护循环；无法启动、停止重启时返回是，界面退出（`stop`）时返回否
#[cfg(not(target_os = "android"))]
async fn run<R: Runtime>(app: &AppHandle<R>) -> bool {
    let Some(dir) = app
        .state::<crate::data::DataDir>()
        .dir()
        .ok()
        .map(PathBuf::from)
    else {
        log::error!("数据目录不可用，无法启动核心进程。");
        return true;
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe.with_file_name(format!("rummage-core{}", std::env::consts::EXE_SUFFIX)),
        Err(err) => {
            log::error!("无法确定核心进程的位置：{err}");
            return true;
        }
    };
    // 自己拉起且尚未确认退出的核心
    let mut own = None;
    let mut failures = VecDeque::new();
    let stopping = || app.state::<Core>().stopping.load(Ordering::Relaxed);
    loop {
        if stopping() {
            return false;
        }
        match rummage_core::client::connect_or_start(&dir, &exe, &mut own).await {
            Ok(client) => session(app, client).await,
            Err(err) => log::warn!("核心进程连接失败：{err}"),
        }
        if stopping() {
            return false;
        }
        // 自己拉起的核心断开时通常已经退出，等它的退出记录写完再重启，也避免重复拉起
        if let Some(exit) = own.as_mut()
            && tokio::time::timeout(Duration::from_secs(3), exit)
                .await
                .is_ok()
        {
            own = None;
        }
        let now = Instant::now();
        failures.push_back(now);
        while failures
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            failures.pop_front();
        }
        if failures.len() >= 5 {
            log::error!("核心进程 60 秒内 5 次连接或启动失败，停止重启。");
            return true;
        }
        set_status(app, Status::Restarting);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
