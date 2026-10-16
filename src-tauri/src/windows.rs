//! 界面进程的 Windows 平台接口。`edit` 用 `ShellExecuteExW` 的 “edit” 动词打开文件（在编辑器中打开脚本而不是执行它，
//! 见 `search.rs` 的 `SCRIPT_EXTS`）；没有注册该动词时返回错误，由调用方改为在资源管理器中显示。
//! 带 `SEE_MASK_FLAG_NO_UI`，失败时不弹出系统的错误框或“打开方式”对话框；在阻塞线程中调用。
//! `default_app` 用 `AssocQueryStringW` 查扩展名的默认程序，供打开到命中位置（`open_at.rs`）判断是不是已知程序。
//! `os_version` 从注册表读取系统名称与版本号、`local_time` 取本地时间，写进诊断包。
//! 托盘：`kill` 强制结束没有按时退出的核心进程；`next_midnight`、`local_time_at` 换算“暂停到明天”与暂停到期时间，
//! 按系统时区（含夏令时）计算。
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, SYSTEMTIME};
use windows_sys::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
use windows_sys::Win32::System::Time::{
    FileTimeToSystemTime, SystemTimeToFileTime, SystemTimeToTzSpecificLocalTime,
    TzSpecificLocalTimeToSystemTime,
};
use windows_sys::Win32::UI::Shell::{
    ASSOCF_INIT_IGNOREUNKNOWN, ASSOCF_NOTRUNCATE, ASSOCSTR_EXECUTABLE, AssocQueryStringW,
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

pub fn edit(path: &Path) -> io::Result<()> {
    let verb = wide(OsStr::new("edit"));
    let file = wide(path.as_os_str());
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    // SAFETY: 保留参数为空、标志为合法常量；只在初始化成功（含 S_FALSE）时配对调用 CoUninitialize。
    let hr = unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    // SAFETY: info 的大小字段正确，动词与文件名是以空字符结尾的宽字符串，调用期间 verb、file 一直有效。
    let ok = unsafe { ShellExecuteExW(&mut info) } != 0;
    let err = io::Error::last_os_error();
    if hr >= 0 {
        // SAFETY: 与上面成功的 CoInitializeEx 配对。
        unsafe { CoUninitialize() };
    }
    if ok { Ok(()) } else { Err(err) }
}

/// 扩展名（不含点）的默认程序的可执行文件（打开到命中位置，见 `open_at.rs`）；没有关联或查不到时为空。
/// 读注册表，可能短暂阻塞，在阻塞线程中调用
pub fn default_app(ext: &str) -> Option<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    let assoc = wide(OsStr::new(&format!(".{ext}")));
    let mut buf = vec![0u16; 2048];
    let mut len = buf.len() as u32;
    // SAFETY: 扩展名是以空字符结尾的宽字符串，附加参数为空表示默认动词；输出缓冲区与其长度一致，调用期间都有效。
    let hr = unsafe {
        AssocQueryStringW(
            ASSOCF_INIT_IGNOREUNKNOWN | ASSOCF_NOTRUNCATE,
            ASSOCSTR_EXECUTABLE,
            assoc.as_ptr(),
            std::ptr::null(),
            buf.as_mut_ptr(),
            &mut len,
        )
    };
    if hr != 0 {
        return None;
    }
    let n = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    (n > 0).then(|| std::ffi::OsString::from_wide(&buf[..n]).into())
}

/// 如“Windows 10 Pro 24H2（内部版本 26100.4351）”；注册表中的名称在 Windows 11 上仍写作 Windows 10，以内部版本号为准
pub fn os_version() -> String {
    let key = match windows_registry::LOCAL_MACHINE
        .open(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion")
    {
        Ok(key) => key,
        Err(err) => return format!("未知（{err}）"),
    };
    let text = |name: &str| key.get_string(name).unwrap_or_default();
    format!(
        "{} {}（内部版本 {}.{}）",
        text("ProductName"),
        text("DisplayVersion"),
        text("CurrentBuild"),
        key.get_u32("UBR").unwrap_or(0)
    )
}

/// 本地时间 `[年, 月, 日, 时, 分, 秒]`，作为诊断包中各项的修改时间
pub fn local_time() -> [u16; 6] {
    let mut t = SYSTEMTIME::default();
    // SAFETY: 只把当前本地时间写进栈上的结构体，指针在调用期间有效。
    unsafe { GetLocalTime(&mut t) };
    [t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond]
}

pub fn kill(pid: u32) -> io::Result<()> {
    // SAFETY: 只请求结束进程的权限；返回的句柄在下面关闭
    let process = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: process 是上面打开的有效句柄
    let ok = unsafe { TerminateProcess(process, 1) } != 0;
    let err = io::Error::last_os_error();
    // SAFETY: process 是上面打开的有效句柄，之后不再使用
    unsafe { CloseHandle(process) };
    if ok { Ok(()) } else { Err(err) }
}

/// FILETIME 的起点（1601-01-01）到 Unix 纪元的 100 纳秒数
const EPOCH_DIFF: i64 = 116_444_736_000_000_000;

fn to_filetime(ticks: i64) -> FILETIME {
    FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    }
}

fn from_filetime(ft: &FILETIME) -> i64 {
    (i64::from(ft.dwHighDateTime) << 32) | i64::from(ft.dwLowDateTime)
}

fn check(ok: i32) -> io::Result<()> {
    if ok != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// 本地时间次日 0 点，Unix 毫秒。在 0 点切换夏令时的时区，那天的 0 点不存在，换算结果由系统决定，可能差一小时
pub fn next_midnight() -> io::Result<i64> {
    let mut today = SYSTEMTIME::default();
    // SAFETY: 只把当前本地时间写进栈上的结构体，指针在调用期间有效。
    unsafe { GetLocalTime(&mut today) };
    today.wHour = 0;
    today.wMinute = 0;
    today.wSecond = 0;
    today.wMilliseconds = 0;
    // 先把本地日期当作不带时区的日历时间加一天，再按时区换算成 UTC
    let mut ft = FILETIME::default();
    // SAFETY: 两个指针都指向栈上有效的结构体
    check(unsafe { SystemTimeToFileTime(&today, &mut ft) })?;
    let next = to_filetime(from_filetime(&ft) + 86_400 * 10_000_000);
    let mut local = SYSTEMTIME::default();
    // SAFETY: 同上
    check(unsafe { FileTimeToSystemTime(&next, &mut local) })?;
    let mut utc = SYSTEMTIME::default();
    // SAFETY: 时区为空表示当前时区；两个指针都指向栈上有效的结构体
    check(unsafe { TzSpecificLocalTimeToSystemTime(std::ptr::null(), &local, &mut utc) })?;
    // SAFETY: 两个指针都指向栈上有效的结构体
    check(unsafe { SystemTimeToFileTime(&utc, &mut ft) })?;
    Ok((from_filetime(&ft) - EPOCH_DIFF) / 10_000)
}

/// Unix 毫秒对应的本地时间 `[年, 月, 日, 时, 分, 秒]`
pub fn local_time_at(ms: i64) -> io::Result<[u16; 6]> {
    let ticks = ms
        .checked_mul(10_000)
        .and_then(|t| t.checked_add(EPOCH_DIFF))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "时间超出范围"))?;
    let ft = to_filetime(ticks);
    let mut utc = SYSTEMTIME::default();
    // SAFETY: 两个指针都指向栈上有效的结构体
    check(unsafe { FileTimeToSystemTime(&ft, &mut utc) })?;
    let mut t = SYSTEMTIME::default();
    // SAFETY: 时区为空表示当前时区；两个指针都指向栈上有效的结构体
    check(unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut t) })?;
    Ok([t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_midnight_is_local_midnight_tomorrow() {
        // 恰好跨过 0 点时前后取到的日期不同，重试一次
        for _ in 0..2 {
            let before = local_time();
            let t = local_time_at(next_midnight().unwrap()).unwrap();
            if local_time()[..3] != before[..3] {
                continue;
            }
            assert_eq!(t[3..], [0, 0, 0]);
            assert_ne!(t[..3], before[..3]);
            return;
        }
        panic!("连续两次跨过 0 点");
    }

    #[test]
    fn local_time_at_rejects_overflow() {
        assert!(local_time_at(i64::MAX).is_err());
    }
}
