//! 非 Windows 平台的占位实现：只用于编译和测试，用 std 直接拉起，不处理句柄继承与作业对象；当前目录同样设为 exe 所在的目录
use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::{Child, Command, Stdio};

pub struct Proc(Child);

impl Proc {
    /// 等进程结束，返回退出码；被信号结束时为 1
    pub fn wait(mut self) -> io::Result<u32> {
        Ok(self.0.wait()?.code().map_or(1, |c| c as u32))
    }
}

pub fn spawn_detached(exe: &Path, args: &[&OsStr]) -> io::Result<Proc> {
    Command::new(exe)
        .args(args)
        .current_dir(exe.parent().unwrap_or(Path::new(".")))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(Proc)
}
