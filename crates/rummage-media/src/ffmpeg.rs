//! 运行 ffmpeg / ffprobe：音频（`audio`）与视频（`video`）共用。
//!
//! - ffmpeg / ffprobe 是 LGPL 构建的独立可执行文件，以子进程调用、经管道读出结果，不链接其库。
//!   子进程不弹控制台窗口，超过时限就结束它（`platform::run`）。路径前加 `file:`，文件名不会被当作其他协议。
//! - 只有 Windows 用 ffmpeg；其他平台（Android 以后用 MediaCodec）目前返回 `Unsupported`。

#[cfg(not(windows))]
#[path = "ffmpeg/other.rs"]
mod platform;
#[cfg(windows)]
#[path = "ffmpeg/windows.rs"]
mod platform;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

/// 打包与开发缓存中 ffmpeg 的版本目录，与 `scripts/fetch-ffmpeg.ps1` 的 `$Version` 一致
pub const FFMPEG_BUILD: &str = "n9.0.2-27-g7e93014c5b";

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("找不到 {0}，请重新安装 Rummage。")]
    MissingTool(String),
    #[error("无法启动 {tool}：{detail}")]
    Spawn { tool: String, detail: String },
    /// `what` 为“音频”或“视频”，下同
    #[error("读取{what}超过 {secs} 秒仍未完成，已放弃。")]
    Timeout { what: &'static str, secs: u64 },
    /// ffmpeg 的报错只写日志
    #[error("无法读取{what}，文件可能已损坏或格式不受支持。")]
    Decode { what: &'static str },
    #[error("文件中没有音轨。")]
    NoAudio,
    #[error("文件中没有视频画面。")]
    NoVideo,
    /// 设置项名称与参数表 `max_audio_min` 一致
    #[error("超过设置中的“音视频时长上限（分钟）”，不识别内容。")]
    TooLong,
    #[error("当前系统不支持识别音频和视频的内容。")]
    Unsupported,
}

pub(crate) fn tool(dir: &Path, name: &str) -> Result<PathBuf, MediaError> {
    let exe = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    if !exe.is_file() {
        return Err(MediaError::MissingTool(exe.display().to_string()));
    }
    Ok(exe)
}

/// 前缀：`subfile` 给出的不是文件路径，而是 ffmpeg 的输入地址
const SUBFILE: &str = "subfile,,";

/// 文件中字节范围 `[start, end)` 的内容（不压缩存放在文档 zip 中的音视频），
/// 作为 ffmpeg 的输入：用 `subfile` 协议直接读，不另存文件。返回值只能交给本模块的函数（`input` 认得它），不是真实路径
pub fn subfile(path: &Path, start: u64, end: u64) -> PathBuf {
    let mut s = OsString::from(format!("{SUBFILE}start,{start},end,{end},,:"));
    s.push(input(path));
    PathBuf::from(s)
}

/// `file:` 加路径，见模块说明；`subfile` 给出的地址原样使用
pub(crate) fn input(path: &Path) -> OsString {
    if path.as_os_str().to_string_lossy().starts_with(SUBFILE) {
        return path.as_os_str().to_owned();
    }
    let mut s = OsString::from("file:");
    s.push(path);
    s
}

/// 子进程的结果：退出是否成功、stderr 的末尾、是否因超时被结束
pub(crate) struct Run {
    pub ok: bool,
    pub stderr: String,
    pub timed_out: bool,
}

/// 运行 `exe`，stdout 逐块交给 `sink`；`sink` 返回否时结束子进程、不再读取
pub(crate) fn run(
    exe: &Path,
    args: &[OsString],
    timeout: Duration,
    sink: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<Run, MediaError> {
    platform::run(exe, args, timeout, sink)
}

/// stderr 的最后一行，写日志用
fn last_line(stderr: &str) -> String {
    stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("没有错误信息")
        .trim()
        .to_string()
}

pub(crate) fn check(
    run: Run,
    timeout: Duration,
    path: &Path,
    what: &'static str,
) -> Result<(), MediaError> {
    if run.timed_out {
        return Err(MediaError::Timeout {
            what,
            secs: timeout.as_secs(),
        });
    }
    if !run.ok {
        log::warn!(
            "ffmpeg 读取“{}”的{what}失败：{}",
            path.display(),
            last_line(&run.stderr)
        );
        return Err(MediaError::Decode { what });
    }
    Ok(())
}

/// 用 `ffmpeg_dir` 中的 ffprobe 读取 `-show_format -show_streams` 的 JSON，超过 `timeout` 就结束它；`what` 用于错误提示
pub(crate) fn probe_json(
    ffmpeg_dir: &Path,
    path: &Path,
    timeout: Duration,
    what: &'static str,
) -> Result<Value, MediaError> {
    let exe = tool(ffmpeg_dir, "ffprobe")?;
    let args: Vec<OsString> = [
        "-v",
        "error",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
    ]
    .iter()
    .map(OsString::from)
    .chain([input(path)])
    .collect();
    let mut out = Vec::new();
    let run = run(&exe, &args, timeout, &mut |b| {
        out.extend_from_slice(b);
        true
    })?;
    check(run, timeout, path, what)?;
    serde_json::from_slice(&out).map_err(|err| {
        log::warn!("ffprobe 输出的元数据无法解析（{}）：{err}", path.display());
        MediaError::Decode { what }
    })
}

/// ffprobe 把多数数字写成字符串；非负有限值才算
pub(crate) fn num(v: &Value) -> Option<f64> {
    v.as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| v.as_f64())
        .filter(|x| x.is_finite() && *x >= 0.0)
}

pub(crate) fn text(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

/// 在格式与流的标签中找第一个键（转小写后）满足 `pred` 的值；标签名的大小写随容器而变
pub(crate) fn tag<'a>(tags: &[&'a Value], pred: &dyn Fn(&str) -> bool) -> Option<&'a Value> {
    tags.iter()
        .filter_map(|t| t.as_object())
        .flat_map(|t| t.iter())
        .find(|(k, _)| pred(&k.to_lowercase()))
        .map(|(_, v)| v)
}

/// 写入 `files.meta_json` 的对象；值为空的项不写
pub(crate) fn meta_json(items: Vec<(&str, Option<Value>)>) -> String {
    let m: serde_json::Map<String, Value> = items
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| (k.to_string(), v)))
        .collect();
    Value::Object(m).to_string()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn subfile_input() {
        let p = subfile(Path::new(r"D:\资料\a,b.pptx"), 10, 20);
        assert_eq!(
            input(&p),
            OsString::from(r"subfile,,start,10,end,20,,:file:D:\资料\a,b.pptx")
        );
        assert_eq!(
            input(Path::new(r"D:\x.mp4")),
            OsString::from(r"file:D:\x.mp4")
        );
    }

    /// 开发缓存中的 ffmpeg（`scripts/fetch-ffmpeg.ps1`）；没有时跳过并提示
    pub(crate) fn ffmpeg_dir() -> Option<PathBuf> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.cache/third-party/ffmpeg")
            .join(FFMPEG_BUILD);
        if cfg!(windows) && dir.join("ffmpeg.exe").is_file() {
            return Some(dir);
        }
        println!(
            "跳过：找不到 {}，请先运行 scripts/fetch-ffmpeg.ps1",
            dir.display()
        );
        None
    }

    #[test]
    fn meta_skips_empty() {
        let json = meta_json(vec![("a", Some(Value::from(1))), ("b", None)]);
        assert_eq!(json, r#"{"a":1}"#);
        assert_eq!(num(&Value::from("9.5")), Some(9.5));
        assert_eq!(num(&Value::from("-1")), None);
        assert_eq!(num(&Value::from("N/A")), None);
    }
}
