//! Android 平台接口：没有推理子进程，推理在应用进程内的线程中运行（`spawn`），收发与子进程相同的帧：
//! 请求与回复各走一个 `std::io::pipe`，线程解析出 `WorkerArgs` 后运行 `run_worker`。
//! - 线程无法强制结束：`Job::kill` 关闭回复管道的写入端，等待回复的一方立即读到结尾（“推理进程关闭了输出”），
//!   同时置位中止标志，正在进行的加载或推理尽快停下（`rummage_llama::Embedder::new` 的 `abort`）；请求管道由调用方关闭，
//!   线程随后自行退出。到 `KILL_GRACE` 仍没有退出时与子进程一样按结束不了处理。
//! - 没有作业对象和内存上限；没有单独的 stderr（llama.cpp 的日志交给应用日志），`spawn` 给出的 stderr 为空。
//! - 运行库在进程中只加载一次（`Runtime::load_once`），之后启动的推理线程共用：重复加载会重复注册 ggml 后端。
//! - 只用 CPU：没有 Vulkan 运行库，不设置环境变量；自检用的驱动版本、驱动事件与显卡列表都为空。
//! - `cpu_name`：读系统属性 `ro.soc.manufacturer` 与 `ro.soc.model`（Android 12 起有），厂商代号换成中文名。

use std::ffi::{CStr, OsString};
use std::fs::File;
use std::io::{self, ErrorKind, PipeReader, PipeWriter, Write};
use std::os::fd::AsFd;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rummage_llama::Runtime;

use crate::{Error, WorkerArgs};

/// 推理线程的“进程号”，只用来区分先后启动的推理线程
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 回复管道的写入端（与推理线程共用，取走即关闭）与中止标志
pub struct Job {
    out: Arc<Mutex<Option<PipeWriter>>>,
    abort: Arc<AtomicBool>,
}

impl Job {
    pub fn new(_mem_bytes: u64) -> io::Result<Self> {
        Ok(Job {
            out: Arc::default(),
            abort: Arc::default(),
        })
    }

    /// 关闭回复管道的写入端并置位中止标志，见模块说明
    pub fn kill(&self) {
        lock(&self.out).take();
        self.abort.store(true, Ordering::Relaxed);
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

/// 运行推理的线程
pub struct Child {
    id: u32,
    thread: Option<JoinHandle<i32>>,
    status: Option<ExitStatus>,
}

impl Child {
    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(t) = self.thread.take_if(|t| t.is_finished()) {
            // panic 时记为 101，与 Rust 程序 panic 退出时的状态码相同
            let code = t.join().unwrap_or(101);
            self.status = Some(ExitStatus::from_raw((code & 0xff) << 8));
        }
        Ok(self.status)
    }

    /// 线程无法强制结束，见模块说明
    pub fn kill(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub type ChildStdin = PipeWriter;

/// 在本进程中启动推理线程；`args` 与推理子进程的命令行参数相同
pub fn spawn(
    _exe: &Path,
    args: Vec<OsString>,
    job: &Job,
) -> Result<(Child, PipeWriter, PipeReader, io::Empty), Error> {
    // 第一个参数是子命令名 `worker`
    let args = WorkerArgs::parse(args.into_iter().skip(1)).map_err(Error::Spawn)?;
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
    let child = Child {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        thread: Some(thread),
        status: None,
    };
    Ok((child, req, reply, io::empty()))
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

/// SoC 厂商与型号，如“高通 SM8550”；型号读不到时为空。
pub fn cpu_name() -> Option<String> {
    let model = system_property(c"ro.soc.model");
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
