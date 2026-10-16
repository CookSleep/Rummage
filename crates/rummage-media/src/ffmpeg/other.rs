//! 非 Windows 平台的占位实现：不调用 ffmpeg，读取音频与视频一律返回 `Unsupported`（Android 以后用 MediaCodec）。

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use super::{MediaError, Run};

pub(super) fn run(
    _exe: &Path,
    _args: &[OsString],
    _timeout: Duration,
    _sink: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<Run, MediaError> {
    Err(MediaError::Unsupported)
}
