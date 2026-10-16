//! 视频：ffprobe 读元数据，按时间窗口切分，整段视频只运行一次 ffmpeg 抽帧，
//! 再把每个窗口的画面帧（和音频段）组装成一个多模态输入。
//!
//! - 窗口：第 k 个窗口从 `k × 步长` 开始，长 `window_s` 秒，步长为窗口长减重叠（至少 1 秒）。窗口铺满整段视频；
//!   下一个窗口的起点距视频末尾不足 `MIN_TAIL_MS` 时不另开窗口，末尾这段并入上一个窗口：上一个窗口的定位（`end_ms`）
//!   延伸到视频末尾，抽帧仍按网格不变（它的最后一帧在窗口末端，离视频末尾不到 `MIN_TAIL_MS`；为覆盖末尾另加或挪动帧
//!   会打乱各窗口共用的网格与帧数）。最后一个窗口超出视频末尾的部分取最后一帧、音频补静音，定位记到视频末尾为止。
//! - 抽帧时刻：窗口内均匀分布、首尾各一帧（只取 1 帧时取中点），与黄金样本相同（`frame_times`）。所有窗口的抽帧时刻都落在
//!   一个等距网格上（`Grid`）：不重叠时网格就是帧间隔，相邻窗口共用边界上的那一帧；带重叠时网格更密，多抽的帧不用。
//! - 一次解码：`fps` 滤镜把视频变成网格间隔的恒定帧率（`start_time=0` 时网格从 0 秒起，开头缺帧时重复第一帧；
//!   `round=up` 使网格时刻 T 取 T 及之前最后一帧，即播放到 T 时屏幕上的那一帧），随后 `scale` 直接缩放到
//!   每帧视觉 token 档位的尺寸（`frame_size`，Gemma 4 公式，与图片相同；档位为参数 `video_frame_tokens`），以 RGB 原始像素经管道读出，不经 JPEG。
//!   第 m 帧即网格上第 m 个时刻，不必逐个定位、也不依赖时间戳输出；关键帧稀疏时相邻窗口也不会拿到同一帧。
//!   软件解码时 ffmpeg 默认按旋转标记把画面转正，所以尺寸按转正后的宽高算（`VideoInfo`）。
//!   黄金样本按 `-ss T` 取 T 之后的第一帧（`post.py`），与这里最多差一帧。
//! - 硬件解码：先用 `-hwaccel d3d11va`，帧留在显存中、经 `fps` 取舍后才下载（`hwdownload`）；硬件解码不支持该编码时
//!   ffmpeg 给出的是内存中的帧，`hwdownload` 报错退出，同一文件改用软件解码再试一次，其他解码错误也同样重试。
//!   硬件解码的帧不会按旋转标记转正（实测 ffmpeg 9.0），带旋转标记的视频（手机竖拍）直接用软件解码（`VideoInfo::rotated`）。
//!   实测（i7-12700H，1080p H.264）硬件解码比多线程软件解码慢约一倍，但 CPU 占用只有约九分之一，后台建索引时更不打扰前台。
//! - 组装（`assemble`）：带音频时依次为 `时间戳, 音频段, 时间戳, 画面帧`，时间戳为窗口起点加帧时刻取整秒的 `mm:ss`，
//!   音频段为窗口音频按帧数等分（`window_s × 16000 / 帧数` 个采样）；不带音频时只放画面帧。与黄金样本的
//!   各变体的 `inputs` 相同，媒体之间不加空格，占位与分词由推理引擎做。
//! - Android 不用 ffmpeg：元数据与抽帧用系统的 MediaExtractor + MediaCodec（`android/video.rs`），取帧规则、YUV 转 RGB 与转正
//!   按 ffmpeg 的做法自己实现（`raw`），缩放用 `resize_bicubic`，结果与电脑接近而不逐位相同；`ffmpeg_dir`、`hwaccel` 在 Android 上不用。
//! - 修改窗口、抽帧或组装的规则时提升 `rummage_embed::space::VIDEO_PREPROCESS`。

#[cfg(any(target_os = "android", test))]
pub(crate) mod raw;

#[cfg(not(target_os = "android"))]
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::audio::SAMPLE_RATE;
use crate::ffmpeg::{self, MediaError, num, tag, text};
#[cfg(not(target_os = "android"))]
use crate::ffmpeg::{input, tool};
use crate::{Error, Rgb, target_size};

/// 末尾不足这么多毫秒的内容不另开窗口、并入上一个窗口：两帧几乎相同，只多算一次推理
const MIN_TAIL_MS: u64 = 500;

/// 视频的元数据（电脑上由 ffprobe 读出，Android 上由 MediaExtractor 读出），各项读不到时为空
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VideoInfo {
    pub duration_ms: Option<u64>,
    /// 按旋转标记转正后的宽高
    pub width: u32,
    pub height: u32,
    /// 带旋转标记：只能用软件解码（见模块说明）
    pub rotated: bool,
    pub codec: Option<String>,
    pub fps: Option<f64>,
    pub bit_rate: Option<u64>,
    pub container: Option<String>,
    /// `creation_time` 标签的原文
    pub created: Option<String>,
    /// 相机或手机型号
    pub camera: Option<String>,
    pub has_gps: bool,
    pub has_audio: bool,
}

impl VideoInfo {
    /// 写入 `files.meta_json` 的内容；读不到的项不写，GPS 只记有无
    pub fn meta_json(&self) -> String {
        ffmpeg::meta_json(vec![
            ("duration_ms", self.duration_ms.map(Value::from)),
            ("width", Some(Value::from(self.width))),
            ("height", Some(Value::from(self.height))),
            ("codec", self.codec.clone().map(Value::from)),
            (
                "fps",
                self.fps.map(|f| Value::from((f * 1000.0).round() / 1000.0)),
            ),
            ("bit_rate", self.bit_rate.map(Value::from)),
            ("container", self.container.clone().map(Value::from)),
            ("created", self.created.clone().map(Value::from)),
            ("camera", self.camera.clone().map(Value::from)),
            ("has_gps", Some(Value::from(self.has_gps))),
            ("has_audio", Some(Value::from(self.has_audio))),
        ])
    }
}

/// 读取元数据：电脑上用 `ffmpeg_dir` 中的 ffprobe，超过 `timeout` 就结束它；Android 上用 MediaExtractor（见 `android/video.rs`）
pub fn probe(ffmpeg_dir: &Path, path: &Path, timeout: Duration) -> Result<VideoInfo, MediaError> {
    // Android 上不设时限，读取卡住时无法中途结束
    #[cfg(target_os = "android")]
    {
        let _ = (ffmpeg_dir, timeout);
        crate::android::video::probe(path)
    }
    #[cfg(not(target_os = "android"))]
    parse_probe(&ffmpeg::probe_json(ffmpeg_dir, path, timeout, "视频")?)
}

/// `"30000/1001"` 形式的帧率
fn rate(v: &Value) -> Option<f64> {
    let (a, b) = v.as_str()?.split_once('/')?;
    let (a, b) = (a.parse::<f64>().ok()?, b.parse::<f64>().ok()?);
    (a > 0.0 && b > 0.0).then(|| a / b)
}

/// 解析 `ffprobe -print_format json -show_format -show_streams` 的输出。封面图（`attached_pic`）不算视频画面
pub fn parse_probe(v: &Value) -> Result<VideoInfo, MediaError> {
    let streams = v["streams"].as_array().map_or(&[][..], Vec::as_slice);
    let stream = streams
        .iter()
        .find(|s| s["codec_type"] == "video" && s["disposition"]["attached_pic"] != 1)
        .ok_or(MediaError::NoVideo)?;
    let format = &v["format"];
    let tags = [&format["tags"], &stream["tags"]];
    let dim = |key: &str| stream[key].as_u64().and_then(|x| u32::try_from(x).ok());
    let (Some(w), Some(h)) = (dim("width"), dim("height")) else {
        return Err(MediaError::NoVideo);
    };
    // 旋转写在显示矩阵（新版本）或 rotate 标签（旧容器）中
    let rotation = stream["side_data_list"]
        .as_array()
        .and_then(|l| l.iter().find_map(|d| d["rotation"].as_f64()))
        .or_else(|| tag(&tags, &|k| k == "rotate").and_then(|r| r.as_str()?.parse().ok()))
        .unwrap_or(0.0);
    let turned = (rotation.round() as i64).rem_euclid(180) == 90;
    let model = tag(&tags, &|k| k == "model" || k.ends_with(".model")).and_then(text);
    let make = tag(&tags, &|k| k == "make" || k.ends_with(".make")).and_then(text);
    let camera = match (make, model) {
        (Some(make), Some(model)) if !model.starts_with(&make) => Some(format!("{make} {model}")),
        (make, model) => model.or(make),
    };
    Ok(VideoInfo {
        duration_ms: num(&format["duration"])
            .or_else(|| num(&stream["duration"]))
            .map(|s| (s * 1000.0).round() as u64),
        width: if turned { h } else { w },
        height: if turned { w } else { h },
        rotated: (rotation.round() as i64).rem_euclid(360) != 0,
        codec: text(&stream["codec_name"]),
        fps: rate(&stream["avg_frame_rate"]).or_else(|| rate(&stream["r_frame_rate"])),
        bit_rate: num(&format["bit_rate"])
            .or_else(|| num(&stream["bit_rate"]))
            .map(|x| x as u64),
        container: text(&format["format_name"]),
        created: tag(&tags, &|k| k == "creation_time").and_then(text),
        camera,
        has_gps: tag(&tags, &|k| k.contains("location")).is_some(),
        has_audio: streams.iter().any(|s| s["codec_type"] == "audio"),
    })
}

/// 窗口设置（参数表的视频预设或高级项）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowOpts {
    pub window_s: u32,
    /// 实际使用的重叠（`crate::audio::effective_overlap`）
    pub overlap_s: u32,
    pub frames: u32,
}

impl WindowOpts {
    fn step_s(&self) -> u64 {
        u64::from(self.window_s - self.overlap_s.min(self.window_s.saturating_sub(1))).max(1)
    }

    /// 每秒的刻度数：帧时刻都是它的整数倍
    fn ticks(&self) -> u64 {
        2 * u64::from(self.frames.max(2) - 1)
    }

    /// 窗口 `k` 第 `i` 帧的时刻（刻度）
    fn tick(&self, k: u64, i: u64) -> u64 {
        let start = k * self.step_s() * self.ticks();
        if self.frames <= 1 {
            // 中点：window_s / 2 秒
            return start + u64::from(self.window_s);
        }
        start + i * 2 * u64::from(self.window_s)
    }

    /// 抽帧网格
    pub fn grid(&self) -> Grid {
        let step = self.step_s() * self.ticks();
        let spacing = if self.frames <= 1 {
            gcd(step, u64::from(self.window_s))
        } else {
            gcd(step, 2 * u64::from(self.window_s))
        };
        Grid {
            ticks: self.ticks(),
            spacing,
        }
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// 等距抽帧网格：第 m 帧在 `m × spacing / ticks` 秒
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    ticks: u64,
    spacing: u64,
}

impl Grid {
    /// `fps` 滤镜的帧率（每秒帧数的分数）
    pub fn fps(&self) -> String {
        let g = gcd(self.ticks, self.spacing);
        format!("{}/{}", self.ticks / g, self.spacing / g)
    }

    /// 时长 `ms` 内的网格帧数（含 0 秒那一帧）
    pub fn frames_in(&self, ms: u64) -> usize {
        (ms * self.ticks / (self.spacing * 1000)) as usize + 1
    }

    /// `frames` 个网格帧覆盖的时长（毫秒），时长未知时由抽出的帧数估计
    pub fn ms(&self, frames: usize) -> u64 {
        frames as u64 * self.spacing * 1000 / self.ticks
    }
}

/// 一个窗口
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub start_ms: u64,
    /// 不超过视频时长
    pub end_ms: u64,
    /// 名义长度，即窗口秒数；音频按它等分
    pub len_ms: u64,
    /// 各帧在网格上的序号
    pub frames: Vec<usize>,
    /// 各帧的时间戳文字（`mm:ss`）
    pub stamps: Vec<String>,
}

/// `mm:ss`，分钟可超过 59（与黄金样本的 `post.py` 相同）
fn stamp(secs: u64) -> String {
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

/// 时长 `duration_ms` 的视频按 `opts` 切成的窗口（见模块说明），至少一个
pub fn windows(duration_ms: u64, opts: WindowOpts) -> Vec<Window> {
    let grid = opts.grid();
    let (win_ms, step_ms) = (u64::from(opts.window_s) * 1000, opts.step_s() * 1000);
    let mut out: Vec<Window> = Vec::new();
    for k in 0.. {
        let start_ms = k * step_ms;
        if k > 0 && start_ms - step_ms + win_ms >= duration_ms {
            break;
        }
        if k > 0 && start_ms + MIN_TAIL_MS > duration_ms {
            if let Some(last) = out.last_mut() {
                last.end_ms = duration_ms;
            }
            break;
        }
        let n = u64::from(opts.frames.max(1));
        let ticks: Vec<u64> = (0..n).map(|i| opts.tick(k, i)).collect();
        out.push(Window {
            start_ms,
            end_ms: (start_ms + win_ms).min(duration_ms.max(start_ms)),
            len_ms: win_ms,
            frames: ticks.iter().map(|t| (t / grid.spacing) as usize).collect(),
            stamps: ticks.iter().map(|t| stamp(t / grid.ticks)).collect(),
        });
    }
    out
}

/// 每帧缩放后的尺寸：`tokens` 为每帧的视觉 token 档位
pub fn frame_size(width: u32, height: u32, tokens: u32) -> Result<(u32, u32), Error> {
    target_size(width, height, tokens)
}

/// 整段视频按 `grid` 抽帧并缩放到 `size`（见模块说明）。`start_s` 为开始的秒数（只供测试对照黄金样本，建索引时为空）。
/// 帧数超过 `max_frames` 时结束 ffmpeg 并返回 `TooLong`；`hwaccel` 为是时先试硬件解码
#[allow(clippy::too_many_arguments)]
pub fn extract(
    ffmpeg_dir: &Path,
    path: &Path,
    size: (u32, u32),
    grid: Grid,
    max_frames: usize,
    hwaccel: bool,
    start_s: Option<f64>,
    timeout: Duration,
) -> Result<Vec<Rgb>, MediaError> {
    #[cfg(target_os = "android")]
    {
        let _ = (ffmpeg_dir, hwaccel);
        crate::android::video::extract(path, size, grid, max_frames, start_s, timeout)
    }
    #[cfg(not(target_os = "android"))]
    if hwaccel {
        match extract_once(
            ffmpeg_dir, path, size, grid, max_frames, true, start_s, timeout,
        ) {
            Err(err @ MediaError::Decode { .. }) => {
                log::debug!("硬件解码失败，改用软件解码：{err}");
            }
            res => return res,
        }
    }
    #[cfg(not(target_os = "android"))]
    extract_once(
        ffmpeg_dir, path, size, grid, max_frames, false, start_s, timeout,
    )
}

#[cfg(not(target_os = "android"))]
#[allow(clippy::too_many_arguments)]
fn extract_once(
    ffmpeg_dir: &Path,
    path: &Path,
    (w, h): (u32, u32),
    grid: Grid,
    max_frames: usize,
    hwaccel: bool,
    start_s: Option<f64>,
    timeout: Duration,
) -> Result<Vec<Rgb>, MediaError> {
    let exe = tool(ffmpeg_dir, "ffmpeg")?;
    let mut args: Vec<OsString> = ["-nostdin", "-hide_banner", "-loglevel", "error"]
        .iter()
        .map(OsString::from)
        .collect();
    // 解码出的帧留在显存里，`fps` 丢掉不要的帧之后才下载：逐帧下载时硬件解码比软件解码还慢几倍
    if hwaccel {
        args.extend(["-hwaccel", "d3d11va", "-hwaccel_output_format", "d3d11"].map(OsString::from));
    }
    if let Some(s) = start_s {
        args.extend([OsString::from("-ss"), OsString::from(format!("{s:.3}"))]);
    }
    let download = if hwaccel {
        "hwdownload,format=nv12|p010,"
    } else {
        ""
    };
    let filter = format!(
        "fps=fps={}:start_time=0:round=up,{download}scale={w}:{h}:flags=bicubic,format=rgb24",
        grid.fps()
    );
    args.push(OsString::from("-i"));
    args.push(input(path));
    // V（大写）不含封面图
    args.extend(
        [
            "-map", "0:V:0", "-an", "-sn", "-dn", "-vf", &filter, "-f", "rawvideo", "-",
        ]
        .map(OsString::from),
    );
    let frame_bytes = (w * h * 3) as usize;
    let mut frames: Vec<Rgb> = Vec::new();
    let mut cur: Vec<u8> = Vec::with_capacity(frame_bytes);
    let mut too_long = false;
    let run = ffmpeg::run(&exe, &args, timeout, &mut |mut b| {
        while !b.is_empty() {
            let n = (frame_bytes - cur.len()).min(b.len());
            cur.extend_from_slice(&b[..n]);
            b = &b[n..];
            if cur.len() == frame_bytes {
                let data = std::mem::replace(&mut cur, Vec::with_capacity(frame_bytes));
                frames.push(Rgb {
                    width: w,
                    height: h,
                    data,
                });
            }
        }
        too_long = frames.len() > max_frames;
        !too_long
    })?;
    if too_long {
        return Err(MediaError::TooLong);
    }
    ffmpeg::check(run, timeout, path, "视频")?;
    if frames.is_empty() {
        return Err(MediaError::NoVideo);
    }
    Ok(frames)
}

/// 一个窗口的输入片段
#[derive(Debug, Clone)]
pub enum VideoPart {
    Text(String),
    /// 已缩放到每帧档位的画面帧；相邻窗口共用的帧只存一份
    Frame(Arc<Rgb>),
    /// 16 kHz 单声道采样
    Audio(Vec<f32>),
}

/// 组装一个窗口（见模块说明）：`frames` 为整段视频的网格帧（不为空），窗口要的帧超出末尾时取最后一帧；
/// `audio` 为整段视频的 16 kHz 采样，为空时只放画面帧
pub fn assemble(window: &Window, frames: &[Arc<Rgb>], audio: Option<&[f32]>) -> Vec<VideoPart> {
    let frame = |m: usize| VideoPart::Frame(frames[m.min(frames.len() - 1)].clone());
    let Some(audio) = audio else {
        return window.frames.iter().map(|&m| frame(m)).collect();
    };
    let n = window.frames.len();
    let per_ms = (SAMPLE_RATE / 1000) as usize;
    // 按窗口的名义长度等分：超出音频末尾的部分补静音，各段一样长
    let seg = window.len_ms as usize * per_ms / n;
    let base = window.start_ms as usize * per_ms;
    let mut parts = Vec::with_capacity(n * 4);
    for i in 0..n {
        let a: Vec<f32> = (base + i * seg..base + (i + 1) * seg)
            .map(|j| audio.get(j).copied().unwrap_or(0.0))
            .collect();
        parts.push(VideoPart::Text(window.stamps[i].clone()));
        parts.push(VideoPart::Audio(a));
        parts.push(VideoPart::Text(window.stamps[i].clone()));
        parts.push(frame(window.frames[i]));
    }
    parts
}

/// 只取画面帧（共用像素，不复制）：带音频的输入超出 token 上限或推理失败时，整段视频改为纯画面
pub fn frames_only(parts: &[VideoPart]) -> Vec<VideoPart> {
    parts
        .iter()
        .filter(|p| matches!(p, VideoPart::Frame(_)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::ffmpeg::tests::ffmpeg_dir;

    /// 与黄金样本的帧比较时用的每帧档位（`video_frame_tokens` 的默认值）
    const GOLDEN_TOKENS: u32 = 140;

    const OBJECTS: WindowOpts = WindowOpts {
        window_s: 2,
        overlap_s: 0,
        frames: 2,
    };
    const SPEECH: WindowOpts = WindowOpts {
        window_s: 6,
        overlap_s: 0,
        frames: 6,
    };

    /// 各窗口各帧的时刻（毫秒）与定位
    fn plan(ms: u64, opts: WindowOpts) -> Vec<(u64, u64, Vec<u64>)> {
        let g = opts.grid();
        windows(ms, opts)
            .iter()
            .map(|w| {
                let t = w
                    .frames
                    .iter()
                    .map(|&m| m as u64 * g.spacing * 1000 / g.ticks)
                    .collect();
                (w.start_ms, w.end_ms, t)
            })
            .collect()
    }

    #[test]
    fn window_frame_times() {
        // 找物体：2 秒 2 帧，相邻窗口共用边界帧；7 秒的视频末窗 6–7 秒，第二帧超出末尾
        assert_eq!(OBJECTS.grid().fps(), "1/2");
        assert_eq!(
            plan(7000, OBJECTS),
            [
                (0, 2000, vec![0, 2000]),
                (2000, 4000, vec![2000, 4000]),
                (4000, 6000, vec![4000, 6000]),
                (6000, 7000, vec![6000, 8000]),
            ]
        );
        // 末尾不足半秒不另开窗口，并入上一个窗口（定位延伸到末尾，帧不变）；正好铺满时也不多开
        assert_eq!(
            plan(6300, OBJECTS),
            [
                (0, 2000, vec![0, 2000]),
                (2000, 4000, vec![2000, 4000]),
                (4000, 6300, vec![4000, 6000]),
            ]
        );
        assert_eq!(plan(6000, OBJECTS).len(), 3);
        assert_eq!(plan(6000, OBJECTS)[2].1, 6000);
        // 半秒及以上另开窗口
        assert_eq!(plan(6500, OBJECTS)[3].0, 6000);
        // 比一个窗口还短
        assert_eq!(plan(800, OBJECTS), [(0, 800, vec![0, 2000])]);
        // 找说话内容：6 秒 6 帧，帧间隔 1.2 秒（黄金样本的 frame_times）
        assert_eq!(SPEECH.grid().fps(), "5/6");
        let p = plan(13_000, SPEECH);
        assert_eq!(p[0].2, [0, 1200, 2400, 3600, 4800, 6000]);
        assert_eq!(p[1].2, [6000, 7200, 8400, 9600, 10800, 12000]);
        assert_eq!(p.len(), 3);
        // 找动作：4 秒 4 帧
        let act = WindowOpts {
            window_s: 4,
            overlap_s: 0,
            frames: 4,
        };
        assert_eq!(act.grid().fps(), "3/4");
        assert_eq!(plan(4000, act)[0].2, [0, 1333, 2666, 4000]);
        // 带重叠：网格变密，帧时刻仍在窗口内均匀分布
        let overlap = WindowOpts {
            window_s: 4,
            overlap_s: 2,
            frames: 3,
        };
        assert_eq!(overlap.grid().fps(), "1/2");
        assert_eq!(
            plan(8000, overlap),
            [
                (0, 4000, vec![0, 2000, 4000]),
                (2000, 6000, vec![2000, 4000, 6000]),
                (4000, 8000, vec![4000, 6000, 8000]),
            ]
        );
        let odd = WindowOpts {
            window_s: 4,
            overlap_s: 1,
            frames: 4,
        };
        let p = plan(10_000, odd);
        assert_eq!(p[1].0, 3000);
        assert_eq!(p[1].2, [3000, 4333, 5666, 7000]);
        // 只取 1 帧时取中点
        let one = WindowOpts {
            window_s: 2,
            overlap_s: 0,
            frames: 1,
        };
        assert_eq!(
            plan(4000, one),
            [(0, 2000, vec![1000]), (2000, 4000, vec![3000])]
        );
        // 重叠不小于窗口时步长 1 秒
        let over = WindowOpts {
            window_s: 2,
            overlap_s: 5,
            frames: 2,
        };
        assert_eq!(
            plan(3000, over).iter().map(|w| w.0).collect::<Vec<_>>(),
            [0, 1000]
        );
        assert_eq!(OBJECTS.grid().frames_in(7000), 4);
        assert_eq!(SPEECH.grid().ms(5), 6000);
    }

    #[test]
    fn stamps_are_floor_seconds() {
        // 黄金样本：窗口起点 1 秒 + 0、1.2、2.4、3.6、4.8、6.0 秒
        assert_eq!(stamp(1), "00:01");
        assert_eq!(stamp(7), "00:07");
        assert_eq!(stamp(3725), "62:05");
        let w = &windows(20_000, SPEECH)[1];
        assert_eq!(
            w.stamps,
            ["00:06", "00:07", "00:08", "00:09", "00:10", "00:12"]
        );
    }

    fn rgb(v: u8) -> Arc<Rgb> {
        Arc::new(Rgb {
            width: 1,
            height: 1,
            data: vec![v; 3],
        })
    }

    fn kinds(parts: &[VideoPart]) -> Vec<String> {
        parts
            .iter()
            .map(|p| match p {
                VideoPart::Text(t) => t.clone(),
                VideoPart::Frame(f) => format!("帧{}", f.data[0]),
                VideoPart::Audio(a) => format!("音{}", a.len()),
            })
            .collect()
    }

    #[test]
    fn assemble_order() {
        let frames: Vec<Arc<Rgb>> = (0..4).map(rgb).collect();
        let ws = windows(7000, OBJECTS);
        // 不带音频：只有画面帧，没有时间戳；末窗超出末尾的帧取最后一帧
        assert_eq!(kinds(&assemble(&ws[0], &frames, None)), ["帧0", "帧1"]);
        assert_eq!(kinds(&assemble(&ws[3], &frames, None)), ["帧3", "帧3"]);
        // 带音频：时间戳、音频段、时间戳、画面帧；每段 1 秒
        let audio: Vec<f32> = (0..7 * 16_000).map(|i| i as f32).collect();
        let parts = assemble(&ws[1], &frames, Some(&audio));
        assert_eq!(
            kinds(&parts),
            [
                "00:02", "音16000", "00:02", "帧1", "00:04", "音16000", "00:04", "帧2"
            ]
        );
        let VideoPart::Audio(a) = &parts[5] else {
            panic!("第 6 项应为音频")
        };
        assert_eq!(a[0], 48_000.0);
        // 末窗超出音频末尾的部分补静音
        let parts = assemble(&ws[3], &frames, Some(&audio));
        let VideoPart::Audio(a) = &parts[5] else {
            panic!("第 6 项应为音频")
        };
        assert_eq!(a.len(), 16_000);
        assert!(a.iter().all(|&x| x == 0.0));
        assert_eq!(kinds(&frames_only(&parts)), ["帧3", "帧3"]);
    }

    #[test]
    fn probe_output() {
        let json = serde_json::json!({"streams": [
            {"codec_type": "video", "codec_name": "mjpeg", "width": 300, "height": 300,
             "disposition": {"attached_pic": 1}},
            {"codec_type": "video", "codec_name": "hevc", "width": 1920, "height": 1080,
             "avg_frame_rate": "30000/1001", "r_frame_rate": "30/1",
             "side_data_list": [{"side_data_type": "Display Matrix", "rotation": -90}],
             "tags": {"creation_time": "2026-10-01T08:30:00.000000Z"}},
            {"codec_type": "audio", "codec_name": "aac"}],
          "format": {"format_name": "mov,mp4,m4a,3gp,3g2,mj2", "duration": "12.345000", "bit_rate": "8000000",
             "tags": {"com.apple.quicktime.make": "Apple", "com.apple.quicktime.model": "iPhone 17",
                      "com.apple.quicktime.location.ISO6709": "+31.2+121.4/"}}});
        let info = parse_probe(&json).unwrap();
        assert_eq!(
            info,
            VideoInfo {
                duration_ms: Some(12_345),
                width: 1080,
                height: 1920,
                rotated: true,
                codec: Some("hevc".into()),
                fps: Some(30_000.0 / 1001.0),
                bit_rate: Some(8_000_000),
                container: Some("mov,mp4,m4a,3gp,3g2,mj2".into()),
                created: Some("2026-10-01T08:30:00.000000Z".into()),
                camera: Some("Apple iPhone 17".into()),
                has_gps: true,
                has_audio: true,
            }
        );
        let meta: Value = serde_json::from_str(&info.meta_json()).unwrap();
        assert_eq!(meta["fps"], 29.97);
        assert_eq!(meta["width"], 1080);
        assert!(!info.meta_json().contains("121.4"));
        // 旧容器的 rotate 标签；没有音轨、没有帧率
        let json = serde_json::json!({"streams": [{"codec_type": "video", "width": 640, "height": 480,
            "avg_frame_rate": "0/0", "tags": {"rotate": "180", "model": "XYZ"}}], "format": {}});
        let info = parse_probe(&json).unwrap();
        assert_eq!((info.width, info.height, info.fps), (640, 480, None));
        assert!(info.rotated);
        assert_eq!(info.camera.as_deref(), Some("XYZ"));
        assert!(!info.has_audio && info.duration_ms.is_none());
        // 只有封面图或音轨
        let json = serde_json::json!({"streams": [{"codec_type": "video", "width": 1, "height": 1,
            "disposition": {"attached_pic": 1}}, {"codec_type": "audio"}], "format": {}});
        assert!(matches!(parse_probe(&json), Err(MediaError::NoVideo)));
    }

    /// 生成黄金视频窗口的原视频，需自行放到 `.cache/golden/vid01.mp4`
    fn golden_source() -> Option<PathBuf> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cache/golden/vid01.mp4");
        if p.is_file() {
            return Some(p);
        }
        println!("跳过：找不到 {}", p.display());
        None
    }

    fn golden_frames() -> Vec<Rgb> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/golden/video/vid01");
        (0..6)
            .map(|i| {
                crate::prepare(
                    &dir.join(format!("frame{i}.jpg")),
                    GOLDEN_TOKENS,
                    100_000_000,
                )
                .unwrap()
            })
            .collect()
    }

    /// 一次解码抽出的帧与黄金样本的帧（PIL 缩成 640×360、JPEG 质量 80，再缩放到 `GOLDEN_TOKENS` 档）比较并报告差异。
    /// 两者的缩放算法、JPEG 有损压缩不同，像素不可能相同；第 0 帧正好落在原视频的一帧上（1.000 秒），取帧规则相同，
    /// 只差缩放与压缩，要求平均差很小。其余帧黄金样本取 T 之后的第一帧，这里取 T 及之前的最后一帧（见模块说明），
    /// 画面在动时差异较大，只报告
    #[test]
    fn frames_near_golden() {
        let (Some(dir), Some(src)) = (ffmpeg_dir(), golden_source()) else {
            return;
        };
        let info = probe(&dir, &src, Duration::from_secs(30)).unwrap();
        assert_eq!((info.width, info.height), (1280, 720));
        let size = frame_size(info.width, info.height, GOLDEN_TOKENS).unwrap();
        let want = golden_frames();
        assert_eq!(size, (want[0].width, want[0].height));
        for hw in [true, false] {
            let got = extract(
                &dir,
                &src,
                size,
                SPEECH.grid(),
                100,
                hw,
                Some(1.0),
                Duration::from_secs(60),
            )
            .unwrap();
            assert!(got.len() >= 6, "{}", got.len());
            for (i, (g, w)) in got.iter().zip(&want).enumerate() {
                let diff: Vec<u32> = g
                    .data
                    .iter()
                    .zip(&w.data)
                    .map(|(a, b)| u32::from(a.abs_diff(*b)))
                    .collect();
                let mean = diff.iter().sum::<u32>() as f64 / diff.len() as f64;
                println!(
                    "硬件解码 {hw}：第 {i} 帧平均差 {mean:.2}，最大差 {}",
                    diff.iter().max().unwrap()
                );
                if i == 0 {
                    assert!(mean < 3.0, "第 0 帧与黄金样本差异过大：{mean}");
                }
            }
        }
    }

    /// 一次解码抽帧的速度：硬件解码开 / 关各一次。运行：
    /// `cargo test -p rummage-media --release -- --ignored --nocapture extract_speed`，`RUMMAGE_SPEED_VIDEO` 指定视频
    #[test]
    #[ignore = "测速，需要本机的 ffmpeg 与一个较长的视频"]
    fn extract_speed() {
        let Some(dir) = ffmpeg_dir() else {
            return;
        };
        let Some(src) = std::env::var_os("RUMMAGE_SPEED_VIDEO")
            .map(PathBuf::from)
            .or_else(golden_source)
        else {
            return;
        };
        let info = probe(&dir, &src, Duration::from_secs(30)).unwrap();
        let size = frame_size(info.width, info.height, 140).unwrap();
        let secs = info.duration_ms.unwrap_or(0) as f64 / 1000.0;
        for opts in [OBJECTS, SPEECH] {
            for hw in [true, false] {
                let t = std::time::Instant::now();
                let frames = extract(
                    &dir,
                    &src,
                    size,
                    opts.grid(),
                    1_000_000,
                    hw,
                    None,
                    Duration::from_secs(3600),
                )
                .unwrap();
                let el = t.elapsed().as_secs_f64();
                println!(
                    "{}（{}×{} {:?}，{secs:.1} 秒）帧率 {}，硬件解码 {hw}：{} 帧，{el:.2} 秒，{:.0} 倍实时",
                    src.display(),
                    info.width,
                    info.height,
                    info.codec,
                    opts.grid().fps(),
                    frames.len(),
                    secs / el
                );
            }
        }
    }
}
