//! Windows 平台接口：驱动器类型、已知文件夹、
//! 带进度和取消的文件复制、等待旧进程退出、迁移锁文件，以及启动时（还没有任何窗口）显示迁移进度和询问用户的 TaskDialog
use std::ffi::{OsStr, OsString, c_void};
use std::fs;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM, S_FALSE, S_OK, WAIT_OBJECT_0, WPARAM,
};
use windows_sys::Win32::Storage::FileSystem::{
    BusTypeUsb, COPY_FILE_FAIL_IF_EXISTS, COPYPROGRESSROUTINE_PROGRESS, CopyFileExW, CreateFileW,
    FILE_SHARE_READ, FILE_SHARE_WRITE, GetDriveTypeW, LPPROGRESS_ROUTINE_CALLBACK_REASON,
    OPEN_EXISTING, PROGRESS_CONTINUE,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR,
    STORAGE_PROPERTY_QUERY, StorageDeviceProperty,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    QueryFullProcessImageNameW, WaitForMultipleObjects,
};
use windows_sys::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOG_NOTIFICATIONS, TASKDIALOGCONFIG, TD_WARNING_ICON,
    TDCBF_CANCEL_BUTTON, TDE_CONTENT, TDF_CALLBACK_TIMER, TDF_SHOW_PROGRESS_BAR, TDM_CLICK_BUTTON,
    TDM_ENABLE_BUTTON, TDM_SET_PROGRESS_BAR_POS, TDM_SET_PROGRESS_BAR_RANGE,
    TDM_UPDATE_ELEMENT_TEXT, TDN_BUTTON_CLICKED, TDN_CREATED, TDN_TIMER,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures,
    FOLDERID_Public, FOLDERID_Videos, SHGetKnownFolderPath,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{IDCANCEL, PostMessageW, SendMessageW};
use windows_sys::core::{BOOL, HRESULT, PWSTR};

use crate::migrate::{COMMITTED, Failure, Progress, cancelled};
use crate::same_path;

/// 由界面进程启动、与它位于同一目录的核心进程
const CORE_EXE: &str = "rummage-core.exe";

fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

pub fn is_remote_drive(letter: u8) -> bool {
    let root = wide(format!("{}:\\", letter as char));
    // SAFETY: root 是以 0 结尾的 UTF-16 字符串，调用期间有效。4 为 DRIVE_REMOTE
    unsafe { GetDriveTypeW(root.as_ptr()) == 4 }
}

/// 盘符对应的设备是否为可移动设备：GetDriveTypeW 为 DRIVE_REMOVABLE（U 盘），或者总线类型为 USB
/// （报告为固定磁盘的 USB 移动硬盘、移动固态硬盘）。查询失败一律按不可移动处理
pub fn is_removable_drive(letter: u8) -> bool {
    let root = wide(format!("{}:\\", letter as char));
    // SAFETY: root 是以 0 结尾的 UTF-16 字符串，调用期间有效。2 为 DRIVE_REMOVABLE
    if unsafe { GetDriveTypeW(root.as_ptr()) } == 2 {
        return true;
    }
    // 打开卷设备只为查询属性，不需要读写权限
    let volume = wide(format!("\\\\.\\{}:", letter as char));
    // SAFETY: volume 是以 0 结尾的 UTF-16 字符串；安全属性与模板句柄允许为空
    let handle = unsafe {
        CreateFileW(
            volume.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return false;
    }
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    // 描述符后面跟着厂商、型号等字符串，留足空间；这里只读取固定部分的 BusType
    #[repr(C)]
    struct Output {
        desc: STORAGE_DEVICE_DESCRIPTOR,
        extra: [u8; 1024],
    }
    // SAFETY: Output 只含整数和字节数组，全零是合法值
    let mut out: Output = unsafe { std::mem::zeroed() };
    let mut returned = 0u32;
    // SAFETY: handle 有效；输入、输出缓冲区的指针与长度一致，调用期间有效
    let ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            &query as *const STORAGE_PROPERTY_QUERY as *const c_void,
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            &mut out as *mut Output as *mut c_void,
            size_of::<Output>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    // SAFETY: handle 由 CreateFileW 打开，只关闭一次
    unsafe { CloseHandle(handle) };
    let filled = std::mem::offset_of!(STORAGE_DEVICE_DESCRIPTOR, BusType) + 4;
    ok != 0 && returned as usize >= filled && out.desc.BusType == BusTypeUsb
}

/// 下载、图片、桌面、文档、视频、音乐、公用文件夹的实际位置（可能被用户移动到其他盘）；查询失败的跳过
pub fn known_dirs() -> Vec<PathBuf> {
    [
        FOLDERID_Downloads,
        FOLDERID_Pictures,
        FOLDERID_Desktop,
        FOLDERID_Documents,
        FOLDERID_Videos,
        FOLDERID_Music,
        FOLDERID_Public,
    ]
    .iter()
    .filter_map(|id| {
        let mut raw: PWSTR = std::ptr::null_mut();
        // SAFETY: id 指向有效的 GUID，令牌为空表示当前用户；raw 由系统分配
        let hr = unsafe { SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut raw) };
        let path = (hr == S_OK && !raw.is_null()).then(|| {
            // SAFETY: 成功时 raw 是以 0 结尾的 UTF-16 字符串，在 CoTaskMemFree 之前有效
            unsafe {
                let len = (0..).take_while(|&i| *raw.add(i) != 0).count();
                PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(raw, len)))
            }
        });
        // SAFETY: 无论成功与否都需释放 raw，释放空指针无影响
        unsafe { CoTaskMemFree(raw as *const c_void) };
        path
    })
    .collect()
}

/// 复制单个文件，目标已存在时报错；复制过程中按字节更新进度，单个大文件也不会长时间停在同一进度
pub fn copy_file(src: &Path, dst: &Path, progress: &Progress) -> io::Result<()> {
    struct Ctx<'a> {
        progress: &'a Progress,
        start: u64,
    }

    unsafe extern "system" fn report(
        _total: i64,
        transferred: i64,
        _stream_size: i64,
        _stream_transferred: i64,
        _stream: u32,
        _reason: LPPROGRESS_ROUTINE_CALLBACK_REASON,
        _src: HANDLE,
        _dst: HANDLE,
        data: *const c_void,
    ) -> COPYPROGRESSROUTINE_PROGRESS {
        // SAFETY: data 是下面传入的 &Ctx，CopyFileExW 返回前一直有效
        let ctx = unsafe { &*(data as *const Ctx) };
        ctx.progress
            .done
            .store(ctx.start + transferred as u64, Ordering::Relaxed);
        PROGRESS_CONTINUE
    }

    let ctx = Ctx {
        progress,
        start: progress.done.load(Ordering::Relaxed),
    };
    let (src, dst) = (wide(src), wide(dst));
    // SAFETY: 路径是以 0 结尾的 UTF-16 字符串；ctx 与取消标志（AtomicI32 与 BOOL 布局相同）在调用期间有效，
    // 回调只在本线程的 CopyFileExW 内部被调用
    let ok = unsafe {
        CopyFileExW(
            src.as_ptr(),
            dst.as_ptr(),
            Some(report),
            &ctx as *const Ctx as *const c_void,
            progress.cancel.as_ptr(),
            COPY_FILE_FAIL_IF_EXISTS,
        )
    };
    if ok != 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    // ERROR_REQUEST_ABORTED：已取消，CopyFileExW 会删除未复制完的目标文件
    if err.raw_os_error() == Some(1235) {
        return Err(cancelled());
    }
    Err(err)
}

/// 打开进程句柄（只为等待它退出）；打不开说明已经退出
fn open_process(pid: u32, access: u32) -> Option<HANDLE> {
    // SAFETY: 只传入数值参数；失败时返回空句柄
    let handle = unsafe { OpenProcess(access, 0, pid) };
    (!handle.is_null()).then_some(handle)
}

/// 与本程序位于同一目录的 rummage-core.exe（开发版在 target/debug/，发布版在安装目录）
fn core_processes() -> Vec<HANDLE> {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    else {
        return Vec::new();
    };
    // SAFETY: 只传入数值参数；失败时返回 INVALID_HANDLE_VALUE
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    // SAFETY: PROCESSENTRY32W 只含整数和数组，全零是合法值
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut handles = Vec::new();
    // SAFETY: snapshot 有效，entry.dwSize 已按要求设置
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    let own = std::process::id();
    while more {
        let len = entry
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.szExeFile.len());
        let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
        // 排除自身
        if entry.th32ProcessID != own
            && name.eq_ignore_ascii_case(CORE_EXE)
            && let Some(handle) = open_process(
                entry.th32ProcessID,
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            )
        {
            let mut buf = vec![0u16; 32768];
            let mut size = buf.len() as u32;
            // SAFETY: handle 带查询权限；buf 的长度与 size 一致
            let ok = unsafe {
                QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut size)
            } != 0;
            let image = PathBuf::from(OsString::from_wide(&buf[..size as usize]));
            if ok && image.parent().is_some_and(|parent| same_path(parent, &dir)) {
                handles.push(handle);
            } else {
                // SAFETY: handle 由 OpenProcess 打开，只关闭一次
                unsafe { CloseHandle(handle) };
            }
        }
        // SAFETY: 同上
        more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    // SAFETY: snapshot 由 CreateToolhelp32Snapshot 打开，只关闭一次
    unsafe { CloseHandle(snapshot) };
    handles
}

/// 最多等待 30 秒，直到 handles 中的进程全部退出；返回是否全部退出，用户取消时返回错误。结束时关闭全部句柄
fn wait_exit(handles: Vec<HANDLE>, progress: &Progress) -> Result<bool, Failure> {
    if handles.is_empty() {
        return Ok(true);
    }
    // WaitForMultipleObjects 最多等待 64 个；同目录的核心进程正常只有一个
    let waited = &handles[..handles.len().min(64)];
    let mut exited = false;
    for _ in 0..300 {
        // SAFETY: waited 中都是有效的进程句柄，数量与指针一致
        let result =
            unsafe { WaitForMultipleObjects(waited.len() as u32, waited.as_ptr(), 1, 100) };
        if result == WAIT_OBJECT_0 {
            exited = true;
            break;
        }
        if progress.cancelled() {
            break;
        }
    }
    for handle in &handles {
        // SAFETY: 每个句柄由 OpenProcess 打开，只关闭一次
        unsafe { CloseHandle(*handle) };
    }
    if progress.cancelled() {
        return Err(Failure::cancelled());
    }
    Ok(exited)
}

/// 先后等待发起更改的旧进程、同目录的核心进程退出（各自计时），再等 WebView2 的浏览器进程释放 lockfile
/// （它们在主程序退出后才陆续退出）
pub fn wait_previous(pid: Option<u32>, from: &Path, progress: &Progress) -> Result<(), Failure> {
    let previous = pid.and_then(|pid| open_process(pid, PROCESS_SYNCHRONIZE));
    if !wait_exit(previous.into_iter().collect(), progress)? {
        log::warn!("原来的 Rummage 30 秒内没有退出");
        return Err(Failure::new(
            "原来的 Rummage 未能及时退出，请在任务管理器中确认它已关闭后再重新启动 Rummage。",
        ));
    }
    if !wait_exit(core_processes(), progress)? {
        log::warn!("rummage-core.exe 30 秒内没有退出");
        return Err(Failure::new(
            "Rummage 仍在后台运行（可能由其他程序启动），请在任务管理器中结束“rummage-core”后再重新启动 Rummage。",
        ));
    }
    let lockfile = from.join("webview").join("EBWebView").join("lockfile");
    for _ in 0..150 {
        if progress.cancelled() {
            return Err(Failure::cancelled());
        }
        match fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&lockfile)
        {
            // ERROR_SHARING_VIOLATION
            Err(err) if err.raw_os_error() == Some(32) => {
                std::thread::sleep(Duration::from_millis(100))
            }
            _ => return Ok(()),
        }
    }
    log::warn!("WebView2 15 秒内没有释放原数据目录");
    Err(Failure::new(
        "原数据目录仍被其他程序占用，请关闭所有 Rummage 窗口后再重新启动 Rummage。",
    ))
}

/// 迁移期间独占打开锁文件，关闭时自动删除；已被其他实例打开时为 None
pub fn lock(path: &Path) -> io::Result<Option<fs::File>> {
    // 0x0400_0000 为 FILE_FLAG_DELETE_ON_CLOSE
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .custom_flags(0x0400_0000)
        .open(path);
    match file {
        Ok(file) => Ok(Some(file)),
        // ERROR_SHARING_VIOLATION
        Err(err) if err.raw_os_error() == Some(32) => Ok(None),
        Err(err) => Err(err),
    }
}

const TITLE: &str = "Rummage";

fn size(bytes: u64) -> String {
    let mb = bytes as f64 / 1024.0 / 1024.0;
    if mb >= 1024.0 {
        return format!("{:.2} GB", mb / 1024.0);
    }
    format!("{mb:.1} MB")
}

type TaskDialogIndirect =
    unsafe extern "system" fn(*const TASKDIALOGCONFIG, *mut i32, *mut i32, *mut BOOL) -> HRESULT;

/// TaskDialogIndirect 只在 6.0 版通用控件中提供（tauri-build 嵌入的清单已声明），动态查找：
/// 静态导入时，没有清单的程序（例如单元测试）会因找不到入口点而无法启动
fn task_dialog() -> Option<TaskDialogIndirect> {
    let name = wide("comctl32.dll");
    // SAFETY: name 是以 0 结尾的 UTF-16 字符串；函数名是以 0 结尾的 C 字符串
    let proc = unsafe {
        let lib = LoadLibraryW(name.as_ptr());
        if lib.is_null() {
            return None;
        }
        GetProcAddress(lib, c"TaskDialogIndirect".as_ptr().cast())
    }?;
    // 窗口尚未创建，tao 还没设置 DPI 感知；先设置，避免对话框在高 DPI 屏幕上模糊
    // SAFETY: 只传入常量；已设置过时调用失败，无副作用
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    // SAFETY: TaskDialogIndirect 的签名与 TaskDialogIndirect 类型一致
    Some(unsafe {
        std::mem::transmute::<unsafe extern "system" fn() -> isize, TaskDialogIndirect>(proc)
    })
}

fn set_content(hwnd: HWND, text: &str) {
    let text = wide(text);
    // SAFETY: hwnd 是回调收到的对话框句柄；text 在同步的 SendMessageW 期间有效
    unsafe {
        SendMessageW(
            hwnd,
            TDM_UPDATE_ELEMENT_TEXT as u32,
            TDE_CONTENT as usize,
            text.as_ptr() as isize,
        )
    };
}

unsafe extern "system" fn callback(
    hwnd: HWND,
    msg: u32,
    _wparam: WPARAM,
    _lparam: LPARAM,
    data: isize,
) -> HRESULT {
    // SAFETY: data 是 show_progress 传入的 &Progress，对话框关闭前一直有效
    let progress = unsafe { &*(data as *const Progress) };
    match msg as TASKDIALOG_NOTIFICATIONS {
        // 进度按千分比显示
        // SAFETY: hwnd 是本对话框的句柄
        TDN_CREATED => unsafe {
            SendMessageW(hwnd, TDM_SET_PROGRESS_BAR_RANGE as u32, 0, 1000 << 16);
        },
        TDN_TIMER => {
            if progress.finished.load(Ordering::Acquire) {
                // SAFETY: 同上
                unsafe { PostMessageW(hwnd, TDM_CLICK_BUTTON as u32, IDCANCEL as usize, 0) };
                return S_OK;
            }
            let state = progress.cancel.load(Ordering::Relaxed);
            if state == COMMITTED {
                // SAFETY: 同上
                unsafe { SendMessageW(hwnd, TDM_ENABLE_BUTTON as u32, IDCANCEL as usize, 0) };
            }
            let (done, total) = (
                progress.done.load(Ordering::Relaxed),
                progress.total.load(Ordering::Relaxed),
            );
            if state == 1 || total == 0 {
                return S_OK;
            }
            let permille = (done.min(total) * 1000 / total) as usize;
            // SAFETY: 同上
            unsafe { SendMessageW(hwnd, TDM_SET_PROGRESS_BAR_POS as u32, permille, 0) };
            let text = format!(
                "已复制 {} / {}（{}%）",
                size(done),
                size(total),
                permille / 10
            );
            set_content(hwnd, &text);
        }
        TDN_BUTTON_CLICKED => {
            if progress.finished.load(Ordering::Acquire) {
                return S_OK;
            }
            // 已开始切换时不能再取消；否则等迁移线程删除已复制的内容，结束后再关闭
            // SAFETY: 同上
            unsafe { SendMessageW(hwnd, TDM_ENABLE_BUTTON as u32, IDCANCEL as usize, 0) };
            if progress
                .cancel
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                set_content(hwnd, "正在取消");
            }
            return S_FALSE;
        }
        _ => {}
    }
    S_OK
}

/// 显示迁移进度框，迁移结束后自动关闭；无法显示时直接返回，由调用方继续等待迁移结束
pub fn show_progress(progress: &Progress) {
    let Some(task_dialog) = task_dialog() else {
        return;
    };
    let title = wide(TITLE);
    let heading = wide("正在更改数据目录");
    let content = wide("正在准备");
    let footer = wide("完成后自动打开。");
    let config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        dwFlags: TDF_SHOW_PROGRESS_BAR | TDF_CALLBACK_TIMER,
        dwCommonButtons: TDCBF_CANCEL_BUTTON,
        pszWindowTitle: title.as_ptr(),
        pszMainInstruction: heading.as_ptr(),
        pszContent: content.as_ptr(),
        pszFooter: footer.as_ptr(),
        pfCallback: Some(callback),
        lpCallbackData: progress as *const Progress as isize,
        ..Default::default()
    };
    // SAFETY: config 中的字符串与 progress 在对话框关闭（函数返回）前一直有效；输出参数允许为空
    let result = unsafe {
        task_dialog(
            &config,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result != S_OK {
        log::warn!("无法显示迁移进度框：0x{result:08X}");
    }
}

/// 能否显示 TaskDialog；不能时 choose 总是返回 None
pub fn can_choose() -> bool {
    task_dialog().is_some()
}

/// 显示带自定义按钮的提示，返回所点按钮的序号（从 0 开始）；关闭对话框或无法显示时返回 None
pub fn choose(heading: &str, content: &str, footer: &str, buttons: &[&str]) -> Option<usize> {
    let task_dialog = task_dialog()?;
    let (title, heading, content, footer) =
        (wide(TITLE), wide(heading), wide(content), wide(footer));
    let labels: Vec<Vec<u16>> = buttons.iter().map(wide).collect();
    let items: Vec<TASKDIALOG_BUTTON> = labels
        .iter()
        .enumerate()
        .map(|(idx, label)| TASKDIALOG_BUTTON {
            nButtonID: 100 + idx as i32,
            pszButtonText: label.as_ptr(),
        })
        .collect();
    let mut config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        pszWindowTitle: title.as_ptr(),
        pszMainInstruction: heading.as_ptr(),
        pszContent: content.as_ptr(),
        pszFooter: footer.as_ptr(),
        cButtons: items.len() as u32,
        pButtons: items.as_ptr(),
        nDefaultButton: 100,
        ..Default::default()
    };
    config.Anonymous1.pszMainIcon = TD_WARNING_ICON;
    let mut clicked = 0;
    // SAFETY: config 中的字符串与按钮数组在对话框关闭（函数返回）前一直有效；clicked 可写，其余输出参数允许为空
    let result = unsafe {
        task_dialog(
            &config,
            &mut clicked,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result != S_OK {
        return None;
    }
    usize::try_from(clicked - 100)
        .ok()
        .filter(|idx| *idx < buttons.len())
}
