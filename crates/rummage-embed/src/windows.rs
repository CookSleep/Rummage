//! Windows 平台接口：推理子进程的 Job Object 与 stdout 保护。
//! - 作业设 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`（监督方退出、句柄关闭时子进程一起结束）和
//!   `JOB_OBJECT_LIMIT_JOB_MEMORY`（整个作业的已提交内存上限，超出时分配失败）。
//!   子进程先启动再放进作业；子进程要等父进程在放进作业之后发来 `Start` 帧才加载运行库（见 `frame`），
//!   它之后启动的子进程自动属于同一作业。
//! - 子进程是控制台程序，以 `CREATE_NO_WINDOW` 启动，避免从无控制台的核心进程拉起时弹出窗口。
//! - stdout 保护：推理子进程先复制一份 stdout 句柄专给协议用，再把进程的标准输出（Win32 的 `STD_OUTPUT_HANDLE`
//!   与 CRT 的 fd 1）都指向 stderr，第三方组件（llama.cpp、ggml、驱动）写 stdout 也不会污染协议。
//! - `set_runtime_env`：推理子进程在加载运行库之前设置 `GGML_VK_VISIBLE_DEVICES`，用 UCRT 的 `_putenv_s`（见该函数）。
//! - 自检用：
//!   - `driver_version`：用 DXGI 枚举适配器，以 `Description` 与 ggml 的设备描述相等来匹配，
//!     再用 `CheckInterfaceSupport(IDXGIDevice)` 取用户模式驱动版本。只查询属性，不创建设备。
//!     `adapters` 列出全部适配器的这些属性，诊断包也用它。
//!   - `driver_events`：用 `EvtQuery` 查 System 通道中最近一段时间的显卡驱动事件：Display 的 4101
//!     （“显示驱动程序停止响应并已恢复”，各厂商通用），以及 NVIDIA（nvlddmkm）、Intel（igfx、igfxn）、
//!     AMD（amdkmdag、amdwddmg）内核驱动的警告及以上级别事件（Level ≤ 3）。
//!     时间窗口用 `timediff(@SystemTime)`（距现在的毫秒数），
//!     不必换算日期。查询本身失败时返回错误，不能当作没有事件放行。

use std::ffi::{CStr, c_char, c_int};
use std::fs::File;
use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::ptr::null;
use std::time::Duration;

use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIDevice, IDXGIFactory1};
use windows::core::Interface;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_ITEMS, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
};
use windows_sys::Win32::System::EventLog::{
    EVT_HANDLE, EvtClose, EvtNext, EvtQuery, EvtQueryChannelPath, EvtRender, EvtRenderEventXml,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::Adapter;

pub struct Job(HANDLE);

// SAFETY: 作业句柄是内核对象句柄，可以在任意线程使用和关闭；`Job` 独占它，关闭只在 Drop 中发生一次。
unsafe impl Send for Job {}
// SAFETY: `TerminateJobObject` 可以在多个线程上对同一作业句柄并发调用；共享时只调用 `kill`，
// 关闭仍只在最后一个持有者 Drop 时发生一次。
unsafe impl Sync for Job {}

impl Job {
    pub fn new(mem_bytes: u64) -> io::Result<Self> {
        // SAFETY: 不带安全属性和名字；失败时返回空句柄。
        let handle = unsafe { CreateJobObjectW(null(), null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Job(handle);
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_JOB_MEMORY;
        info.JobMemoryLimit = usize::try_from(mem_bytes).unwrap_or(usize::MAX);
        // SAFETY: 句柄有效；指针与长度描述同一个结构体，调用期间有效。
        let ok = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    pub fn assign(&self, child: &Child) -> io::Result<()> {
        // SAFETY: 作业句柄有效；进程句柄由 `Child` 持有，调用期间有效。
        let ok = unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle()) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// 结束作业中的全部进程。
    pub fn kill(&self) {
        // SAFETY: 作业句柄有效。失败（进程已全部退出）时无需处理。
        unsafe { TerminateJobObject(self.0, 1) };
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: 句柄有效且只关闭一次；作业设了 KILL_ON_JOB_CLOSE，关闭即结束其中的进程。
        unsafe { CloseHandle(self.0) };
    }
}

pub fn hide_window(cmd: &mut Command) {
    cmd.creation_flags(CREATE_NO_WINDOW);
}

// MSVC / MinGW 的 C 运行库函数；llama.cpp 等 DLL 与本进程共用同一个 UCRT，fd 表与环境变量表是进程级的。
unsafe extern "C" {
    fn _dup2(src: c_int, dst: c_int) -> c_int;
    fn _putenv_s(name: *const c_char, value: *const c_char) -> c_int;
}

/// 设置之后加载的运行库能读到的环境变量。ggml 用 C 运行库的 `getenv` 读取，它读的是 UCRT 启动时复制的环境表，
/// 看不到 `SetEnvironmentVariableW`（`std::env::set_var`）的修改；`_putenv_s` 同时改 UCRT 的表和进程环境。
/// 只在推理子进程加载运行库之前、还没有其他线程时调用。
pub fn set_runtime_env(name: &CStr, value: &CStr) -> io::Result<()> {
    // SAFETY: 两个参数都是以空字符结尾的字符串；调用时进程中没有其他线程读写环境变量（见上）。
    if unsafe { _putenv_s(name.as_ptr(), value.as_ptr()) } != 0 {
        return Err(io::Error::other("无法设置环境变量"));
    }
    Ok(())
}

/// 返回专给协议用的 stdout 副本，并把进程的标准输出重定向到 stderr。只在推理子进程启动时调用一次。
pub fn protocol_stdout() -> io::Result<File> {
    let out = File::from(io::stdout().as_handle().try_clone_to_owned()?);
    // SAFETY: 只读取并设置本进程的标准句柄；stderr 句柄由进程持有，不会被关闭。
    unsafe {
        let err = GetStdHandle(STD_ERROR_HANDLE);
        if err.is_null() || err == INVALID_HANDLE_VALUE || SetStdHandle(STD_OUTPUT_HANDLE, err) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    // SAFETY: fd 1、2 是 CRT 启动时按标准句柄建立的；_dup2 只改 fd 表，失败时返回 -1。
    if unsafe { _dup2(2, 1) } != 0 {
        return Err(io::Error::other("无法把 C 运行库的 stdout 重定向到 stderr"));
    }
    Ok(out)
}

/// 与 `description` 相同的第一个 DXGI 适配器的用户模式驱动版本（如 `32.0.15.6094`）；找不到或查询失败时为空。
pub fn driver_version(description: &str) -> Option<String> {
    adapters()
        .into_iter()
        .find(|a| a.description == description)?
        .driver
}

/// 全部 DXGI 适配器（含系统的软件适配器）；创建工厂失败时为空。
pub fn adapters() -> Vec<Adapter> {
    let mut out = Vec::new();
    // SAFETY: 只创建 DXGI 工厂并读取适配器描述与驱动版本，接口由 windows crate 的智能指针管理引用计数。
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
            return out;
        };
        let mut i = 0;
        // 越界时 EnumAdapters1 返回 DXGI_ERROR_NOT_FOUND。
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            i += 1;
            let Ok(desc) = adapter.GetDesc1() else {
                continue;
            };
            let len = desc
                .Description
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(desc.Description.len());
            out.push(Adapter {
                description: String::from_utf16_lossy(&desc.Description[..len]),
                vendor_id: desc.VendorId,
                device_id: desc.DeviceId,
                vram_mb: desc.DedicatedVideoMemory as u64 / (1024 * 1024),
                driver: adapter
                    .CheckInterfaceSupport(&IDXGIDevice::IID)
                    .ok()
                    .map(format_umd_version),
            });
        }
    }
    out
}

/// UMD 版本按 16 位一段拆成四段。
fn format_umd_version(v: i64) -> String {
    let v = v as u64;
    format!(
        "{}.{}.{}.{}",
        v >> 48,
        (v >> 32) & 0xffff,
        (v >> 16) & 0xffff,
        v & 0xffff
    )
}

/// 查最近 `window` 内 System 通道中显卡驱动事件的 XPath 查询。
fn driver_events_query(window: Duration) -> String {
    format!(
        "*[System[((Provider[@Name='Display'] and EventID=4101) or (Provider[@Name='nvlddmkm' or @Name='igfx' or @Name='igfxn' or @Name='amdkmdag' or @Name='amdwddmg'] and Level<=3)) and TimeCreated[timediff(@SystemTime) <= {}]]]",
        window.as_millis()
    )
}

/// 从事件的 XML 中取出 `start` 之后到 `end` 之前的文本，只用于生成说明。
fn xml_field<'a>(xml: &'a str, start: &str, end: char) -> &'a str {
    xml.split_once(start)
        .and_then(|(_, rest)| rest.split(end).next())
        .unwrap_or("?")
}

struct EvtHandle(EVT_HANDLE);

impl Drop for EvtHandle {
    fn drop(&mut self) {
        // SAFETY: 句柄由 EvtQuery / EvtNext 返回且非空，只关闭一次。
        unsafe { EvtClose(self.0) };
    }
}

fn render_xml(event: &EvtHandle) -> io::Result<String> {
    let mut buf: Vec<u16> = Vec::new();
    loop {
        let mut used = 0u32;
        let mut count = 0u32;
        // SAFETY: 缓冲区指针与字节数描述同一块内存；不够时返回 ERROR_INSUFFICIENT_BUFFER 并给出所需字节数。
        let ok = unsafe {
            EvtRender(
                0,
                event.0,
                EvtRenderEventXml,
                (buf.len() * 2) as u32,
                buf.as_mut_ptr().cast(),
                &mut used,
                &mut count,
            )
        };
        if ok != 0 {
            let len = (used as usize / 2).min(buf.len());
            return Ok(String::from_utf16_lossy(&buf[..len])
                .trim_end_matches('\0')
                .to_string());
        }
        // SAFETY: 只读取本线程的最后错误码。
        if unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
            return Err(io::Error::last_os_error());
        }
        buf = vec![0; (used as usize).div_ceil(2)];
    }
}

/// 最近 `window` 内的显卡驱动事件，每条写成“来源 事件 ID 时间”。
pub fn driver_events(window: Duration) -> io::Result<Vec<String>> {
    let channel: Vec<u16> = "System\0".encode_utf16().collect();
    let query: Vec<u16> = format!("{}\0", driver_events_query(window))
        .encode_utf16()
        .collect();
    // SAFETY: 两个字符串以 NUL 结尾，调用期间有效；会话为 0 表示本机。
    let handle = unsafe { EvtQuery(0, channel.as_ptr(), query.as_ptr(), EvtQueryChannelPath) };
    if handle == 0 {
        return Err(io::Error::last_os_error());
    }
    let results = EvtHandle(handle);
    let mut out = Vec::new();
    loop {
        let mut batch: [EVT_HANDLE; 16] = [0; 16];
        let mut n = 0u32;
        // SAFETY: 数组长度与传入的个数一致；本地查询，超时 1 秒足够。
        let ok = unsafe {
            EvtNext(
                results.0,
                batch.len() as u32,
                batch.as_mut_ptr(),
                1000,
                0,
                &mut n,
            )
        };
        if ok == 0 {
            // SAFETY: 只读取本线程的最后错误码。
            if unsafe { GetLastError() } == ERROR_NO_MORE_ITEMS {
                return Ok(out);
            }
            return Err(io::Error::last_os_error());
        }
        // 先全部包装，渲染出错提前返回时其余句柄也会关闭。
        let events: Vec<EvtHandle> = batch[..n as usize].iter().map(|&h| EvtHandle(h)).collect();
        for e in &events {
            let xml = render_xml(e)?;
            out.push(format!(
                "{} 事件 {}（{}）",
                xml_field(&xml, "Provider Name='", '\''),
                event_id(&xml),
                xml_field(&xml, "SystemTime='", '\''),
            ));
        }
    }
}

/// `<EventID Qualifiers='0'>4101</EventID>` 中的 4101。
fn event_id(xml: &str) -> &str {
    let tag = xml_field(xml, "<EventID", '<');
    tag.split_once('>').map_or("?", |(_, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_query_window() {
        assert_eq!(
            driver_events_query(Duration::from_millis(12_345)),
            "*[System[((Provider[@Name='Display'] and EventID=4101) or (Provider[@Name='nvlddmkm' or @Name='igfx' or @Name='igfxn' or @Name='amdkmdag' or @Name='amdwddmg'] and Level<=3)) and TimeCreated[timediff(@SystemTime) <= 12345]]]"
        );
        assert!(driver_events_query(Duration::from_secs(90)).ends_with("<= 90000]]]"));
    }

    #[test]
    fn events_query_accepted() {
        // 只检查查询语法被事件日志服务接受；窗口为 1 毫秒，通常没有事件。
        driver_events(Duration::from_millis(1)).unwrap();
    }

    #[test]
    fn parses_event_xml() {
        let xml = "<Event><System><Provider Name='Display'/><EventID Qualifiers='0'>4101</EventID>\
                   <TimeCreated SystemTime='2026-10-13T01:02:03.4567890Z'/></System></Event>";
        assert_eq!(xml_field(xml, "Provider Name='", '\''), "Display");
        assert_eq!(event_id(xml), "4101");
        assert_eq!(event_id("<EventID>153</EventID>"), "153");
        assert_eq!(xml_field("", "SystemTime='", '\''), "?");
    }

    #[test]
    fn umd_version() {
        let v: i64 = (32 << 48) | (15 << 16) | 6094;
        assert_eq!(format_umd_version(v), "32.0.15.6094");
    }
}
