//! 关机与注销（`on_session_end`）：核心由界面以 `CREATE_NO_WINDOW` 启动，且会加载 user32.dll（核心进程的模块中
//! 有 USER32、GDI32）。按 `SetConsoleCtrlHandler` 的文档，加载了 user32 / gdi32 的控制台程序收不到
//! `CTRL_LOGOFF_EVENT` / `CTRL_SHUTDOWN_EVENT`，没有控制台窗口时也不会有 `CTRL_CLOSE_EVENT`；文档建议改用隐藏窗口
//! 处理 `WM_QUERYENDSESSION` / `WM_ENDSESSION`。仅消息窗口（`HWND_MESSAGE`）收不到这类广播消息，所以用一个不显示的
//! 普通顶层窗口，在专用线程上收消息。`WM_ENDSESSION` 的 `wParam` 为真时会话确定结束，窗口过程返回后进程随时可能被
//! 结束，所以回调同步执行完再返回。
//!
//! 电源事件（`on_power`）：同一个隐藏窗口接收 `WM_POWERBROADCAST`，只处理 `PBT_APMSUSPEND`（即将睡眠）与
//! `PBT_APMRESUMEAUTOMATIC`（唤醒，无论用户是否在场都会发出，只发一次）。窗口在第一次调用 `on_session_end` 或
//! `on_power` 时创建，之后共用。回调在窗口线程上同步执行，耗时的工作由回调自己放到别的线程。
use std::io::{self, ErrorKind};
use std::ptr::null_mut;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowExW, GetMessageW,
    GetWindowThreadProcessId, MSG, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND, RegisterClassExW,
    SendMessageW, TranslateMessage, WM_ENDSESSION, WM_POWERBROADCAST, WM_QUERYENDSESSION,
    WNDCLASSEXW,
};

use super::wide;
use crate::Power;

/// 监听关机与注销的隐藏窗口的类名
const SESSION_WINDOW_CLASS: &str = "RummageCoreSessionEnd";

/// 本进程的会话窗口，供单元测试直接发消息
#[cfg(test)]
static SESSION_HWND: std::sync::atomic::AtomicPtr<std::ffi::c_void> =
    std::sync::atomic::AtomicPtr::new(null_mut());

/// 会话结束时执行的回调，窗口过程中读取
static SESSION_END: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
/// 电源事件的回调，窗口过程中读取
static POWER: OnceLock<Box<dyn Fn(Power) + Send + Sync>> = OnceLock::new();
/// 隐藏窗口的创建结果；只创建一次
static WINDOW: OnceLock<Result<(), String>> = OnceLock::new();

unsafe extern "system" fn session_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // 不阻止关机或注销
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if wparam != 0
                && let Some(f) = SESSION_END.get()
            {
                f();
            }
            0
        }
        WM_POWERBROADCAST if matches!(wparam as u32, PBT_APMSUSPEND | PBT_APMRESUMEAUTOMATIC) => {
            if let Some(f) = POWER.get() {
                f(if wparam as u32 == PBT_APMSUSPEND {
                    Power::Suspend
                } else {
                    Power::Resume
                });
            }
            1
        }
        // SAFETY: 参数原样来自系统对本窗口过程的调用
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 在本线程创建隐藏窗口并收消息，直到出错；创建结果经 `ready` 告知调用方
fn session_window(ready: std::sync::mpsc::Sender<io::Result<()>>) {
    let class = wide(SESSION_WINDOW_CLASS);
    // SAFETY: 传入空指针取当前可执行文件的模块句柄
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(session_proc),
        hInstance: instance,
        lpszClassName: class.as_ptr(),
        // SAFETY: WNDCLASSEXW 其余字段都允许为零（无图标、光标、背景和菜单）
        ..unsafe { std::mem::zeroed() }
    };
    // SAFETY: wc 已完整初始化，类名在本线程存活期间有效
    if unsafe { RegisterClassExW(&wc) } == 0 {
        let _ = ready.send(Err(io::Error::last_os_error()));
        return;
    }
    // 普通顶层窗口：父窗口为空、不调用 ShowWindow，所以不显示；仅消息窗口收不到 WM_QUERYENDSESSION
    // SAFETY: 类已注册，类名与标题都是以 0 结尾的 UTF-16 字符串，其余参数为零或空
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        let _ = ready.send(Err(io::Error::last_os_error()));
        return;
    }
    #[cfg(test)]
    SESSION_HWND.store(hwnd, std::sync::atomic::Ordering::SeqCst);
    let _ = ready.send(Ok(()));
    // SAFETY: MSG 全为零是合法的初始值
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    // SAFETY: msg 在循环期间有效；GetMessageW 出错（-1）或收到 WM_QUIT（0）时退出
    while unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) } > 0 {
        // SAFETY: msg 来自 GetMessageW
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// 创建隐藏窗口；已创建（或创建失败过）时直接返回上次的结果
fn ensure_window() -> io::Result<()> {
    let res = WINDOW.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("session-end".into())
            .spawn(move || session_window(tx))
            .map_err(|err| err.to_string())?;
        match rx.recv() {
            Ok(res) => res.map_err(|err| err.to_string()),
            Err(_) => Err("会话监听线程意外退出".to_string()),
        }
    });
    res.clone().map_err(io::Error::other)
}

/// 系统关机或注销时调用 `on_end`（见模块说明）。只能调用一次；窗口创建失败时返回错误
pub fn on_session_end(on_end: Box<dyn Fn() + Send + Sync>) -> io::Result<()> {
    if SESSION_END.set(on_end).is_err() {
        return Err(io::Error::new(ErrorKind::AlreadyExists, "已在监听会话结束"));
    }
    ensure_window()
}

/// 系统即将睡眠、已唤醒时调用 `on_power`（见模块说明）。只能调用一次；窗口创建失败时返回错误
pub fn on_power(on_power: Box<dyn Fn(Power) + Send + Sync>) -> io::Result<()> {
    if POWER.set(on_power).is_err() {
        return Err(io::Error::new(ErrorKind::AlreadyExists, "已在监听电源事件"));
    }
    ensure_window()
}

/// 向进程 `pid` 的会话窗口发送与系统关机时相同的 `WM_QUERYENDSESSION`、`WM_ENDSESSION`，供集成测试模拟关机；
/// 找不到该进程的窗口时返回 `NotFound`
pub fn send_session_end(pid: u32) -> io::Result<()> {
    let class = wide(SESSION_WINDOW_CLASS);
    let mut hwnd: HWND = null_mut();
    loop {
        // SAFETY: 类名是以 0 结尾的 UTF-16 字符串；hwnd 为空或上一次找到的窗口
        hwnd = unsafe { FindWindowExW(null_mut(), hwnd, class.as_ptr(), std::ptr::null()) };
        if hwnd.is_null() {
            return Err(io::Error::new(
                ErrorKind::NotFound,
                "找不到该进程的会话窗口",
            ));
        }
        let mut owner = 0;
        // SAFETY: hwnd 是刚找到的窗口，owner 在调用期间有效
        unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
        if owner == pid {
            break;
        }
    }
    // SAFETY: 两条消息都不带指针参数
    unsafe {
        SendMessageW(hwnd, WM_QUERYENDSESSION, 0, 0);
        SendMessageW(hwnd, WM_ENDSESSION, 1, 0);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn end_session_runs_callback() {
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        on_session_end(Box::new(move || {
            h.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();
        assert!(on_session_end(Box::new(|| {})).is_err());
        let hwnd = SESSION_HWND.load(Ordering::SeqCst);
        assert!(!hwnd.is_null());
        // SAFETY: hwnd 是本进程刚创建的窗口，这两条消息不带指针参数
        unsafe {
            assert_eq!(SendMessageW(hwnd, WM_QUERYENDSESSION, 0, 0), 1);
            // 关机被取消：不执行
            SendMessageW(hwnd, WM_ENDSESSION, 0, 0);
            assert_eq!(hits.load(Ordering::SeqCst), 0);
            SendMessageW(hwnd, WM_ENDSESSION, 1, 0);
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn power_broadcast_runs_callback() {
        let got = Arc::new(std::sync::Mutex::new(Vec::new()));
        let g = got.clone();
        on_power(Box::new(move |event| g.lock().unwrap().push(event))).unwrap();
        assert!(on_power(Box::new(|_| {})).is_err());
        let hwnd = SESSION_HWND.load(Ordering::SeqCst);
        assert!(!hwnd.is_null());
        // 只发给本进程的窗口
        let mut owner = 0;
        // SAFETY: hwnd 是本进程刚创建的窗口，owner 在调用期间有效
        unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
        assert_eq!(owner, std::process::id());
        // SAFETY: 同上；WM_POWERBROADCAST 的这几种事件 lParam 为 0，不带指针
        unsafe {
            assert_eq!(
                SendMessageW(hwnd, WM_POWERBROADCAST, PBT_APMSUSPEND as usize, 0),
                1
            );
            SendMessageW(hwnd, WM_POWERBROADCAST, PBT_APMRESUMEAUTOMATIC as usize, 0);
            // PBT_APMRESUMESUSPEND（用户在场时额外发出的唤醒通知）忽略，避免同一次唤醒验证两次
            SendMessageW(
                hwnd,
                WM_POWERBROADCAST,
                windows_sys::Win32::UI::WindowsAndMessaging::PBT_APMRESUMESUSPEND as usize,
                0,
            );
        }
        assert_eq!(*got.lock().unwrap(), [Power::Suspend, Power::Resume]);
    }
}
