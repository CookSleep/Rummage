//! 视频命中时刻的一帧静态画面：结果显示时才取，不在建索引时为每个窗口存图。
//!
//! - `-ss` 放在 `-i` 之前，按关键帧快速定位后解码到该时刻，取一帧；ffmpeg 内缩到不超过缩略图宽度，
//!   以 PNG 经管道读出（PNG 编码器是 ffmpeg 自带的，LGPL 构建也有），再与缩略图一样编码为 WebP。
//! - 软件解码：只取一帧，硬件解码的初始化反而更慢；按旋转标记转正（ffmpeg 默认）。
//! - **调用方必须先确认不是网盘占位文件**（`rummage_index::ensure_local`）。

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use crate::ffmpeg::{self, MediaError, input, tool};
use crate::{Rgb, THUMB_WIDTH};

/// 取 `ms` 毫秒处的一帧，宽度不超过 `THUMB_WIDTH`；时刻超出视频末尾时 ffmpeg 不输出画面，返回 `NoVideo`
pub fn frame_at(
    ffmpeg_dir: &Path,
    path: &Path,
    ms: u64,
    timeout: Duration,
) -> Result<Rgb, MediaError> {
    let exe = tool(ffmpeg_dir, "ffmpeg")?;
    let seek = format!("{}.{:03}", ms / 1000, ms % 1000);
    let scale = format!("scale='min({THUMB_WIDTH},iw)':-2");
    let mut args: Vec<OsString> = ["-nostdin", "-hide_banner", "-loglevel", "error", "-ss"]
        .iter()
        .map(OsString::from)
        .collect();
    args.push(OsString::from(seek));
    args.push(OsString::from("-i"));
    args.push(input(path));
    args.extend(
        [
            "-map",
            "0:V:0",
            "-an",
            "-sn",
            "-dn",
            "-frames:v",
            "1",
            "-vf",
            &scale,
            "-c:v",
            "png",
            "-f",
            "image2pipe",
            "-",
        ]
        .map(OsString::from),
    );
    let mut out = Vec::new();
    let run = ffmpeg::run(&exe, &args, timeout, &mut |b| {
        out.extend_from_slice(b);
        true
    })?;
    ffmpeg::check(run, timeout, path, "视频")?;
    if out.is_empty() {
        return Err(MediaError::NoVideo);
    }
    let img = image::load_from_memory_with_format(&out, image::ImageFormat::Png)
        .map_err(|err| {
            log::warn!("ffmpeg 从“{}”取出的画面无法解码：{err}", path.display());
            MediaError::Decode { what: "视频" }
        })?
        .to_rgb8();
    Ok(Rgb {
        width: img.width(),
        height: img.height(),
        data: img.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffmpeg::tests::ffmpeg_dir;

    #[test]
    fn frame_of_generated_video() {
        let Some(dir) = ffmpeg_dir() else {
            return;
        };
        let tmp = std::env::temp_dir().join(format!("rummage-frame-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let video = tmp.join("测试 视频.mp4");
        // 3 秒、640×360 的测试画面（MPEG-4 Part 2，LGPL 构建自带的编码器）
        let ok = std::process::Command::new(dir.join("ffmpeg.exe"))
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc=duration=3:size=640x360:rate=10")
            .args(["-c:v", "mpeg4"])
            .arg(&video)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let img = frame_at(&dir, &video, 1_500, Duration::from_secs(20)).unwrap();
        assert_eq!((img.width, img.height), (THUMB_WIDTH, 270));
        assert_eq!(img.data.len(), (img.width * img.height * 3) as usize);
        // 超出末尾
        let err = frame_at(&dir, &video, 60_000, Duration::from_secs(20)).unwrap_err();
        assert!(matches!(err, MediaError::NoVideo), "{err}");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
