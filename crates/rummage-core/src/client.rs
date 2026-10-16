//! 客户端连接与握手：界面进程与命令行打开核心的管道，先核对服务端进程属于当前用户，
//! 再发出带令牌的 `Hello`，收到 `Welcome` 后由调用方自行收发 `ClientMsg` / `CoreMsg`。整个过程限时 5 秒。
//! `connect_local`：后台服务在本进程中时（Android）不经管道，用一对内存流连接同一个 `State`，握手与请求照常
//! `connect_or_start`（界面与命令行共用）：先连接；核心未运行时拉起
//! `rummage-core --data-dir <目录>`（不显示窗口、不继承调用方的句柄、尝试脱离调用方的作业对象，见 `spawn`），
//! 本进程拉起的还没退出时不重复拉起，再每 100 毫秒重试、最多等 10 秒。拉起的核心以 `EXIT_ALREADY_RUNNING` 退出时
//! （几个调用方同时拉起，别人的核心先占住了管道）照常重试到 10 秒期满，以其他退出码退出时立即返回；核心版本与本程序不同（例如更新后旧核心仍在常驻）
//! 时请它退出、等进程结束后拉起新的。本进程拉起的核心异常退出时，把退出码和 `core.log` 最后 200 行写进本进程的日志
#[cfg(not(windows))]
#[path = "client/other.rs"]
mod spawn;
#[cfg(windows)]
#[path = "client/windows.rs"]
mod spawn;

use std::ffi::OsStr;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::Path;
use std::time::{Duration, Instant};

use std::sync::Arc;

use rummage_proto::{
    ApiError, ClientMsg, CoreMsg, Hello, PROTOCOL_VERSION, Reply, Request, Welcome, decode, encode,
};
use tokio::io::{AsyncRead, AsyncWrite, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::oneshot;
use tokio::sync::oneshot::error::TryRecvError;

use crate::{
    HANDSHAKE_TIMEOUT, MAX_HELLO, MAX_LINE, State, Stream, platform, read_line, write_line,
};

/// 内存流每个方向的缓冲区大小；写满后写入方等待对方读取
const LOCAL_BUFFER: usize = 64 * 1024;

pub struct Client<S = Stream> {
    pub reader: BufReader<ReadHalf<S>>,
    pub writer: WriteHalf<S>,
    pub welcome: Welcome,
    /// 核心进程的 PID，用于等它退出；本进程中的后台服务为本进程的 PID
    pub server_pid: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// 管道不存在
    #[error("Rummage 后台服务未运行。")]
    NotRunning,
    /// 管道的服务端不是当前用户的进程，或无法确认；此时不发出令牌
    #[error("无法确认 Rummage 后台服务属于当前用户，已拒绝连接。")]
    Untrusted,
    /// 握手被拒绝（令牌不对、协议版本不符）
    #[error("{}", .0.message)]
    Rejected(ApiError),
    #[error("无法连接 Rummage 后台服务：{0}")]
    Io(#[from] io::Error),
}

pub async fn connect(pipe: &str, token: &str) -> Result<Client, ConnectError> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(pipe, token))
        .await
        .unwrap_or_else(|_| Err(io::Error::new(ErrorKind::TimedOut, "连接超时。").into()))
}

/// 连接本进程中的后台服务：一端交给连接处理（与管道上的连接相同），另一端握手
pub async fn connect_local(state: &Arc<State>) -> Result<Client<DuplexStream>, ConnectError> {
    let (ours, theirs) = tokio::io::duplex(LOCAL_BUFFER);
    tokio::spawn(crate::server::handle(theirs, state.clone()));
    tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        hello(ours, state.token(), std::process::id()),
    )
    .await
    .unwrap_or_else(|_| Err(io::Error::new(ErrorKind::TimedOut, "连接超时。").into()))
}

async fn handshake(pipe: &str, token: &str) -> Result<Client, ConnectError> {
    let stream = platform::open(pipe).await.map_err(|err| match err.kind() {
        ErrorKind::NotFound => ConnectError::NotRunning,
        _ => ConnectError::Io(err),
    })?;
    let server_pid = platform::verify_server(&stream).map_err(|_| ConnectError::Untrusted)?;
    hello(stream, token, server_pid).await
}

/// 发出带令牌的 `Hello` 并等 `Welcome`
async fn hello<S: AsyncRead + AsyncWrite>(
    stream: S,
    token: &str,
    server_pid: u32,
) -> Result<Client<S>, ConnectError> {
    let (rd, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(rd);
    let hello = Hello {
        protocol: PROTOCOL_VERSION,
        token: token.into(),
    };
    write_line(&mut writer, &encode(&hello)).await?;
    let mut buf = Vec::new();
    let Some(line) = read_line(&mut reader, &mut buf, MAX_HELLO).await? else {
        return Err(io::Error::new(ErrorKind::UnexpectedEof, "连接被断开。").into());
    };
    match decode::<Result<Welcome, ApiError>>(&line)
        .map_err(|err| io::Error::new(ErrorKind::InvalidData, err))?
    {
        Ok(welcome) => Ok(Client {
            reader,
            writer,
            welcome,
            server_pid,
        }),
        Err(err) => Err(ConnectError::Rejected(err)),
    }
}

/// 发出一个请求并等到它的回复，期间收到的事件丢弃
pub async fn request<S: AsyncRead + AsyncWrite>(
    client: &mut Client<S>,
    id: u64,
    body: Request,
) -> io::Result<Result<Reply, ApiError>> {
    send(client, id, body).await?;
    reply(client, id).await
}

/// 只写出请求。出错时请求没有送到核心
pub async fn send<S: AsyncRead + AsyncWrite>(
    client: &mut Client<S>,
    id: u64,
    body: Request,
) -> io::Result<()> {
    write_line(
        &mut client.writer,
        &encode(&ClientMsg::Request { id, body }),
    )
    .await
}

/// 等请求 `id` 的回复，期间收到的事件丢弃。出错时请求已写出，核心可能已经处理
pub async fn reply<S: AsyncRead + AsyncWrite>(
    client: &mut Client<S>,
    id: u64,
) -> io::Result<Result<Reply, ApiError>> {
    let mut buf = Vec::new();
    loop {
        let Some(line) = read_line(&mut client.reader, &mut buf, MAX_LINE).await? else {
            return Err(io::Error::new(
                ErrorKind::UnexpectedEof,
                "Rummage 后台服务断开了连接。",
            ));
        };
        if let Ok(CoreMsg::Response { id: got, result }) = decode(&line)
            && got == id
        {
            return Ok(result);
        }
    }
}

/// 请核心退出并等它的进程结束（最多 10 秒），用于重新构建前和版本不一致时；返回进程是否已结束
pub async fn shutdown(mut client: Client) -> io::Result<bool> {
    let msg = ClientMsg::Request {
        id: 0,
        body: Request::Shutdown,
    };
    write_line(&mut client.writer, &encode(&msg)).await?;
    // 等到回复或核心断开；中间可能夹着事件
    let mut buf = Vec::new();
    while let Ok(Some(line)) = read_line(&mut client.reader, &mut buf, MAX_LINE).await {
        if let Ok(CoreMsg::Response { id: 0, .. }) = decode(&line) {
            break;
        }
    }
    drop(client.reader);
    drop(client.writer);
    let pid = client.server_pid;
    tokio::task::spawn_blocking(move || platform::wait_exit(pid, Duration::from_secs(10)))
        .await
        .map_err(io::Error::other)
}

/// 本进程拉起、尚未确认退出的核心：进程结束时收到退出码（无法取得时为 `u32::MAX`）
pub type Own = oneshot::Receiver<u32>;

/// core.log 的最后若干行
fn tail(path: &Path, lines: usize) -> String {
    match fs::read(path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            let all: Vec<&str> = text.lines().collect();
            all[all.len().saturating_sub(lines)..].join("\n")
        }
        Err(err) => format!("（无法读取 core.log：{err}）"),
    }
}

/// 拉起核心进程，返回它退出时的通知；异常退出时在通知前记录退出码和 core.log 尾部
fn start(exe: &Path, dir: &Path) -> io::Result<Own> {
    let proc = spawn::spawn_detached(exe, &[OsStr::new("--data-dir"), dir.as_os_str()]).map_err(
        |err| io::Error::new(err.kind(), format!("无法启动 \"{}\"：{err}", exe.display())),
    )?;
    let (tx, rx) = oneshot::channel();
    let log_path = dir.join("logs").join("core.log");
    std::thread::spawn(move || {
        let code = match proc.wait() {
            Ok(code) => code,
            Err(err) => {
                log::warn!("无法等待核心进程退出：{err}");
                u32::MAX
            }
        };
        // 已有核心在运行不算异常
        if code != 0 && code != u32::MAX && code != u32::from(crate::EXIT_ALREADY_RUNNING) {
            log::warn!(
                "核心进程异常退出，退出码 {code}（0x{code:08X}）。core.log 最后 200 行：\n{}",
                tail(&log_path, 200)
            );
        }
        let _ = tx.send(code);
    });
    Ok(rx)
}

async fn try_connect(pipe: &str, dir: &Path) -> Result<Client, ConnectError> {
    let token = crate::read_token(dir).map_err(|err| match err.kind() {
        // 没有令牌文件说明核心没在这个数据目录上运行过或已正常退出
        ErrorKind::NotFound => ConnectError::NotRunning,
        _ => ConnectError::Io(err),
    })?;
    connect(pipe, &token).await
}

/// 连接数据目录 `dir` 的核心，未运行时用 `exe` 拉起（见模块说明）。`own` 为本进程拉起、尚未确认退出的核心，
/// 跨多次调用保留（界面的守护逻辑）；只调用一次的传入 `&mut None`
pub async fn connect_or_start(
    dir: &Path,
    exe: &Path,
    own: &mut Option<Own>,
) -> Result<Client, ConnectError> {
    let pipe = crate::pipe_name(dir)?;
    match try_connect(&pipe, dir).await {
        Ok(client) if client.welcome.core_version != env!("CARGO_PKG_VERSION") => {
            log::warn!(
                "核心进程版本 {} 与本程序版本 {} 不一致，让它退出后重新拉起。",
                client.welcome.core_version,
                env!("CARGO_PKG_VERSION")
            );
            if !shutdown(client).await? {
                return Err(io::Error::new(
                    ErrorKind::TimedOut,
                    "旧版本的后台服务 10 秒内没有退出。",
                )
                .into());
            }
            if let Some(exit) = own.as_mut()
                && tokio::time::timeout(Duration::from_secs(3), exit)
                    .await
                    .is_ok()
            {
                *own = None;
            }
        }
        Err(ConnectError::NotRunning) => {}
        res => return res,
    }
    if own.is_none() {
        *own = Some(start(exe, dir)?);
    }
    let begin = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        // 管道出现到令牌写入之间有极短的间隔，期间可能读到上次的令牌而被拒绝，一并重试
        let res = try_connect(&pipe, dir).await;
        if res.is_ok() || begin.elapsed() >= Duration::from_secs(10) {
            return res;
        }
        let Some(exit) = own.as_mut() else {
            continue;
        };
        match exit.try_recv() {
            Err(TryRecvError::Empty) => {}
            // 别的调用方同时拉起的核心先占住了管道，它启动完就能连上
            Ok(code) if code == u32::from(crate::EXIT_ALREADY_RUNNING) => *own = None,
            _ => {
                *own = None;
                return res;
            }
        }
    }
}

/// 不经管道、各平台都能运行
#[cfg(test)]
mod local_tests {
    use rummage_index::DbError;

    use super::*;

    #[tokio::test]
    async fn connect_local_handshake_and_request() {
        let state = State::new("test-token".into(), Err(DbError::Sqlite));
        let mut client = connect_local(&state).await.unwrap();
        assert_eq!(client.welcome.protocol, PROTOCOL_VERSION);
        assert_eq!(client.server_pid, std::process::id());
        assert_eq!(
            request(&mut client, 1, Request::Ping).await.unwrap(),
            Ok(Reply::Pong)
        );
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn not_running_without_token() {
        let dir = crate::test_dir();
        let pipe = crate::pipe_name(dir.path()).unwrap();
        assert!(matches!(
            try_connect(&pipe, dir.path()).await,
            Err(ConnectError::NotRunning)
        ));
    }

    #[tokio::test]
    async fn start_fails_fast() {
        let dir = crate::test_dir();
        // 程序不存在：拉起失败，立即返回
        let missing = dir.path().join("missing.exe");
        let err = connect_or_start(dir.path(), &missing, &mut None)
            .await
            .err()
            .unwrap();
        assert!(err.to_string().contains("missing.exe"), "{err}");
        // 拉起的程序马上以其他退出码退出（whoami 不认识参数时为 1）、没有建立管道：不等满 10 秒
        let begin = Instant::now();
        let mut own = None;
        let res = connect_or_start(
            dir.path(),
            Path::new(r"C:\Windows\System32\whoami.exe"),
            &mut own,
        )
        .await;
        assert!(matches!(res, Err(ConnectError::NotRunning)));
        assert!(own.is_none());
        assert!(begin.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn already_running_keeps_waiting() {
        // 退出码为 2（已有核心在运行；where 不认识参数时同样为 2）：继续等别人的核心，直到 10 秒期满
        let dir = crate::test_dir();
        let begin = Instant::now();
        let mut own = None;
        let res = connect_or_start(
            dir.path(),
            Path::new(r"C:\Windows\System32\where.exe"),
            &mut own,
        )
        .await;
        assert!(matches!(res, Err(ConnectError::NotRunning)));
        assert!(own.is_none());
        assert!(begin.elapsed() >= Duration::from_secs(10));
    }

    #[test]
    fn tail_keeps_last_lines() {
        let dir = crate::test_dir();
        let log = dir.path().join("core.log");
        fs::write(&log, "a\nb\nc\n").unwrap();
        assert_eq!(tail(&log, 2), "b\nc");
        assert!(tail(&dir.path().join("none.log"), 2).contains("无法读取"));
    }
}
