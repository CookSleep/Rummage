//! 一个运行中的推理子进程：启动并放进作业对象、发 `Start`，由两个线程分别读取 stdout 上的帧与 stderr 末尾，
//! 结束时连同作业一起结束并读完 stderr；结束不了时最多等 `KILL_GRACE`，不让调用方永久卡住。监督与重启规则见上级模块 `local`。

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{VK_VISIBLE_ENV, lock};
use crate::frame::{self, FromWorker, ToWorker};
use crate::{Error, KILL_GRACE, platform};

/// 父进程保留的子进程 stderr 行数，用于错误信息和设备丢失判断。
const STDERR_LINES: usize = 200;
/// 结束推理进程后检查它是否已退出的间隔
const KILL_POLL: Duration = Duration::from_millis(20);

pub(super) enum Recv {
    Frame(FromWorker, Vec<u8>),
    Timeout,
    Closed(String),
}

/// 一个运行中的推理子进程。stdout 与 stderr 各由一个线程读取。
pub(super) struct Proc {
    pub(super) child: Child,
    /// 与 `Status` 共享，供 `LocalBackend::kill_worker` 在调度线程之外结束推理进程。
    pub(super) job: Arc<platform::Job>,
    pub(super) stdin: Option<ChildStdin>,
    pub(super) frames: Receiver<Result<(FromWorker, Vec<u8>), String>>,
    pub(super) stderr: Option<JoinHandle<()>>,
    pub(super) tail: Arc<Mutex<VecDeque<String>>>,
}

impl Proc {
    pub(super) fn spawn(exe: &Path, args: Vec<OsString>, mem_bytes: u64) -> Result<Proc, Error> {
        let job = platform::Job::new(mem_bytes)
            .map_err(|err| Error::Spawn(format!("无法创建作业对象：{err}")))?;
        let job = Arc::new(job);
        let mut cmd = Command::new(exe);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_remove(VK_VISIBLE_ENV.to_str().expect("变量名是 ASCII"));
        platform::hide_window(&mut cmd);
        let mut child = cmd
            .spawn()
            .map_err(|err| Error::Spawn(format!("{}：{err}", exe.display())))?;
        if let Err(err) = job.assign(&child) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Spawn(format!("无法把推理进程放进作业对象：{err}")));
        }
        let mut stdin = child.stdin.take();
        // 已在作业中，通知子进程开始加载运行库。写失败说明子进程已退出，交给下面的读取线程报告。
        if let Some(w) = stdin.as_mut() {
            let _ = frame::write(w, &ToWorker::Start, &[]);
        }
        let stdout = child.stdout.take().expect("stdout 已设为管道");
        let stderr = child.stderr.take().expect("stderr 已设为管道");

        let (tx, frames) = mpsc::channel();
        thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            loop {
                let msg = match frame::read::<FromWorker>(&mut r) {
                    Ok(Some(f)) => Ok(f),
                    Ok(None) => Err("推理进程关闭了输出".to_string()),
                    Err(err) => Err(format!("读取推理进程的输出失败：{err}")),
                };
                let end = msg.is_err();
                if tx.send(msg).is_err() || end {
                    return;
                }
            }
        });
        let tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_LINES)));
        let t = tail.clone();
        let stderr = thread::spawn(move || {
            let mut r = BufReader::new(stderr);
            let mut line = Vec::new();
            loop {
                line.clear();
                if !matches!(r.read_until(b'\n', &mut line), Ok(n) if n > 0) {
                    return;
                }
                let mut t = lock(&t);
                if t.len() == STDERR_LINES {
                    t.pop_front();
                }
                t.push_back(String::from_utf8_lossy(&line).trim_end().to_string());
            }
        });
        Ok(Proc {
            child,
            job,
            stdin,
            frames,
            stderr: Some(stderr),
            tail,
        })
    }

    pub(super) fn recv(&self, timeout: Duration) -> Recv {
        match self.frames.recv_timeout(timeout) {
            Ok(Ok((msg, payload))) => Recv::Frame(msg, payload),
            Ok(Err(msg)) => Recv::Closed(msg),
            Err(RecvTimeoutError::Timeout) => Recv::Timeout,
            Err(RecvTimeoutError::Disconnected) => Recv::Closed("推理进程关闭了输出".to_string()),
        }
    }

    pub(super) fn tail(&self) -> String {
        let t = lock(&self.tail);
        t.iter().map(String::as_str).collect::<Vec<_>>().join("\n")
    }

    /// 结束整个作业，最多等 `grace` 让它退出并读完 stderr；返回退出状态、stderr 的最后若干行与是否已退出。
    /// 驱动挂起时进程可能结束不了：到期就不再等它，读取 stderr 的线程随它留着
    pub(super) fn kill(&mut self, grace: Duration) -> (Option<ExitStatus>, String, bool) {
        self.job.kill();
        let _ = self.child.kill();
        self.stdin = None;
        let (status, ended) = self.wait_exit(grace);
        (status, self.tail(), ended)
    }

    /// 最多等 `grace` 让进程退出并读完 stderr；返回退出状态与是否已退出
    pub(super) fn wait_exit(&mut self, grace: Duration) -> (Option<ExitStatus>, bool) {
        let deadline = Instant::now() + grace;
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(s)) => break Some(s),
                Ok(None) if Instant::now() < deadline => thread::sleep(KILL_POLL),
                Ok(None) => {
                    self.stderr = None;
                    return (None, false);
                }
                Err(_) => break None,
            }
        };
        // 进程退出后管道随之关闭，读取线程很快结束
        if let Some(h) = self.stderr.take() {
            while !h.is_finished() && Instant::now() < deadline {
                thread::sleep(KILL_POLL);
            }
            if h.is_finished() {
                let _ = h.join();
            }
        }
        (status, true)
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        if self.stderr.is_some() {
            self.kill(KILL_GRACE);
        }
    }
}
