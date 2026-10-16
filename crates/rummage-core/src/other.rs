//! 非 Windows 平台的占位实现：只用于编译和测试。核心进程目前只在 Windows 上作为独立进程运行，
//! Android 上引擎在应用进程内运行，这里一律返回“不支持”；资源占用控制的检测与通知都不做
use std::convert::Infallible;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::resource::Sensed;
use crate::{BindError, Power, State};

pub type Stream = tokio::io::DuplexStream;

fn unsupported() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "当前平台不支持后台服务")
}

pub fn user_sid() -> io::Result<String> {
    Err(unsupported())
}

pub struct Listener(Infallible);

pub async fn bind(_name: &str) -> Result<Listener, BindError> {
    Err(unsupported().into())
}

pub async fn serve(listener: Listener, _state: Arc<State>) -> io::Result<Infallible> {
    match listener.0 {}
}

pub async fn open(_name: &str) -> io::Result<Stream> {
    Err(unsupported())
}

pub fn write_private(_path: &Path, _data: &[u8]) -> io::Result<()> {
    Err(unsupported())
}

pub fn verify_server(_pipe: &Stream) -> io::Result<u32> {
    Err(unsupported())
}

pub fn wait_exit(_pid: u32, _timeout: Duration) -> bool {
    true
}

pub fn process_memory(_pid: u32) -> io::Result<(u64, u64)> {
    Err(unsupported())
}

pub fn send_session_end(_pid: u32) -> io::Result<()> {
    Err(unsupported())
}

/// 大小为 `len` 的文件；常见的文件系统上 `set_len` 扩展出的部分本来就不占空间
#[cfg(test)]
pub fn sparse_file(path: &Path, len: u64) -> io::Result<()> {
    std::fs::File::create(path)?.set_len(len)
}

/// 没有会话结束通知，不做任何事
pub fn on_session_end(_on_end: Box<dyn Fn() + Send + Sync>) -> io::Result<()> {
    Ok(())
}

/// 没有电源事件通知，不做任何事
pub fn on_power(_on_power: Box<dyn Fn(Power) + Send + Sync>) -> io::Result<()> {
    Ok(())
}

/// 不调整优先级
pub fn lower_priority() -> io::Result<()> {
    Ok(())
}

/// 不检测：不按全屏、电池、磁盘空间暂停
pub(crate) fn sense(_dir: &Path) -> Sensed {
    Sensed::default()
}

/// 不查询剩余空间
pub(crate) fn free_gb(_dir: &Path) -> Option<f64> {
    None
}
