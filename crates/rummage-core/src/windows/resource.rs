//! 资源占用控制：
//! - 进程优先级（`lower_priority`）：核心进程设为 `BELOW_NORMAL_PRIORITY_CLASS`。推理子进程会继承：按 `CreateProcess`
//!   的文档，创建时没有指定优先级类、而父进程是 `IDLE` 或 `BELOW_NORMAL` 时，子进程取父进程的优先级类；
//!   推理子进程的 Job Object 也没有设 `JOB_OBJECT_LIMIT_PRIORITY_CLASS`。集成测试 `tests/resource.rs` 核对这一点。
//! - 检测（`sense`）：参照 Chromium 的 `ui/base/fullscreen_win.cc`。`SHQueryUserNotificationState` 为
//!   `QUNS_RUNNING_D3D_FULL_SCREEN` 或 `QUNS_PRESENTATION_MODE`（D3D 独占全屏、演示模式），或者有窗口盖满它所在的
//!   显示器时算全屏。不用 `QUNS_BUSY`：它会被点击穿透的叠加层（如盖满整屏的屏幕光效窗口）触发，让索引一直暂停。
//!   窗口的样式筛选沿用 Chromium：样式有 `WS_DLGFRAME` / `WS_THICKFRAME`、扩展样式有 `WS_EX_WINDOWEDGE` /
//!   `WS_EX_TOOLWINDOW` 的不算，可调整大小的普通窗口无论是否最大化都由此排除。与 Chromium 有意不同：Chromium 只看
//!   主显示器上的前台窗口，这里用 `EnumWindows` 检查所有显示器上的顶层窗口，与各自所在显示器的 `rcMonitor` 比较，
//!   让副屏上的全屏视频也能暂停索引；因此另外排除点击穿透的叠加层（扩展样式同时有 `WS_EX_TRANSPARENT` 与
//!   `WS_EX_LAYERED`，或有 `WS_EX_NOACTIVATE`），它们不会成为前台，Chromium 不需要处理。同时跳过最小化窗口与被 DWM
//!   隐藏的窗口（`DWMWA_CLOAKED` 非零，例如挂起的 UWP 窗口、其他虚拟桌面上的窗口；按 Raymond Chen 的
//!   “Cloaked windows”一文，只看 `IsWindowVisible` 不够）、桌面与外壳窗口（`Progman`、`WorkerW`、`GetShellWindow`、
//!   `GetDesktopWindow`），以及本进程的窗口（Chromium 同样排除自己的窗口）。窗口取 DWM 的可见边界
//!   （`DWMWA_EXTENDED_FRAME_BOUNDS`，物理像素；`GetWindowRect` 含不可见的缩放边框），显示器范围在临时切到
//!   “按显示器感知 DPI”的线程上取，两者都是物理像素。`GetSystemPowerStatus` 的 `ACLineStatus` 为 0 时算使用电池
//!   （255 表示未知，不算）；`GetDiskFreeSpaceExW` 取数据目录所在卷对当前用户可用的空间。
use std::io;
use std::path::Path;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromWindow,
};
use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows_sys::Win32::System::Threading::{
    BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, SetPriorityClass,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::Shell::{
    QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GWL_STYLE, GetClassNameW, GetDesktopWindow, GetShellWindow,
    GetWindowLongW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, WS_DLGFRAME,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_EX_WINDOWEDGE,
    WS_THICKFRAME,
};

use super::wide;
use crate::resource::Sensed;

/// 把本进程设为低优先级（`BELOW_NORMAL_PRIORITY_CLASS`），推理子进程随之继承（见模块说明）
pub fn lower_priority() -> io::Result<()> {
    // SAFETY: GetCurrentProcess 返回的伪句柄总是有效，带 PROCESS_SET_INFORMATION 权限
    if unsafe { SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// 窗口可见边界是否盖满显示器（两者同为物理像素）
fn covers(win: &RECT, mon: &RECT) -> bool {
    win.left <= mon.left && win.top <= mon.top && win.right >= mon.right && win.bottom >= mon.bottom
}

/// 扩展样式是否为点击穿透的叠加层（见模块说明）
fn overlay(ex: u32) -> bool {
    let through = WS_EX_TRANSPARENT | WS_EX_LAYERED;
    ex & through == through || ex & WS_EX_NOACTIVATE != 0
}

/// 是否为带边框的普通窗口或工具窗口，沿用 Chromium `fullscreen_win.cc` 的样式筛选（见模块说明）
fn framed(style: u32, ex: u32) -> bool {
    style & (WS_DLGFRAME | WS_THICKFRAME) != 0 || ex & (WS_EX_WINDOWEDGE | WS_EX_TOOLWINDOW) != 0
}

/// 窗口是否盖满它所在的显示器（见模块说明）；调用方已让本线程按显示器感知 DPI。查询失败按“不是”处理
fn window_covers_monitor(hwnd: HWND, shell: HWND, desktop: HWND) -> bool {
    if hwnd == shell || hwnd == desktop {
        return false;
    }
    let mut owner = 0;
    // SAFETY: hwnd 来自 EnumWindows，期间失效时这些调用返回 0；owner 是输出参数
    let (visible, iconic, style, ex) = unsafe {
        GetWindowThreadProcessId(hwnd, &mut owner);
        (
            IsWindowVisible(hwnd) != 0,
            IsIconic(hwnd) != 0,
            GetWindowLongW(hwnd, GWL_STYLE) as u32,
            GetWindowLongW(hwnd, GWL_EXSTYLE) as u32,
        )
    };
    if owner == std::process::id() || !visible || iconic || overlay(ex) || framed(style, ex) {
        return false;
    }
    let mut cloaked = 0u32;
    // SAFETY: cloaked 是输出参数，传入的大小与其一致
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED as u32,
            (&mut cloaked as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
    };
    if hr >= 0 && cloaked != 0 {
        return false;
    }
    let mut class = [0u16; 64];
    // SAFETY: class 可写 64 个 UTF-16 单元，长度按此传入；hwnd 失效时调用失败返回 0
    let len = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
    let class = String::from_utf16_lossy(&class[..len.max(0) as usize]);
    if class == "Progman" || class == "WorkerW" {
        return false;
    }
    // SAFETY: RECT 全为零是合法的初始值
    let mut win: RECT = unsafe { std::mem::zeroed() };
    // SAFETY: win 是输出参数，传入的大小与其一致
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            (&mut win as *mut RECT).cast(),
            size_of::<RECT>() as u32,
        )
    };
    if hr < 0 {
        return false;
    }
    // SAFETY: MONITORINFO 全为零是合法的初始值，cbSize 随后设置
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<MONITORINFO>() as u32;
    // SAFETY: hwnd 失效时 MONITOR_DEFAULTTONULL 返回空，不调用 GetMonitorInfoW；info.cbSize 已设置
    let ok = unsafe {
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL);
        !mon.is_null() && GetMonitorInfoW(mon, &mut info) != 0
    };
    ok && covers(&win, &info.rcMonitor)
}

unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> windows_sys::core::BOOL {
    // SAFETY: lparam 是 any_window_covers_monitor 传入的 Vec 指针，EnumWindows 返回前一直有效且只在本线程访问
    unsafe { (*(lparam as *mut Vec<HWND>)).push(hwnd) };
    1
}

/// 任一显示器上是否有窗口盖满整个显示器（见模块说明）
fn any_window_covers_monitor() -> bool {
    let mut windows: Vec<HWND> = Vec::new();
    // SAFETY: 回调只把句柄放进 windows，windows 在调用期间有效
    if unsafe {
        EnumWindows(
            Some(collect_window),
            &mut windows as *mut Vec<HWND> as LPARAM,
        )
    } == 0
    {
        log::debug!("无法枚举窗口：{}", io::Error::last_os_error());
        return false;
    }
    // SAFETY: 这两个函数没有参数，返回值可能为空
    let (shell, desktop) = unsafe { (GetShellWindow(), GetDesktopWindow()) };
    // 核心进程不感知 DPI，系统给的显示器坐标会按缩放换算；临时让本线程按显示器感知 DPI，取物理像素，用完恢复
    // SAFETY: 只改变本线程的 DPI 感知方式，下面立即恢复；参数是系统定义的常量
    let old = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let found = windows
        .into_iter()
        .any(|hwnd| window_covers_monitor(hwnd, shell, desktop));
    if !old.is_null() {
        // SAFETY: old 是上面 SetThreadDpiAwarenessContext 返回的本线程原来的值
        unsafe { SetThreadDpiAwarenessContext(old) };
    }
    found
}

/// 检测全屏、电池与 `dir` 所在卷的剩余空间（见模块说明）；查询失败的项按“不暂停”处理。
/// 每 5 秒调用一次，失败只记 debug 日志，避免刷满日志文件
pub(crate) fn sense(dir: &Path) -> Sensed {
    let mut state = 0;
    // SAFETY: state 是输出参数，调用期间有效
    let hr = unsafe { SHQueryUserNotificationState(&mut state) };
    if hr < 0 {
        log::debug!("无法查询全屏状态：HRESULT {hr:#010x}");
    }
    let fullscreen = (hr >= 0
        && matches!(state, QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE))
        || any_window_covers_monitor();
    // SAFETY: SYSTEM_POWER_STATUS 全为零是合法的初始值
    let mut power: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: power 是输出参数，调用期间有效
    let battery = if unsafe { GetSystemPowerStatus(&mut power) } != 0 {
        power.ACLineStatus == 0
    } else {
        log::debug!("无法查询电源状态：{}", io::Error::last_os_error());
        false
    };
    Sensed {
        fullscreen,
        battery,
        free_gb: free_gb(dir),
        ..Sensed::default()
    }
}

/// `dir` 所在卷对当前用户可用的空间（GB）；查询失败时为空
pub(crate) fn free_gb(dir: &Path) -> Option<f64> {
    let path = wide(dir);
    let mut free = 0u64;
    // SAFETY: path 是以 0 结尾的 UTF-16 字符串；free 是输出参数，另两个输出参数允许为空
    let ok = unsafe { GetDiskFreeSpaceExW(path.as_ptr(), &mut free, null_mut(), null_mut()) };
    if ok != 0 {
        return Some(free as f64 / (1u64 << 30) as f64);
    }
    log::debug!(
        "无法查询 {} 的剩余空间：{}",
        dir.display(),
        io::Error::last_os_error()
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_compares_rects() {
        let r = |left, top, right, bottom| RECT {
            left,
            top,
            right,
            bottom,
        };
        // 副屏在主屏右侧
        let mon = r(2560, 0, 4480, 1080);
        assert!(covers(&r(2560, 0, 4480, 1080), &mon));
        assert!(covers(&r(2550, -10, 4490, 1090), &mon));
        // 最大化窗口的可见边界是工作区，不含任务栏
        assert!(!covers(&r(2560, 0, 4480, 1032), &mon));
        assert!(!covers(&r(2561, 0, 4480, 1080), &mon));
        // 盖满另一个显示器的窗口
        assert!(!covers(&r(0, 0, 2560, 1440), &mon));
    }

    #[test]
    fn overlay_styles() {
        assert!(overlay(WS_EX_TRANSPARENT | WS_EX_LAYERED));
        assert!(overlay(WS_EX_NOACTIVATE));
        // 只有其中一个不算点击穿透
        assert!(!overlay(WS_EX_LAYERED));
        assert!(!overlay(WS_EX_TRANSPARENT));
        assert!(!overlay(0));
    }

    #[test]
    fn framed_styles() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{WS_OVERLAPPEDWINDOW, WS_POPUP};
        // 普通窗口（含最大化）有 WS_THICKFRAME
        assert!(framed(WS_OVERLAPPEDWINDOW, 0));
        assert!(framed(WS_DLGFRAME, 0));
        assert!(framed(WS_POPUP, WS_EX_WINDOWEDGE));
        assert!(framed(WS_POPUP, WS_EX_TOOLWINDOW));
        // 无边框弹出窗口（游戏、全屏视频）
        assert!(!framed(WS_POPUP, 0));
    }

    #[test]
    fn sense_reports_free_space() {
        let s = sense(&std::env::temp_dir());
        assert!(s.free_gb.is_some_and(|gb| gb > 0.0), "{s:?}");
    }
}
