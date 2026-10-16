//! Windows 上拉起常驻的核心进程，直接调用 `CreateProcessW`：
//! - **不继承任何句柄**（`bInheritHandles` 为假）。std 的 `Command` 总是让子进程继承父进程全部可继承的句柄：命令行被管道
//!   接收时，常驻的核心会一直占着管道，`rummage-core search x | findstr y` 要等核心退出才结束。只去掉本进程标准句柄的
//!   继承标志不够：在 `cmd /c "rummage-core search … | findstr …"` 中，cmd 把它自己的标准输出（调用方的管道）也作为
//!   可继承句柄传给命令行，核心再继承下去。核心的标准输入输出落在 `CREATE_NO_WINDOW` 创建的不显示的控制台上。
//! - 尝试脱离调用方的作业对象（`CREATE_BREAKAWAY_FROM_JOB`）：MCP 客户端可能把子进程放进“作业关闭时结束全部进程”的
//!   作业对象，核心会随之被结束。作业对象不允许脱离时 `CreateProcessW` 返回拒绝访问，此时不带这个标志重试。
//! - 当前目录设为 exe 所在的目录，不沿用调用方的当前目录（否则核心会一直占着调用方当时所在的文件夹，
//!   使它无法删除，所在的移动硬盘也无法弹出）。
//! - 命令行按 `CommandLineToArgvW` 的规则给每个参数加引号（引号前与末尾的反斜杠加倍）。
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::null;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, HANDLE, WAIT_FAILED};
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW, CreateProcessW, GetExitCodeProcess, INFINITE,
    PROCESS_INFORMATION, STARTUPINFOW, WaitForSingleObject,
};

/// 拉起的进程，持有它的进程句柄
pub struct Proc(HANDLE);

// SAFETY: 进程句柄是内核对象的引用，可以在任何线程上等待和关闭；Proc 独占这个句柄
unsafe impl Send for Proc {}

impl Drop for Proc {
    fn drop(&mut self) {
        // SAFETY: self.0 是 CreateProcessW 返回的进程句柄，只在这里关闭一次
        unsafe { CloseHandle(self.0) };
    }
}

impl Proc {
    /// 等进程结束，返回退出码
    pub fn wait(self) -> io::Result<u32> {
        // SAFETY: self.0 是有效的进程句柄
        if unsafe { WaitForSingleObject(self.0, INFINITE) } == WAIT_FAILED {
            return Err(io::Error::last_os_error());
        }
        let mut code = 0;
        // SAFETY: self.0 是有效的进程句柄，code 是有效的输出位置
        if unsafe { GetExitCodeProcess(self.0, &mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(code)
    }
}

/// 按 `CommandLineToArgvW` 的规则加引号后追加到命令行
fn push_quoted(out: &mut Vec<u16>, arg: &OsStr) {
    let (quote, backslash) = (u16::from(b'"'), u16::from(b'\\'));
    out.push(quote);
    let mut slashes = 0;
    for c in arg.encode_wide() {
        if c == backslash {
            slashes += 1;
        } else {
            if c == quote {
                out.extend(std::iter::repeat_n(backslash, slashes + 1));
            }
            slashes = 0;
        }
        out.push(c);
    }
    // 结尾的反斜杠后面紧跟收尾的引号，要加倍
    out.extend(std::iter::repeat_n(backslash, slashes));
    out.push(quote);
}

fn command_line(exe: &Path, args: &[&OsStr]) -> Vec<u16> {
    let mut line = Vec::new();
    push_quoted(&mut line, exe.as_os_str());
    for a in args {
        line.push(u16::from(b' '));
        push_quoted(&mut line, a);
    }
    line.push(0);
    line
}

fn create(exe: &[u16], line: &mut [u16], cwd: &[u16], flags: u32) -> io::Result<Proc> {
    // SAFETY: 全零的 STARTUPINFOW 是合法的初始值，随后设置 cb
    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = size_of::<STARTUPINFOW>() as u32;
    // SAFETY: 同上，PROCESS_INFORMATION 只作输出
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: exe、line、cwd 都以 0 结尾，line 可写（CreateProcessW 可能原地修改命令行）；不继承句柄、不传环境块，
    // 当前目录为 exe 所在的目录；si、pi 在调用期间有效
    let ok = unsafe {
        CreateProcessW(
            exe.as_ptr(),
            line.as_mut_ptr(),
            null(),
            null(),
            0,
            flags,
            null(),
            cwd.as_ptr(),
            &si,
            &mut pi,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: 主线程句柄由 CreateProcessW 返回，不再使用，立即关闭
    unsafe { CloseHandle(pi.hThread) };
    Ok(Proc(pi.hProcess))
}

pub fn spawn_detached(exe: &Path, args: &[&OsStr]) -> io::Result<Proc> {
    let app: Vec<u16> = exe.as_os_str().encode_wide().chain([0]).collect();
    let mut line = command_line(exe, args);
    let dir = exe.parent().unwrap_or(Path::new("."));
    let cwd: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
    match create(
        &app,
        &mut line.clone(),
        &cwd,
        CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB,
    ) {
        Err(err) if err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
            create(&app, &mut line, &cwd, CREATE_NO_WINDOW)
        }
        res => res,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

    use super::*;

    /// 用系统的规则把命令行拆回参数
    fn split(line: &[u16]) -> Vec<OsString> {
        let mut n = 0;
        // SAFETY: line 以 0 结尾；返回的数组由 LocalFree 释放
        let argv = unsafe { CommandLineToArgvW(line.as_ptr(), &mut n) };
        assert!(!argv.is_null());
        let out = (0..n as usize)
            .map(|i| {
                // SAFETY: argv 有 n 项，每项是以 0 结尾的宽字符串
                let p = unsafe { *argv.add(i) };
                // SAFETY: 逐个读到结尾的 0 为止，不越过字符串末尾
                let len = (0..).take_while(|&j| unsafe { *p.add(j) } != 0).count();
                // SAFETY: p 指向 len 个有效字符
                OsString::from_wide(unsafe { std::slice::from_raw_parts(p, len) })
            })
            .collect();
        // SAFETY: argv 由 CommandLineToArgvW 分配
        unsafe { LocalFree(argv.cast()) };
        out
    }

    #[test]
    fn quoting_round_trips() {
        let args = [
            r"D:\我的 资料\data dir",
            r"D:\末尾\",
            r#"引号"中间"#,
            r#"a\"b"#,
            "",
            r"\\server\share\x",
        ];
        let os: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        let exe = Path::new(r"C:\Program Files\Rummage\rummage-core.exe");
        let parsed = split(&command_line(exe, &os));
        assert_eq!(parsed[0], exe.as_os_str());
        assert_eq!(
            parsed[1..],
            args.iter().map(OsString::from).collect::<Vec<_>>()[..]
        );
    }

    #[test]
    fn spawns_and_waits() {
        // where 找不到时退出码为 1：加了引号的参数按标准规则原样到达
        let exe = Path::new(r"C:\Windows\System32\where.exe");
        let p = spawn_detached(exe, &[OsStr::new("/q"), OsStr::new("rummage 不存在.exe")]).unwrap();
        assert_eq!(p.wait().unwrap(), 1);
        let p = spawn_detached(exe, &[OsStr::new("/q"), OsStr::new("cmd.exe")]).unwrap();
        assert_eq!(p.wait().unwrap(), 0);
        assert!(spawn_detached(Path::new(r"C:\不存在\x.exe"), &[]).is_err());
    }

    #[test]
    fn runs_in_exe_dir() {
        // where 先在当前目录中找：复制到临时目录后，只有当前目录是 exe 所在的目录时才能找到旁边的标记文件
        let dir = crate::test_dir();
        let exe = dir.path().join("where.exe");
        std::fs::copy(r"C:\Windows\System32\where.exe", &exe).unwrap();
        std::fs::write(dir.path().join("rummage-cwd.marker"), b"").unwrap();
        let p =
            spawn_detached(&exe, &[OsStr::new("/q"), OsStr::new("rummage-cwd.marker")]).unwrap();
        assert_eq!(p.wait().unwrap(), 0);
    }
}
