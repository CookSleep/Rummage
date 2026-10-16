//! Windows 上运行 ffmpeg / ffprobe：以 `CREATE_NO_WINDOW` 启动（核心进程没有控制台，否则每个文件都会闪出窗口），
//! stdout 逐块交给调用方，stderr 在另一个线程中读完、只留末尾（读不完 ffmpeg 会卡在写 stderr 上）。
//! 看门狗线程到时限就结束子进程；调用方不再需要输出时（如超过采样上限）也立即结束。
//! 子进程都放进进程内共用的一个 Job Object（`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`），句柄一直不关：核心进程退出或被强行结束时
//! 系统关闭句柄，正在运行的 ffmpeg 随之结束，不会留下孤儿进程（例如卡在读慢速磁盘上、还没写管道的 ffprobe）。

use std::ffi::OsString;
use std::io::Read;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::Duration;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows::Win32::System::Threading::CREATE_NO_WINDOW;
use windows::core::PCWSTR;

use super::{MediaError, Run};

/// 错误信息只需要 stderr 的最后几行
const STDERR_TAIL: usize = 4096;

/// 进程内共用的作业（见模块说明）；创建失败时为空，只记日志，看门狗与管道断开仍能结束子进程
fn job() -> Option<HANDLE> {
    static JOB: OnceLock<Option<usize>> = OnceLock::new();
    let job = JOB.get_or_init(|| {
        // SAFETY: 不带安全属性和名字（句柄不可继承）；成功后句柄保存到进程退出，从不关闭。
        let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
            .inspect_err(|err| log::warn!("无法创建 ffmpeg 的作业对象：{err}"))
            .ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: 句柄有效；指针与长度描述同一个结构体，调用期间有效。
        unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .inspect_err(|err| log::warn!("无法设置 ffmpeg 的作业对象：{err}"))
        .ok()?;
        Some(job.0 as usize)
    });
    job.map(|h| HANDLE(h as *mut _))
}

fn kill(child: &Mutex<Child>) {
    // 已经退出时 kill 返回错误，忽略
    let _ = child.lock().unwrap_or_else(PoisonError::into_inner).kill();
}

/// `sink` 返回否时结束子进程、不再读取
pub(super) fn run(
    exe: &Path,
    args: &[OsString],
    timeout: Duration,
    sink: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<Run, MediaError> {
    let name = exe
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn()
        .map_err(|err| MediaError::Spawn {
            tool: name.clone(),
            detail: err.to_string(),
        })?;
    if let Some(job) = job() {
        // SAFETY: 作业句柄有效；进程句柄由 `child` 持有，调用期间有效。
        if let Err(err) = unsafe { AssignProcessToJobObject(job, HANDLE(child.as_raw_handle())) } {
            log::warn!("无法把 {name} 放进作业对象：{err}");
        }
    }
    let mut out = child.stdout.take().expect("stdout 设为管道");
    let mut err = child.stderr.take().expect("stderr 设为管道");
    let child = Arc::new(Mutex::new(child));
    let timed_out = Arc::new(AtomicBool::new(false));
    let (done, wait) = mpsc::channel::<()>();
    let watchdog = {
        let (child, timed_out) = (child.clone(), timed_out.clone());
        thread::spawn(move || {
            if let Err(RecvTimeoutError::Timeout) = wait.recv_timeout(timeout) {
                timed_out.store(true, Ordering::Relaxed);
                kill(&child);
            }
        })
    };
    let stderr = thread::spawn(move || {
        let mut tail = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = err.read(&mut buf) {
            if n == 0 {
                break;
            }
            tail.extend_from_slice(&buf[..n]);
            if tail.len() > STDERR_TAIL {
                tail.drain(..tail.len() - STDERR_TAIL);
            }
        }
        String::from_utf8_lossy(&tail).into_owned()
    });
    let mut buf = vec![0u8; 64 * 1024];
    // 子进程退出或被结束后管道关闭，读到 0 或出错
    while let Ok(n) = out.read(&mut buf) {
        if n == 0 {
            break;
        }
        if !sink(&buf[..n]) {
            kill(&child);
            break;
        }
    }
    drop(out);
    // 通知看门狗不必再等
    drop(done);
    let _ = watchdog.join();
    let status = child
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .wait()
        .map_err(|err| MediaError::Spawn {
            tool: name,
            detail: err.to_string(),
        })?;
    Ok(Run {
        ok: status.success(),
        stderr: stderr.join().unwrap_or_default(),
        timed_out: timed_out.load(Ordering::Relaxed),
    })
}
