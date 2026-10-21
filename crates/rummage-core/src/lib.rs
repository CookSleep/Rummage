//! 核心进程的本地 IPC：命名管道 `\\.\pipe\rummage-core-<当前用户 SID>-<数据目录哈希>`，
//! 不同数据目录的核心互不干扰。消息为换行分隔的 JSON（`rummage_proto`）。
//! 安全：管道的安全描述符只允许当前用户访问，并拒绝远程客户端；客户端发出令牌前先核对服务端进程属于当前用户，
//! 防止同名管道被抢先创建后骗取令牌。连接后第一条消息必须是带令牌的 `Hello`，握手限时 5 秒、单行 4 KiB，
//! 认证之后单行上限才放宽到 16 MiB。
//! 令牌在核心每次启动时随机生成，写入数据目录的 `core.token`，文件同样只授予当前用户。
//! 单实例：第一个管道实例以 `FILE_FLAG_FIRST_PIPE_INSTANCE` 创建，已有核心在运行时创建失败（`BindError::AlreadyRunning`）。
//! 库部分同时供界面进程使用（管道名、读取令牌、`client::connect` 握手），平台相关部分在 `platform` 中。
//! Android 上没有核心进程：界面进程用 `start` 做同样的初始化，再经 `client::connect_local` 不经管道连接
pub mod client;
pub mod models;
pub mod pipeline;
pub mod proxy;
mod resource;
mod server;

#[cfg(target_os = "android")]
#[path = "android.rs"]
mod platform;
#[cfg(not(any(windows, target_os = "android")))]
#[path = "other.rs"]
mod platform;
#[cfg(windows)]
#[path = "windows.rs"]
mod platform;

use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rummage_index::Db;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::pipeline::Config;

pub use platform::{
    Listener, Stream, bind, lower_priority, on_power, on_session_end, process_memory,
    send_session_end, serve, wait_exit, write_private,
};
pub use server::{State, apply_log_setting};

/// 系统电源事件，由 `on_power` 的回调收到
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Power {
    /// 即将睡眠或休眠
    Suspend,
    /// 已从睡眠或休眠中唤醒
    Resume,
}

/// 参数 `write_log_file`：为假时不写 core.log（`apply_log_setting` 设置，main 的日志写入端读取）
pub static LOG_TO_FILE: AtomicBool = AtomicBool::new(true);

/// 认证后单行消息的上限，超过即断开，防止对端让进程无限占用内存
pub const MAX_LINE: usize = 16 * 1024 * 1024;
/// 握手行的上限：认证之前不允许对端占用更多内存
pub const MAX_HELLO: usize = 4 * 1024;
/// 握手（客户端的整个连接过程、服务端读握手行）的时限，防止对端连上后不说话一直占着连接
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// 当前用户的核心已在运行（同一数据目录）时，新拉起的核心进程的退出码；拉起方据此继续等先启动的那个
pub const EXIT_ALREADY_RUNNING: u8 = 2;

/// 取得锁；持锁的线程 panic 过也照常使用（被保护的数据在 panic 前已整体写入或未改）
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, thiserror::Error)]
pub enum BindError {
    #[error("Rummage 后台服务已在运行。")]
    AlreadyRunning,
    #[error("管道名被其他程序占用，Rummage 后台服务无法启动。")]
    Occupied,
    #[error("无法创建 IPC 管道：{0}")]
    Io(#[from] io::Error),
}

/// 打开索引数据库并载入参数，再启动索引流水线（先恢复上次中断的作业）、扫描线程（对全部根目录对账一次）与暂停检测；
/// 核心进程与 Android 的进程内后台服务共用。数据库打开失败时照常返回，需要数据库的请求返回 `unavailable`。
/// 会读写数据库、读系统状态，在阻塞线程中调用
pub fn start(token: String, data_dir: &Path, cfg: Config) -> Arc<State> {
    // 上次退出时没来得及删除的内嵌音视频临时文件
    match std::fs::remove_dir_all(&cfg.embed_tmp) {
        Err(err) if err.kind() != ErrorKind::NotFound => {
            log::warn!("无法清空 {}：{err}", cfg.embed_tmp.display());
        }
        _ => {}
    }
    let state = State::new(token, Db::open(data_dir));
    apply_log_setting(&state.params());
    state.configure(cfg.clone());
    state.start_pipeline(if cfg.litert.is_some() {
        pipeline::Options::litert(cfg)
    } else {
        pipeline::Options::local(cfg)
    });
    state.start_scanner();
    state.start_monitor();
    state
}

pub fn pipe_name(data_dir: &Path) -> io::Result<String> {
    Ok(format!(
        r"\\.\pipe\rummage-core-{}-{:016x}",
        platform::user_sid()?,
        dir_hash(data_dir)
    ))
}

/// 数据目录的 FNV-1a 64 位哈希。先规范化：统一用反斜杠、转小写、去掉 `\\?\` 前缀和末尾分隔符，
/// 同一目录的不同写法得到同一个管道名
fn dir_hash(dir: &Path) -> u64 {
    let text = dir.to_string_lossy().replace('/', r"\").to_lowercase();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    text.trim_end_matches('\\')
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325, |hash, b| {
            (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
}

pub fn token_path(data_dir: &Path) -> PathBuf {
    data_dir.join("core.token")
}

/// 32 字节随机数的十六进制
pub fn new_token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub fn read_token(data_dir: &Path) -> io::Result<String> {
    Ok(std::fs::read_to_string(token_path(data_dir))?
        .trim()
        .to_string())
}

/// 常数时间比较，耗时不随第一个不同字节的位置变化，避免按时间逐字节猜出令牌
fn token_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// 读取一行（去掉行尾换行）；对端关闭时返回 `None`，超过 `max` 字节或不是 UTF-8 时返回错误
pub async fn read_line<R: AsyncBufRead + Unpin>(
    r: &mut R,
    buf: &mut Vec<u8>,
    max: usize,
) -> io::Result<Option<String>> {
    buf.clear();
    let n = (&mut *r)
        .take(max as u64 + 1)
        .read_until(b'\n', buf)
        .await?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
    } else if buf.len() > max {
        return Err(io::Error::new(ErrorKind::InvalidData, "消息超过长度上限"));
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    String::from_utf8(std::mem::take(buf))
        .map(Some)
        .map_err(|err| io::Error::new(ErrorKind::InvalidData, err))
}

pub async fn write_line<W: AsyncWrite + Unpin>(w: &mut W, line: &str) -> io::Result<()> {
    w.write_all(format!("{line}\n").as_bytes()).await?;
    w.flush().await
}

/// 测试用的临时目录。释放时删除失败（后台线程刚放开数据库、杀毒软件一时还占着新写的文件）就重试几秒，
/// 不在 `%TEMP%` 留下目录；`tempfile::TempDir` 删除失败时不报错也不重试
#[cfg(test)]
pub(crate) struct TestDir(Option<tempfile::TempDir>);

#[cfg(test)]
impl TestDir {
    pub(crate) fn path(&self) -> &Path {
        self.0.as_ref().expect("释放之前一直有目录").path()
    }
}

#[cfg(test)]
impl Drop for TestDir {
    fn drop(&mut self) {
        let Some(dir) = self.0.take() else {
            return;
        };
        let path = dir.keep();
        for _ in 0..50 {
            if std::fs::remove_dir_all(&path).is_ok() || !path.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        eprintln!("无法删除测试临时目录 {}", path.display());
    }
}

/// 测试用的扫描上限：参数表的默认上限，所有类型都开启
#[cfg(test)]
pub(crate) const TEST_LIMITS: rummage_index::ScanLimits = rummage_index::ScanLimits {
    max_doc_mb: 200,
    max_image_mb: 100,
    max_audio_min: 240,
    max_image_mp: 100,
    documents: true,
    images: true,
    audio: true,
    video: true,
};

#[cfg(test)]
pub(crate) fn test_dir() -> TestDir {
    TestDir(Some(tempfile::tempdir().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_compare() {
        let token = new_token().unwrap();
        assert_eq!(token.len(), 64);
        assert!(token_eq(&token, &token.clone()));
        assert!(!token_eq(&token, &new_token().unwrap()));
        assert!(!token_eq(&token, &token[..63]));
        assert!(!token_eq(&token, ""));
        assert!(token_eq("", ""));
    }

    #[tokio::test]
    async fn read_line_limits() {
        let mut buf = Vec::new();
        let mut input: &[u8] = b"a\r\nb\nc";
        assert_eq!(
            read_line(&mut input, &mut buf, MAX_LINE)
                .await
                .unwrap()
                .as_deref(),
            Some("a")
        );
        assert_eq!(
            read_line(&mut input, &mut buf, MAX_LINE)
                .await
                .unwrap()
                .as_deref(),
            Some("b")
        );
        assert_eq!(
            read_line(&mut input, &mut buf, MAX_LINE)
                .await
                .unwrap()
                .as_deref(),
            Some("c")
        );
        assert_eq!(
            read_line(&mut input, &mut buf, MAX_LINE).await.unwrap(),
            None
        );

        let long = vec![b'x'; MAX_LINE + 1];
        let mut input: &[u8] = &long;
        assert!(read_line(&mut input, &mut buf, MAX_LINE).await.is_err());
        let mut input: &[u8] = b"12345\n";
        assert!(read_line(&mut input, &mut buf, 4).await.is_err());
    }

    #[test]
    fn dir_hash_normalizes() {
        let h = dir_hash(Path::new(r"D:\Data\Rummage"));
        assert_eq!(h, dir_hash(Path::new(r"d:\data\rummage")));
        assert_eq!(h, dir_hash(Path::new("D:/Data/Rummage/")));
        assert_eq!(h, dir_hash(Path::new(r"D:\DATA\Rummage\\")));
        assert_eq!(h, dir_hash(Path::new(r"\\?\D:\Data\Rummage")));
        assert_ne!(h, dir_hash(Path::new(r"D:\Data\Rummage2")));
        // FNV-1a 64 位的标准测试向量
        assert_eq!(dir_hash(Path::new("")), 0xcbf2_9ce4_8422_2325);
        assert_eq!(dir_hash(Path::new("a")), 0xaf63_dc4c_8601_ec8c);
    }
}

/// 起真实的命名管道服务端，管道名带随机后缀，不与正在运行的核心冲突
#[cfg(all(test, windows))]
mod pipe_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use rummage_proto::{
        ApiError, ClientMsg, CoreMsg, Event, Hello, PROTOCOL_VERSION, ParamValue, Reply, Request,
        Welcome, decode, encode,
    };
    use tokio::io::{BufReader, ReadHalf, WriteHalf};

    use rummage_index::{Db, DbError};

    use super::*;
    use crate::client::{Client, ConnectError};

    const TOKEN: &str = "test-token";

    async fn start(db: Result<Db, DbError>) -> (String, Arc<State>) {
        let name = format!(r"\\.\pipe\rummage-core-test-{}", new_token().unwrap());
        let listener = bind(&name).await.unwrap();
        let state = State::new(TOKEN.into(), db);
        tokio::spawn(serve(listener, state.clone()));
        (name, state)
    }

    /// 不需要数据库的测试用，不在临时目录中留下打开着的数据库
    fn no_db() -> Result<Db, DbError> {
        Err(DbError::Sqlite)
    }

    /// 需要数据库的测试：手动建立运行时，先释放运行时（连同服务任务持有的数据库连接），再删除临时目录
    fn with_db<F: Future<Output = ()>>(test: impl FnOnce(Result<Db, DbError>) -> F) {
        let dir = tempfile::tempdir().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(test(Db::open(dir.path())));
        drop(rt);
        dir.close().unwrap();
    }

    async fn raw(name: &str) -> (BufReader<ReadHalf<Stream>>, WriteHalf<Stream>) {
        let (rd, wr) = tokio::io::split(platform::open(name).await.unwrap());
        (BufReader::new(rd), wr)
    }

    async fn recv(client: &mut Client) -> CoreMsg {
        let mut buf = Vec::new();
        let line = tokio::time::timeout(
            Duration::from_secs(5),
            read_line(&mut client.reader, &mut buf, MAX_LINE),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        decode(&line).unwrap()
    }

    async fn request(client: &mut Client, id: u64, body: Request) -> Result<Reply, ApiError> {
        write_line(
            &mut client.writer,
            &encode(&ClientMsg::Request { id, body }),
        )
        .await
        .unwrap();
        loop {
            if let CoreMsg::Response { id: got, result } = recv(client).await
                && got == id
            {
                return result;
            }
        }
    }

    /// 同一连接上先发修改、不等回复紧接着发只读：读到的是修改后的状态
    #[test]
    fn read_after_write_on_one_connection() {
        with_db(|db| async {
            let (name, state) = start(db).await;
            let mut client = client::connect(&name, TOKEN).await.unwrap();
            // 条数不超过 `history_limit` 的默认值 10
            let queries: Vec<String> = (0..8).map(|i| format!("查询{i}")).collect();
            for (i, q) in queries.iter().enumerate() {
                let body = Request::AddHistory { query: q.clone() };
                let msg = encode(&ClientMsg::Request {
                    id: i as u64 + 1,
                    body,
                });
                write_line(&mut client.writer, &msg).await.unwrap();
            }
            let Ok(Reply::History { queries: got }) =
                request(&mut client, 100, Request::GetHistory).await
            else {
                panic!("应回复搜索历史");
            };
            for q in &queries {
                assert!(got.contains(q), "{q} 不在 {got:?} 中");
            }
            drop(client);
            drop(state);
        });
    }

    #[tokio::test]
    async fn handshake_and_ping() {
        let (name, _) = start(no_db()).await;
        let mut client = client::connect(&name, TOKEN).await.unwrap();
        assert_eq!(client.welcome.protocol, PROTOCOL_VERSION);
        assert_eq!(client.welcome.core_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(
            request(&mut client, 1, Request::Ping).await,
            Ok(Reply::Pong)
        );
        // 数据库不可用时 Ping 照常，需要数据库的请求返回 unavailable
        let err = request(&mut client, 2, Request::GetParams)
            .await
            .unwrap_err();
        assert_eq!(err.code, ApiError::UNAVAILABLE);
    }

    #[tokio::test]
    async fn wrong_token_rejected() {
        let (name, _) = start(no_db()).await;
        match client::connect(&name, "wrong").await {
            Err(ConnectError::Rejected(err)) => assert_eq!(err.code, ApiError::UNAUTHORIZED),
            other => panic!("应被拒绝：{:?}", other.map(|c| c.welcome)),
        }
    }

    #[tokio::test]
    async fn protocol_mismatch_rejected() {
        let (name, _) = start(no_db()).await;
        let (mut rd, mut wr) = raw(&name).await;
        let hello = Hello {
            protocol: PROTOCOL_VERSION + 1,
            token: TOKEN.into(),
        };
        write_line(&mut wr, &encode(&hello)).await.unwrap();
        let mut buf = Vec::new();
        let line = read_line(&mut rd, &mut buf, MAX_LINE)
            .await
            .unwrap()
            .unwrap();
        let reply: Result<Welcome, ApiError> = decode(&line).unwrap();
        assert_eq!(reply.unwrap_err().code, ApiError::PROTOCOL_MISMATCH);
        // 回复后断开
        assert!(!matches!(
            read_line(&mut rd, &mut buf, MAX_LINE).await,
            Ok(Some(_))
        ));
    }

    #[tokio::test]
    async fn oversized_line_disconnects() {
        let (name, _) = start(no_db()).await;
        let client = client::connect(&name, TOKEN).await.unwrap();
        let mut reader = client.reader;
        let mut writer = client.writer;
        // 服务端读满上限后就断开，写入可能中途失败，忽略写入结果
        tokio::spawn(async move {
            let _ = writer.write_all(&vec![b'a'; MAX_LINE + 16]).await;
            let _ = writer.write_all(b"\n").await;
        });
        let mut buf = Vec::new();
        let res = tokio::time::timeout(
            Duration::from_secs(10),
            read_line(&mut reader, &mut buf, MAX_LINE),
        )
        .await
        .unwrap();
        assert!(!matches!(res, Ok(Some(_))));
    }

    #[test]
    fn set_param_out_of_range_rejected() {
        with_db(|db| async {
            let (name, state) = start(db).await;
            let mut client = client::connect(&name, TOKEN).await.unwrap();
            let body = Request::SetParam {
                key: "result_limit".into(),
                value: ParamValue::Int(100_000),
            };
            let err = request(&mut client, 1, body).await.unwrap_err();
            assert_eq!(err.code, ApiError::BAD_REQUEST);
            assert_eq!(
                state.params()["result_limit"],
                rummage_proto::params::defaults()["result_limit"]
            );
        });
    }

    #[test]
    fn set_param_broadcasts() {
        with_db(set_param_broadcasts_inner);
    }

    async fn set_param_broadcasts_inner(db: Result<Db, DbError>) {
        let (name, _) = start(db).await;
        let mut a = client::connect(&name, TOKEN).await.unwrap();
        let mut b = client::connect(&name, TOKEN).await.unwrap();
        // 确认 b 的连接已进入请求阶段（已订阅事件），再由 a 修改
        assert_eq!(request(&mut b, 1, Request::Ping).await, Ok(Reply::Pong));
        let body = Request::SetParam {
            key: "weight_name".into(),
            value: ParamValue::Int(2),
        };
        assert_eq!(request(&mut a, 1, body).await, Ok(Reply::Done));
        let CoreMsg::Event {
            body: Event::ParamsChanged { values },
        } = recv(&mut b).await
        else {
            panic!("应收到参数变更事件");
        };
        // 浮点参数收到整数时保存规范化后的值
        assert_eq!(values["weight_name"], ParamValue::Float(2.0));
    }

    #[tokio::test]
    async fn verifies_own_server() {
        let (name, _) = start(no_db()).await;
        let pipe = platform::open(&name).await.unwrap();
        assert_eq!(platform::verify_server(&pipe).unwrap(), std::process::id());
    }

    #[tokio::test]
    async fn second_bind_already_running() {
        let (name, _) = start(no_db()).await;
        assert!(matches!(bind(&name).await, Err(BindError::AlreadyRunning)));
    }

    #[tokio::test]
    async fn handshake_timeout_disconnects() {
        let (name, _) = start(no_db()).await;
        let (mut rd, _wr) = raw(&name).await;
        let mut buf = Vec::new();
        let res = tokio::time::timeout(
            HANDSHAKE_TIMEOUT + Duration::from_secs(3),
            read_line(&mut rd, &mut buf, MAX_LINE),
        )
        .await
        .expect("服务端应在握手超时后断开");
        assert!(!matches!(res, Ok(Some(_))));
    }

    #[tokio::test]
    async fn oversized_hello_disconnects() {
        let (name, _) = start(no_db()).await;
        let (mut rd, mut wr) = raw(&name).await;
        let hello = Hello {
            protocol: PROTOCOL_VERSION,
            token: "x".repeat(MAX_HELLO),
        };
        let _ = write_line(&mut wr, &encode(&hello)).await;
        let mut buf = Vec::new();
        let res = tokio::time::timeout(
            Duration::from_secs(3),
            read_line(&mut rd, &mut buf, MAX_LINE),
        )
        .await
        .unwrap();
        assert!(!matches!(res, Ok(Some(_))));
    }

    #[tokio::test]
    async fn bad_request_keeps_connection() {
        let (name, _) = start(no_db()).await;
        let mut client = client::connect(&name, TOKEN).await.unwrap();
        write_line(
            &mut client.writer,
            r#"{"type":"Request","id":5,"body":{"type":"Nope"}}"#,
        )
        .await
        .unwrap();
        let CoreMsg::Response { id: 5, result } = recv(&mut client).await else {
            panic!("应回复 id 5");
        };
        assert_eq!(result.unwrap_err().code, ApiError::BAD_REQUEST);
        assert_eq!(
            request(&mut client, 6, Request::Ping).await,
            Ok(Reply::Pong)
        );

        // 读不出 id 时断开
        write_line(&mut client.writer, "not json").await.unwrap();
        let mut buf = Vec::new();
        let res = tokio::time::timeout(
            Duration::from_secs(3),
            read_line(&mut client.reader, &mut buf, MAX_LINE),
        )
        .await
        .unwrap();
        assert!(!matches!(res, Ok(Some(_))));
    }

    #[tokio::test]
    async fn shutdown_request_notifies() {
        let (name, state) = start(no_db()).await;
        let mut client = client::connect(&name, TOKEN).await.unwrap();
        assert_eq!(
            request(&mut client, 1, Request::Shutdown).await,
            Ok(Reply::Done)
        );
        tokio::time::timeout(Duration::from_secs(3), state.shutdown_requested())
            .await
            .expect("应通知主循环退出");
    }
}
