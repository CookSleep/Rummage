//! Windows 与 Android 以外平台的占位实现，只用于编译和测试：没有作业对象，`Job` 只记下子进程的 pid，`Job::kill` 用系统的
//! `kill -KILL` 结束它（检测超时与中止要在调度线程之外结束推理进程）；不限制内存；stdout 只复制、不重定向。
//! 自检用的驱动版本与驱动事件没有对应的接口：版本记为未知，事件为空，显卡列表为空；不读处理器型号。

use std::ffi::{CStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::AsFd;
use std::path::Path;
pub use std::process::{Child, ChildStdin};
use std::process::{ChildStderr, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rummage_llama::Runtime;

use crate::Error;
use crate::local::VK_VISIBLE_ENV;

/// 放进来的子进程的 pid
pub struct Job(Mutex<Option<u32>>);

impl Job {
    pub fn new(_mem_bytes: u64) -> io::Result<Self> {
        Ok(Job(Mutex::new(None)))
    }

    fn assign(&self, child: &Child) -> io::Result<()> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(child.id());
        Ok(())
    }

    /// 结束子进程。监督方在回收子进程（`wait`）之前先丢弃对 `Job` 的共享引用（`Supervisor::stop`），
    /// 所以 pid 还属于这个子进程；只有它自行退出、被 `try_wait` 回收后到 `stop` 之前的极短时间内例外，占位实现不处理
    pub fn kill(&self) {
        let Some(pid) = *self.0.lock().unwrap_or_else(PoisonError::into_inner) else {
            return;
        };
        let res = Command::new("kill")
            .args(["-KILL", pid.to_string().as_str()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if let Err(err) = res {
            log::warn!("无法结束推理进程 {pid}：{err}");
        }
    }
}

/// 启动推理子进程（标准输入输出都是管道）并记下它的 pid
pub fn spawn(
    exe: &Path,
    args: Vec<OsString>,
    job: &Job,
) -> Result<(Child, ChildStdin, ChildStdout, ChildStderr), Error> {
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove(VK_VISIBLE_ENV.to_str().expect("变量名是 ASCII"))
        .spawn()
        .map_err(|err| Error::Spawn(format!("{}：{err}", exe.display())))?;
    job.assign(&child)
        .map_err(|err| Error::Spawn(format!("无法记下推理进程：{err}")))?;
    let stdin = child.stdin.take().expect("stdin 已设为管道");
    let stdout = child.stdout.take().expect("stdout 已设为管道");
    let stderr = child.stderr.take().expect("stderr 已设为管道");
    Ok((child, stdin, stdout, stderr))
}

/// 推理子进程用的运行库：一个进程只加载一次
pub fn load_runtime(dir: &Path) -> Result<Arc<Runtime>, rummage_llama::Error> {
    Runtime::load(dir).map(Arc::new)
}

/// 设置之后加载的运行库能读到的环境变量。只在推理子进程加载运行库之前、还没有其他线程时调用。
pub fn set_runtime_env(name: &CStr, value: &CStr) -> io::Result<()> {
    let (Ok(name), Ok(value)) = (name.to_str(), value.to_str()) else {
        return Err(io::Error::other("环境变量不是有效的 UTF-8"));
    };
    // SAFETY: 调用时进程中没有其他线程读写环境变量（见上）。
    unsafe { std::env::set_var(name, value) };
    Ok(())
}

pub fn protocol_stdout() -> io::Result<File> {
    Ok(File::from(io::stdout().as_fd().try_clone_to_owned()?))
}

pub fn cpu_name() -> Option<String> {
    None
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
