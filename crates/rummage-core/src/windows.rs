//! Windows 平台接口：当前用户 SID、只授予当前用户的安全描述符（SDDL `O:<SID>D:P(A;;GA;;;<SID>)`）、
//! 命名管道的创建与接受连接、客户端打开管道并核对服务端进程的用户、等待进程退出、读取进程的内存占用，
//! 以及用同一安全描述符创建令牌文件。
//! 管道每个实例都带上安全描述符并拒绝远程客户端；接受一个连接后立即创建下一个实例，新客户端随时可以连上
//!
//! 关机、注销与电源事件的隐藏窗口见 `session`，进程优先级与暂停条件的检测见 `resource`。
//! 上次退出是否为崩溃（`crashed`）：查 Application 通道中 Windows 错误报告记下的“Application Error”事件 1000，
//! 出错程序为本程序；结束任务、断电、关机都没有这个事件。
use std::convert::Infallible;
use std::ffi::c_void;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::Path;
use std::ptr::null_mut;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    LocalFree, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    EqualSid, GetTokenInformation, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL};
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows_sys::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, WaitForSingleObject,
};
use windows_sys::core::PWSTR;

use crate::{BindError, State};

#[path = "windows/resource.rs"]
mod resource;
#[path = "windows/session.rs"]
mod session;

pub use resource::lower_priority;
pub(crate) use resource::{free_gb, sense};
pub use session::{on_power, on_session_end, send_session_end};

pub type Stream = NamedPipeClient;

fn wide(text: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

/// 进程令牌中的 TOKEN_USER；process 由调用方负责关闭
fn token_user(process: HANDLE) -> io::Result<Vec<u64>> {
    let mut token: HANDLE = null_mut();
    // SAFETY: process 是有效的进程句柄（或当前进程的伪句柄）；token 是输出参数，成功时得到需要关闭的句柄
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut len = 0u32;
    // SAFETY: 传空缓冲区只为取得所需长度，这次调用按约定返回失败，不写缓冲区
    unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len) };
    // 用 u64 作缓冲区元素，保证 TOKEN_USER 的对齐
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: buf 至少有 len 字节且对齐满足 TOKEN_USER；token 是上面打开的有效句柄
    let ok =
        unsafe { GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) };
    let err = io::Error::last_os_error();
    // SAFETY: token 是上面打开的有效句柄，之后不再使用
    unsafe { CloseHandle(token) };
    if ok == 0 {
        return Err(err);
    }
    Ok(buf)
}

/// buf 必须来自 `token_user`
fn sid_of(buf: &[u64]) -> PSID {
    // SAFETY: token_user 成功时 buf 开头是 TOKEN_USER，其中的 SID 指针指向 buf 内部，随 buf 存活
    unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid }
}

fn current_user() -> io::Result<Vec<u64>> {
    // SAFETY: GetCurrentProcess 返回的伪句柄总是有效，不需要关闭
    token_user(unsafe { GetCurrentProcess() })
}

pub fn user_sid() -> io::Result<String> {
    let user = current_user()?;
    let mut text: PWSTR = null_mut();
    // SAFETY: SID 有效（user 存活）；text 是输出参数，成功时由系统用 LocalAlloc 分配
    if unsafe { ConvertSidToStringSidW(sid_of(&user), &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: text 是以 0 结尾的 UTF-16 字符串，读完后用 LocalFree 释放且不再使用
    let sid = unsafe {
        let len = (0..).take_while(|&i| *text.add(i) != 0).count();
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        LocalFree(text.cast());
        sid
    };
    Ok(sid)
}

/// 核对管道服务端进程的用户是否为当前用户，是则返回其 PID。
/// 防止别的用户或程序抢先创建同名管道冒充核心、骗取令牌；任何一步失败都视为不可信
pub fn verify_server(pipe: &Stream) -> io::Result<u32> {
    let mut pid = 0u32;
    // SAFETY: pipe 持有有效的管道客户端句柄；pid 是输出参数
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: 只请求查询权限；返回的句柄在下面关闭
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let server = token_user(process);
    // SAFETY: process 是上面打开的有效句柄，之后不再使用
    unsafe { CloseHandle(process) };
    let (server, me) = (server?, current_user()?);
    // SAFETY: 两个 SID 都指向存活缓冲区中的有效 SID
    if unsafe { EqualSid(sid_of(&server), sid_of(&me)) } == 0 {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            "管道的服务端不属于当前用户",
        ));
    }
    Ok(pid)
}

/// 等待进程退出，超时返回 false；进程已不存在时返回 true
pub fn wait_exit(pid: u32, timeout: Duration) -> bool {
    // SAFETY: 只请求等待权限；返回的句柄在下面关闭
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if process.is_null() {
        return true;
    }
    let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
    // SAFETY: process 是上面打开的有效句柄
    let res = unsafe { WaitForSingleObject(process, ms) };
    // SAFETY: process 是上面打开的有效句柄，之后不再使用
    unsafe { CloseHandle(process) };
    res == WAIT_OBJECT_0
}

/// 进程的工作集与专用提交大小（字节），`bench` 测核心进程的空闲内存用
pub fn process_memory(pid: u32) -> io::Result<(u64, u64)> {
    // SAFETY: 只请求查询权限；返回的句柄在下面关闭
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut c = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: process 是上面打开的有效句柄；c 可写，大小与 cb 一致，按文档可以当作 PROCESS_MEMORY_COUNTERS 传入
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            process,
            (&raw mut c).cast::<PROCESS_MEMORY_COUNTERS>(),
            c.cb,
        )
    };
    let err = io::Error::last_os_error();
    // SAFETY: process 是上面打开的有效句柄，之后不再使用
    unsafe { CloseHandle(process) };
    if ok == 0 {
        return Err(err);
    }
    Ok((c.WorkingSetSize as u64, c.PrivateUsage as u64))
}

/// 只允许当前用户访问（`P` 表示不继承上级目录的权限）
struct Security {
    attrs: SECURITY_ATTRIBUTES,
}

// SAFETY: 安全描述符是本结构独占的一块堆内存，创建后只被系统调用读取，不会被修改，可以跨线程共享
unsafe impl Send for Security {}
// SAFETY: 同上
unsafe impl Sync for Security {}

impl Security {
    fn current_user() -> io::Result<Self> {
        let sddl = wide(format!("O:{sid}D:P(A;;GA;;;{sid})", sid = user_sid()?));
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: sddl 是以 0 结尾的 UTF-16 字符串；sd 是输出参数，由系统用 LocalAlloc 分配，在 Drop 中释放
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            attrs: SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd,
                bInheritHandle: 0,
            },
        })
    }
}

impl Drop for Security {
    fn drop(&mut self) {
        // SAFETY: 描述符由 ConvertStringSecurityDescriptorToSecurityDescriptorW 分配，只在这里释放一次
        unsafe { LocalFree(self.attrs.lpSecurityDescriptor) };
    }
}

fn create(name: &str, first: bool, sec: &Security) -> io::Result<NamedPipeServer> {
    // SAFETY: attrs 指向有效的 SECURITY_ATTRIBUTES，其中的描述符在 sec 存活期间有效；系统只在调用期间读取它
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(name, &sec.attrs as *const _ as *mut c_void)
    }
}

/// 已创建第一个实例、尚未开始接受连接的管道
pub struct Listener {
    name: String,
    sec: Security,
    next: NamedPipeServer,
}

/// 需要在 tokio 运行时中调用
pub async fn bind(name: &str) -> Result<Listener, BindError> {
    let sec = Security::current_user()?;
    match create(name, true, &sec) {
        Ok(next) => Ok(Listener {
            name: name.into(),
            sec,
            next,
        }),
        // 同名管道已存在时，FILE_FLAG_FIRST_PIPE_INSTANCE 使创建以拒绝访问失败；
        // 连上去核对服务端：是当前用户的进程就是已在运行的核心，否则名字被别人占用
        Err(err) if err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
            match open(name).await.and_then(|pipe| verify_server(&pipe)) {
                Ok(_) => Err(BindError::AlreadyRunning),
                Err(_) => Err(BindError::Occupied),
            }
        }
        Err(err) => Err(err.into()),
    }
}

/// 一直接受连接；只有无法创建新的管道实例时才返回
pub async fn serve(listener: Listener, state: Arc<State>) -> io::Result<Infallible> {
    let mut server = listener.next;
    loop {
        let res = server.connect().await;
        let next = create(&listener.name, false, &listener.sec)?;
        let conn = std::mem::replace(&mut server, next);
        match res {
            Ok(()) => {
                tokio::spawn(crate::server::handle(conn, state.clone()));
            }
            Err(err) => log::warn!("等待 IPC 连接失败：{err}"),
        }
    }
}

/// 管道不存在时返回 `NotFound`；所有实例都忙时短暂重试
pub async fn open(name: &str) -> io::Result<Stream> {
    let start = Instant::now();
    loop {
        match ClientOptions::new().open(name) {
            Err(err)
                if err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
                    && start.elapsed() < Duration::from_secs(2) => {}
            res => return res,
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 写入只授予当前用户的文件。文件已存在时创建不会应用新的安全描述符，所以先删除再新建
pub fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    let sec = Security::current_user()?;
    match fs::remove_file(path) {
        Err(err) if err.kind() != ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    let path = wide(path);
    // SAFETY: path 是以 0 结尾的 UTF-16 字符串；sec.attrs 在调用期间有效
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_WRITE,
            0,
            &sec.attrs,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: handle 是刚创建的有效文件句柄，交给 File 独占并负责关闭
    let mut file = unsafe { File::from_raw_handle(handle) };
    file.write_all(data)
}

/// 大小为 `len` 的稀疏文件，不实际占用磁盘空间；测试中造只核对大小的模型文件用
#[cfg(test)]
pub fn sparse_file(path: &Path, len: u64) -> io::Result<()> {
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;
    let f = File::create(path)?;
    let mut n = 0u32;
    // SAFETY: 句柄在 f 存活期间有效；FSCTL_SET_SPARSE 不带输入缓冲区时即设为稀疏，也没有输出；同步调用，不用 OVERLAPPED
    let ok = unsafe {
        DeviceIoControl(
            f.as_raw_handle(),
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            null_mut(),
            0,
            &mut n,
            null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    f.set_len(len)
}

/// 最近 `window` 内本程序是否崩溃过（见模块说明）
pub(crate) fn crashed(window: Duration) -> io::Result<bool> {
    let exe = std::env::current_exe()?;
    let name = exe
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let query = format!(
        "*[System[Provider[@Name='Application Error'] and EventID=1000 and TimeCreated[timediff(@SystemTime) <= {}]]]",
        window.as_millis()
    );
    let tag = format!("<data name='appname'>{name}</data>");
    Ok(rummage_embed::event_xml("Application", &query)?
        .iter()
        .any(|xml| xml.to_lowercase().contains(&tag)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 查询语句能在本机的系统日志上执行；测试程序本身没有崩溃过
    #[test]
    fn crashed_query_runs() {
        assert!(!crashed(Duration::from_secs(7 * 24 * 3600)).unwrap());
    }
}
