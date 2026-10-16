//! Windows、Android 以外平台的占位实现：不调用 ffmpeg，一律返回 `Unsupported`（Android 的音视频用系统解码器，见 `android.rs`）。

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
