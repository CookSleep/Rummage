//! Android 平台接口：推理（LiteRT）收发与子进程相同的帧，按后端分两种方式运行（`spawn`）：
//! - CPU：应用进程内的线程。请求与回复各走一个 `std::io::pipe`，线程解析出 `WorkerArgs` 后运行 `run_worker`。
//!   线程无法强制结束：`Job::kill` 关闭回复管道的写入端，等待回复的一方立即读到结尾（“推理进程关闭了输出”），
//!   同时置位中止标志；请求管道由调用方关闭，线程随后自行退出。到 `KILL_GRACE` 仍没有退出时与子进程一样按结束不了处理。
//! - GPU、NPU（`--litert gpu|npu`）：未经检测的加速器只能在可强行结束的进程中初始化，放在应用的 `:embed` 进程（Kotlin 的
//!   绑定服务 `EmbedService`）中。不从 nativeLibraryDir 执行子进程：Android 12 起有全局 32 个的“幽灵进程”上限，后台高 CPU 的子进程会被杀。
//!   1. 本进程在抽象命名空间监听一个只用一次的 Unix 套接字（名字带本进程号与序号），经应用登记的启动方法（`set_launcher`，
//!      JNI 在应用中）绑定服务，把套接字名与参数交给它；
//!   2. 服务进程连上套接字，先发 4 字节小端的进程号，再在同一连接上运行 `run_worker`（JNI 入口在应用中），结束后整个进程退出；
//!   3. 本进程只接受同一用户（`SO_PEERCRED` 的 uid 与本进程相同）的连接，`ACCEPT_TIMEOUT` 内没有连上就报启动失败。
//!   4. `Job::kill` 先解除绑定（否则系统会重新拉起服务），再 `SIGKILL`；进程是否还在用 `kill(pid, 0)` 判断。
//!   5. 同一个服务的所有绑定都在同一个进程中：换设备时旧引擎的推理进程还没结束、新的就已启动，结束旧的会连新的一起结束。
//!      所以有 `SLOTS` 个服务（`:embed`、`:embed2`），新的推理进程用一个没有推理在运行的（`SLOT_PIDS`），都在用时等它们结束。
//!   6. 推理进程自己退出时按应用的退出记录（`ApplicationExitInfo`）区分是被系统结束（内存不足、后台冻结等）还是出错
//!      （`killed_by_system`）：前者由上层重新拉起，后者按后端失效处理。
//! - 没有作业对象和内存上限；没有单独的 stderr，`spawn` 给出的 stderr 为空。
//! - NPU：高通的 QNN 库按文件名加载 HTP 的 Stub，而应用只在 nativeLibraryDir 中按文件名查找；推理进程在创建引擎前按完整路径
//!   预加载分发库目录中的 Stub（`preload_npu`），之后按文件名加载时复用已加载的库。
//! - 运行库在进程中只加载一次（`Runtime::load_once`）：llama.cpp 只在电脑上用，这里保留以便编译。
//! - 自检用的驱动版本、驱动事件与显卡列表都为空。
//! - `cpu_name`：读系统属性 `ro.soc.manufacturer` 与 `ro.soc.model`，厂商代号换成中文名。

use std::ffi::{CStr, CString, OsString};
use std::fs::{self, File};
use std::io::{self, ErrorKind, PipeWriter, Read, Write};
use std::os::android::net::SocketAddrExt;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rummage_llama::Runtime;

use crate::{Error, WorkerArgs};

/// 推理线程的“进程号”，只用来区分先后启动的推理线程；也用作 `:embed` 进程套接字名中的序号
static NEXT_ID: AtomicU32 = AtomicU32::new(1);
/// 等 `:embed` 进程连上套接字的时限：包括系统创建进程与加载应用的原生库
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(30);
const ACCEPT_POLL: Duration = Duration::from_millis(20);
/// `:embed` 进程的个数（见模块说明），与插件清单中的 `EmbedService`、`EmbedService2` 对应
const SLOTS: usize = 2;
/// 各个 `:embed` 进程中运行的推理：启动中为 `Some(None)`，连上后为进程号；进程已不在时可以再用
static SLOT_PIDS: Mutex<[Option<Option<u32>>; SLOTS]> = Mutex::new([None; SLOTS]);
/// 推理进程退出后，系统写入退出记录可能稍晚：最多等这么久
const EXIT_INFO_WAIT: Duration = Duration::from_secs(2);

/// 在 `:embed` 进程中启动与结束推理的方法（见模块说明），由应用在启动时登记
pub struct Launcher {
    /// 绑定第 `slot` 个服务（见模块说明），交给它套接字名与推理进程的参数（含子命令名 `worker`）
    pub start: fn(socket: &str, args: &[String], slot: usize) -> io::Result<()>,
    /// 解除绑定
    pub stop: fn(socket: &str),
    /// 本应用这个进程号在 `since`（Unix 毫秒）之后的退出记录：被系统结束（内存不足、冻结等）时为 `Some(true)`，
    /// 崩溃或自己退出时为 `Some(false)`，还没有记录时为 `None`；早于 `since` 的是复用了同一进程号的旧进程的
    pub killed: fn(pid: u32, since: i64) -> Option<bool>,
}

static LAUNCHER: OnceLock<Launcher> = OnceLock::new();

/// 登记 `:embed` 进程的启动方法；只在第一次调用时生效
pub fn set_launcher(l: Launcher) {
    let _ = LAUNCHER.set(l);
}

/// 在 `:embed` 进程中：连上 `socket`，发出本进程号后在这条连接上运行推理（见模块说明），返回退出码
pub fn serve_embed_process(socket: &str, args: Vec<String>) -> i32 {
    let res = SocketAddr::from_abstract_name(socket.as_bytes())
        .and_then(|a| UnixStream::connect_addr(&a));
    let mut out = match res {
        Ok(s) => s,
        Err(err) => {
            log::error!("推理进程连不上 {socket}：{err}");
            return 1;
        }
    };
    let args = match crate::WorkerArgs::parse(args.into_iter().skip(1).map(OsString::from)) {
        Ok(a) => a,
        Err(msg) => {
            log::error!("推理进程的参数无效：{msg}");
            return 1;
        }
    };
    let mut inp = match out.try_clone() {
        Ok(s) => s,
        Err(err) => {
            log::error!("推理进程无法复制连接：{err}");
            return 1;
        }
    };
    if out.write_all(&std::process::id().to_le_bytes()).is_err() {
        return 1;
    }
    crate::run_worker(args, None, &mut inp, &mut out)
}

/// 推理进程（`:embed` 中的）还在运行
fn alive(pid: u32) -> bool {
    // SAFETY: 信号 0 只检查进程是否存在与是否有权限，不发送信号
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// 解除绑定并强制结束 `:embed` 中的推理进程
fn kill_process(socket: &str, pid: u32) {
    if let Some(l) = LAUNCHER.get() {
        (l.stop)(socket);
    }
    // SAFETY: 只向这个推理进程发信号；pid 是它自己报告的，属于本应用（同一 uid）
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 推理线程：回复管道的写入端（与推理线程共用，取走即关闭）与中止标志；`:embed` 中的推理进程：套接字名与进程号
pub struct Job {
    out: Arc<Mutex<Option<PipeWriter>>>,
    abort: Arc<AtomicBool>,
    process: Mutex<Option<(String, u32)>>,
}

impl Job {
    pub fn new(_mem_bytes: u64) -> io::Result<Self> {
        Ok(Job {
            out: Arc::default(),
            abort: Arc::default(),
            process: Mutex::new(None),
        })
    }

    /// 推理线程：关闭回复管道的写入端并置位中止标志；推理进程：解除绑定并强制结束（见模块说明）
    pub fn kill(&self) {
        lock(&self.out).take();
        self.abort.store(true, Ordering::Relaxed);
        if let Some((socket, pid)) = lock(&self.process).as_ref() {
            kill_process(socket, *pid);
        }
    }
}

/// 推理线程写回复用；线程结束（含 panic）时关闭写入端，读取方随即读到结尾
struct Out(Arc<Mutex<Option<PipeWriter>>>);

impl Write for Out {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match lock(&self.0).as_mut() {
            Some(w) => w.write(buf),
            None => Err(ErrorKind::BrokenPipe.into()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match lock(&self.0).as_mut() {
            Some(w) => w.flush(),
            None => Err(ErrorKind::BrokenPipe.into()),
        }
    }
}

impl Drop for Out {
    fn drop(&mut self) {
        lock(&self.0).take();
    }
}

/// 运行推理的线程，或 `:embed` 中的推理进程
pub enum Child {
    Thread {
        id: u32,
        thread: Option<JoinHandle<i32>>,
        status: Option<ExitStatus>,
    },
    Process {
        pid: u32,
        socket: String,
        /// 连上的时间（Unix 毫秒），用来排除复用了同一进程号的旧进程的退出记录
        since: i64,
    },
}

impl Child {
    pub fn id(&self) -> u32 {
        match self {
            Child::Thread { id, .. } => *id,
            Child::Process { pid, .. } => *pid,
        }
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match self {
            Child::Thread { thread, status, .. } => {
                if let Some(t) = thread.take_if(|t| t.is_finished()) {
                    // panic 时记为 101，与 Rust 程序 panic 退出时的状态码相同
                    let code = t.join().unwrap_or(101);
                    *status = Some(ExitStatus::from_raw((code & 0xff) << 8));
                }
                Ok(*status)
            }
            // 不是本进程的子进程，取不到退出码；不在了就记为被 SIGKILL 结束
            Child::Process { pid, .. } => Ok((!alive(*pid)).then(|| ExitStatus::from_raw(9))),
        }
    }

    /// 线程无法强制结束（见模块说明）；推理进程解除绑定并 `SIGKILL`
    pub fn kill(&mut self) -> io::Result<()> {
        if let Child::Process { pid, socket, .. } = self {
            kill_process(socket, *pid);
        }
        Ok(())
    }
}

pub type ChildStdin = Box<dyn Write + Send>;

/// 已退出的推理进程是被系统结束的（见模块说明）；推理线程，以及由 `Job::kill` 结束的（自检超时、取消等）总是假
pub fn killed_by_system(child: &Child, job: &Job) -> bool {
    let (Child::Process { pid, since, .. }, Some(l)) = (child, LAUNCHER.get()) else {
        return false;
    };
    if job.abort.load(Ordering::Relaxed) {
        return false;
    }
    let started = Instant::now();
    loop {
        if let Some(k) = (l.killed)(*pid, *since) {
            return k;
        }
        if started.elapsed() > EXIT_INFO_WAIT {
            return false;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// 启动推理：CPU 在本进程的线程中，GPU、NPU 在 `:embed` 进程中（见模块说明）；`args` 与推理子进程的命令行参数相同
#[allow(clippy::type_complexity)]
pub fn spawn(
    _exe: &Path,
    args: Vec<OsString>,
    job: &Job,
) -> Result<
    (
        Child,
        ChildStdin,
        Box<dyn Read + Send>,
        Box<dyn Read + Send>,
    ),
    Error,
> {
    // 第一个参数是子命令名 `worker`
    let parsed = WorkerArgs::parse(args.iter().skip(1).cloned()).map_err(Error::Spawn)?;
    let accel = matches!(
        &parsed,
        WorkerArgs::Embed { litert: Some(l), .. } if l.accel != crate::Accel::Cpu
    );
    if accel {
        return spawn_process(args, job);
    }
    let args = parsed;
    let pipe = || io::pipe().map_err(|err| Error::Spawn(format!("无法创建管道：{err}")));
    let (mut inp, req) = pipe()?;
    let (reply, out) = pipe()?;
    *lock(&job.out) = Some(out);
    let mut out = Out(job.out.clone());
    let abort = job.abort.clone();
    let thread = thread::Builder::new()
        .name("embed-worker".to_string())
        .spawn(move || crate::run_worker(args, Some(abort), &mut inp, &mut out))
        .map_err(|err| Error::Spawn(format!("无法启动推理线程：{err}")))?;
    let child = Child::Thread {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        thread: Some(thread),
        status: None,
    };
    Ok((child, Box::new(req), Box::new(reply), Box::new(io::empty())))
}

/// 在 `:embed` 进程中启动推理，等它连上套接字并报告进程号（见模块说明）
#[allow(clippy::type_complexity)]
fn spawn_process(
    args: Vec<OsString>,
    job: &Job,
) -> Result<
    (
        Child,
        ChildStdin,
        Box<dyn Read + Send>,
        Box<dyn Read + Send>,
    ),
    Error,
> {
    let fail = |what: &str, err: io::Error| Error::Spawn(format!("{what}：{err}"));
    let launcher = LAUNCHER
        .get()
        .ok_or_else(|| Error::Spawn("应用没有登记推理进程的启动方法。".to_string()))?;
    let socket = format!(
        "rummage-embed-{}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    );
    let addr = SocketAddr::from_abstract_name(socket.as_bytes())
        .map_err(|err| fail("无法创建推理进程的套接字", err))?;
    let listener =
        UnixListener::bind_addr(&addr).map_err(|err| fail("无法创建推理进程的套接字", err))?;
    listener
        .set_nonblocking(true)
        .map_err(|err| fail("无法设置推理进程的套接字", err))?;
    let args: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let slot = take_slot()?;
    let release = || lock(&SLOT_PIDS)[slot] = None;
    if let Err(err) = (launcher.start)(&socket, &args, slot) {
        release();
        return Err(fail("无法启动推理进程", err));
    }
    let started = Instant::now();
    let stream = loop {
        match listener.accept() {
            Ok((s, _)) if same_user(&s) => break s,
            Ok(_) => log::warn!("推理进程的套接字收到其他用户的连接，已拒绝。"),
            Err(err) if err.kind() == ErrorKind::WouldBlock => {}
            Err(err) => {
                (launcher.stop)(&socket);
                release();
                return Err(fail("推理进程没能连上", err));
            }
        }
        if started.elapsed() > ACCEPT_TIMEOUT {
            (launcher.stop)(&socket);
            release();
            return Err(Error::Spawn(format!(
                "推理进程在 {} 秒内没有启动。",
                ACCEPT_TIMEOUT.as_secs()
            )));
        }
        thread::sleep(ACCEPT_POLL);
    };
    let setup = || -> io::Result<(u32, UnixStream)> {
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(ACCEPT_TIMEOUT))?;
        let mut pid = [0u8; 4];
        (&stream).read_exact(&mut pid)?;
        stream.set_read_timeout(None)?;
        Ok((u32::from_le_bytes(pid), stream.try_clone()?))
    };
    let (pid, reader) = match setup() {
        Ok(x) => x,
        Err(err) => {
            (launcher.stop)(&socket);
            release();
            return Err(fail("推理进程没有报告进程号", err));
        }
    };
    lock(&SLOT_PIDS)[slot] = Some(Some(pid));
    *lock(&job.process) = Some((socket.clone(), pid));
    Ok((
        Child::Process {
            pid,
            socket,
            since: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as i64),
        },
        Box::new(stream),
        Box::new(reader),
        Box::new(io::empty()),
    ))
}

/// 占用一个没有推理在运行的 `:embed` 进程（见模块说明）；都在用时最多等 `ACCEPT_TIMEOUT`
fn take_slot() -> Result<usize, Error> {
    let started = Instant::now();
    loop {
        {
            let mut slots = lock(&SLOT_PIDS);
            let free = slots.iter().position(|s| match s {
                None => true,
                Some(Some(pid)) => !alive(*pid),
                Some(None) => false,
            });
            if let Some(i) = free {
                slots[i] = Some(None);
                return Ok(i);
            }
        }
        if started.elapsed() > ACCEPT_TIMEOUT {
            return Err(Error::Spawn(format!(
                "已有 {SLOTS} 个推理进程在运行，{} 秒内没有结束。",
                ACCEPT_TIMEOUT.as_secs()
            )));
        }
        thread::sleep(ACCEPT_POLL);
    }
}

/// 连接的另一端与本进程是同一个用户（应用自己的 `:embed` 进程）
fn same_user(s: &UnixStream) -> bool {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` 与 `len` 是有效的输出参数，`len` 为 `cred` 的大小；套接字在调用期间有效
    let ok = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    } == 0;
    // SAFETY: 无参数
    ok && cred.uid == unsafe { libc::getuid() }
}

/// 按完整路径预加载 `dir` 中的 QNN Stub（见模块说明）；加载失败只记日志，由创建引擎时报告
pub fn preload_npu(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.as_bytes();
        if !(name.starts_with(b"libQnnHtp") && name.ends_with(b"Stub.so")) {
            continue;
        }
        let Ok(path) = CString::new(e.path().as_os_str().as_bytes()) else {
            continue;
        };
        // SAFETY: 路径以 NUL 结尾；加载的是我们下载并校验过的 QNN 库，句柄不关闭，库留在进程中供 QNN 按文件名复用
        let h = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
        if h.is_null() {
            log::warn!("无法预加载 {}", e.path().display());
        }
    }
}

/// 进程内共用同一份运行库，见模块说明
pub fn load_runtime(dir: &Path) -> Result<Arc<Runtime>, rummage_llama::Error> {
    Runtime::load_once(dir)
}

/// 推理在应用进程内运行，不能改进程的环境变量；只用 CPU，用不到
pub fn set_runtime_env(_name: &CStr, _value: &CStr) -> io::Result<()> {
    Err(io::Error::new(
        ErrorKind::Unsupported,
        "Android 上不设置推理用的环境变量",
    ))
}

pub fn protocol_stdout() -> io::Result<File> {
    Ok(File::from(io::stdout().as_fd().try_clone_to_owned()?))
}

/// 系统属性的值；没有这一项时为空串
fn system_property(name: &CStr) -> String {
    let mut buf = [0u8; libc::PROP_VALUE_MAX as usize];
    // SAFETY: `name` 以 NUL 结尾；`buf` 有 PROP_VALUE_MAX 字节，`__system_property_get` 最多写入这么多（含结尾的 NUL）。
    let len = unsafe { libc::__system_property_get(name.as_ptr(), buf.as_mut_ptr().cast()) };
    let len = usize::try_from(len).unwrap_or(0).min(buf.len());
    String::from_utf8_lossy(&buf[..len]).trim().to_string()
}

/// 芯片型号 `ro.soc.model`（Android 12 起有），选 NPU 模型用；读不到时为空
pub fn soc_model() -> String {
    system_property(c"ro.soc.model")
}

/// SoC 厂商与型号，如“高通 SM8550”；型号读不到时为空。
pub fn cpu_name() -> Option<String> {
    let model = soc_model();
    if model.is_empty() {
        return None;
    }
    let vendor = match system_property(c"ro.soc.manufacturer").as_str() {
        "QTI" | "Qualcomm" => "高通".to_string(),
        "MediaTek" | "Mediatek" => "联发科".to_string(),
        "Samsung" => "三星".to_string(),
        v => v.to_string(),
    };
    if vendor.is_empty() {
        return Some(model);
    }
    Some(format!("{vendor} {model}"))
}

pub fn driver_version(_description: &str) -> Option<String> {
    None
}

pub fn adapters() -> Vec<crate::Adapter> {
    Vec::new()
}

pub fn driver_events(_window: Duration) -> io::Result<Vec<String>> {
    Ok(Vec::new())
}
