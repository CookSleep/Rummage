//! 音频：用 ffprobe 读元数据，用 ffmpeg 解码为 16 kHz 单声道 f32，按时间切段并跳过静音。
//!
//! - ffmpeg 的调用方式见 `ffmpeg`。
//! - 解码：只取第一条音轨（`-map 0:a:0`，带封面图的 mp3 也只解音频），`-ac 1 -ar 16000 -f f32le` 由 ffmpeg 混成单声道并重采样；
//!   16 位 PCM 的输入转成 f32 时就是 `x / 32768`，与黄金样本的读法逐位相同。
//!   采样数超过上限时立即结束 ffmpeg 并返回 `TooLong`，不会因为文件谎报时长而读入过多数据。视频的音轨同样用它解码。
//! - 元数据：时长、编码、采样率、声道数、码率、容器，录制时间只认 `creation_time` 标签（mp3 的 `date` 多是发行年份），
//!   位置标签只记“有没有”。
//! - 切段（`spans`）：段长、步长按采样数计，步长为段长减重叠（至少 1 秒）。不丢弃任何内容，也不产生过短或几乎重合的段：
//!   - 比一段还短（或正好一段）的音频整段作为一段。
//!   - 否则先按长图切片的取法算段数 n：相邻段至少重叠 `overlap_s` 所需的最少段数。第 n 段只为盖住
//!     前 n − 1 段按步长排开后剩下的尾巴 r（0 < r ≤ 步长）。
//!   - r 不到半个步长、且并入后不超过模型的上限 `MAX_SEGMENT_S` 时，尾巴并入上一段（与视频窗口末尾不足 `MIN_TAIL_MS`
//!     时并入上一个窗口的思路相同）：前 n − 1 段从 0 起按步长排开，最后一段延长到末端，长为段长加 r。
//!   - 否则 n 段等长、起点在 `[0, 总长 − 段长]` 上均匀分布：首段从 0 开始、末段与末端对齐，实际重叠可能略大于 `overlap_s`。
//!   - 整段音量（RMS，dBFS）低于静音阈值的段跳过。修改这里的规则时提升 `rummage_embed::space::AUDIO_PREPROCESS`。

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::ffmpeg::{self, MediaError, input, num, tag, text, tool};

/// 送入模型的采样率
pub const SAMPLE_RATE: u32 = 16_000;
/// 模型单个输入的音频上限（秒）：参数表中音频段长的上限
pub const MAX_SEGMENT_S: u32 = 30;

/// ffprobe 读出的元数据，各项读不到时为空
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioInfo {
    pub duration_ms: Option<u64>,
    pub codec: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub bit_rate: Option<u64>,
    pub container: Option<String>,
    /// `creation_time` 标签的原文
    pub created: Option<String>,
    pub has_gps: bool,
}

impl AudioInfo {
    /// 写入 `files.meta_json` 的内容；读不到的项不写
    pub fn meta_json(&self) -> String {
        ffmpeg::meta_json(vec![
            ("duration_ms", self.duration_ms.map(Value::from)),
            ("codec", self.codec.clone().map(Value::from)),
            ("sample_rate", self.sample_rate.map(Value::from)),
            ("channels", self.channels.map(Value::from)),
            ("bit_rate", self.bit_rate.map(Value::from)),
            ("container", self.container.clone().map(Value::from)),
            ("created", self.created.clone().map(Value::from)),
            ("has_gps", Some(Value::from(self.has_gps))),
        ])
    }
}

/// 用 `ffmpeg_dir` 中的 ffprobe 读取元数据，超过 `timeout` 就结束它
pub fn probe(ffmpeg_dir: &Path, path: &Path, timeout: Duration) -> Result<AudioInfo, MediaError> {
    parse_probe(&ffmpeg::probe_json(ffmpeg_dir, path, timeout, "音频")?)
}

/// 解析 `ffprobe -print_format json -show_format -show_streams` 的输出
pub fn parse_probe(v: &Value) -> Result<AudioInfo, MediaError> {
    let stream = v["streams"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["codec_type"] == "audio"))
        .ok_or(MediaError::NoAudio)?;
    let format = &v["format"];
    let tags = [&format["tags"], &stream["tags"]];
    Ok(AudioInfo {
        duration_ms: num(&format["duration"])
            .or_else(|| num(&stream["duration"]))
            .map(|s| (s * 1000.0).round() as u64),
        codec: text(&stream["codec_name"]),
        sample_rate: num(&stream["sample_rate"]).map(|x| x as u32),
        channels: stream["channels"].as_u64().map(|x| x as u32),
        bit_rate: num(&format["bit_rate"])
            .or_else(|| num(&stream["bit_rate"]))
            .map(|x| x as u64),
        container: text(&format["format_name"]),
        created: tag(&tags, &|k| k == "creation_time").and_then(text),
        has_gps: tag(&tags, &|k| k.contains("location")).is_some(),
    })
}

/// 解码为 16 kHz 单声道 f32 采样。`expect` 为按时长估计的采样数（预先分配），超过 `max` 个采样时结束 ffmpeg 并返回 `TooLong`
pub fn decode(
    ffmpeg_dir: &Path,
    path: &Path,
    expect: usize,
    max: usize,
    timeout: Duration,
) -> Result<Vec<f32>, MediaError> {
    let exe = tool(ffmpeg_dir, "ffmpeg")?;
    let rate = SAMPLE_RATE.to_string();
    let args: Vec<OsString> = ["-nostdin", "-hide_banner", "-loglevel", "error", "-i"]
        .iter()
        .map(OsString::from)
        .chain([input(path)])
        .chain(
            [
                "-map", "0:a:0", "-vn", "-ac", "1", "-ar", &rate, "-f", "f32le", "-",
            ]
            .iter()
            .map(OsString::from),
        )
        .collect();
    let mut samples: Vec<f32> = Vec::with_capacity(expect.min(max));
    // 上一块末尾不足 4 字节的部分
    let mut rest: Vec<u8> = Vec::new();
    let mut too_long = false;
    let run = ffmpeg::run(&exe, &args, timeout, &mut |b| {
        rest.extend_from_slice(b);
        let (chunks, tail) = rest.as_chunks::<4>();
        samples.extend(chunks.iter().map(|c| f32::from_le_bytes(*c)));
        rest = tail.to_vec();
        too_long = samples.len() > max;
        !too_long
    })?;
    if too_long {
        return Err(MediaError::TooLong);
    }
    ffmpeg::check(run, timeout, path, "音频")?;
    Ok(samples)
}

/// 一段音频在采样中的范围 `[start, end)`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn start_ms(&self) -> u64 {
        self.start as u64 * 1000 / u64::from(SAMPLE_RATE)
    }

    pub fn end_ms(&self) -> u64 {
        self.end as u64 * 1000 / u64::from(SAMPLE_RATE)
    }
}

/// 实际使用的重叠秒数：不小于段长时取段长减 1 秒，步长至少 1 秒
pub fn effective_overlap(segment_s: u32, overlap_s: u32) -> u32 {
    overlap_s.min(segment_s.saturating_sub(1))
}

/// `len` 个采样按 `segment_s` 秒一段、重叠 `overlap_s` 秒切分（规则见模块说明）
pub fn spans(len: usize, segment_s: u32, overlap_s: u32) -> Vec<Span> {
    let rate = SAMPLE_RATE as usize;
    let seg = segment_s.max(1) as usize * rate;
    let hop = (segment_s.max(1) - effective_overlap(segment_s.max(1), overlap_s)) as usize * rate;
    if len == 0 {
        return Vec::new();
    }
    if len <= seg {
        return vec![Span { start: 0, end: len }];
    }
    let n = (len - seg).div_ceil(hop) + 1;
    let tail = len - seg - (n - 2) * hop;
    if tail * 2 < hop && seg + tail <= MAX_SEGMENT_S as usize * rate {
        return (0..n - 1)
            .map(|i| Span {
                start: i * hop,
                end: if i == n - 2 { len } else { i * hop + seg },
            })
            .collect();
    }
    (0..n)
        .map(|i| {
            let start = (len - seg) * i / (n - 1);
            Span {
                start,
                end: start + seg,
            }
        })
        .collect()
}

/// 均方根音量（dBFS）；全为 0 时为负无穷
pub(crate) fn rms_db(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return f64::NEG_INFINITY;
    }
    let sum: f64 = samples.iter().map(|&x| f64::from(x) * f64::from(x)).sum();
    10.0 * (sum / samples.len() as f64).log10()
}

/// 按 `spans` 切段，去掉整段音量低于 `silence_db` 的段
pub fn segments(samples: &[f32], segment_s: u32, overlap_s: u32, silence_db: f64) -> Vec<Span> {
    spans(samples.len(), segment_s, overlap_s)
        .into_iter()
        .filter(|s| rms_db(&samples[s.start..s.end]) >= silence_db)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffmpeg::tests::ffmpeg_dir;

    const R: usize = SAMPLE_RATE as usize;

    fn ranges(len: usize, seg: u32, overlap: u32) -> Vec<(usize, usize)> {
        spans(len, seg, overlap)
            .iter()
            .map(|s| (s.start / R, s.end / R))
            .collect()
    }

    fn ms(len_ms: usize, seg: u32, overlap: u32) -> Vec<(u64, u64)> {
        spans(len_ms * R / 1000, seg, overlap)
            .iter()
            .map(|s| (s.start_ms(), s.end_ms()))
            .collect()
    }

    #[test]
    fn split_with_overlap() {
        // 10 秒一段、重叠 2 秒（步长 8 秒）。正好铺满：0–10、8–18、16–26
        assert_eq!(ranges(26 * R, 10, 2), [(0, 10), (8, 18), (16, 26)]);
        assert_eq!(ranges(18 * R, 10, 2), [(0, 10), (8, 18)]);
        // 尾巴不到半个步长（4 秒）：并入上一段，不出现几乎重合的两段
        assert_eq!(ms(10_500, 10, 2), [(0, 10_500)]);
        assert_eq!(ms(13_900, 10, 2), [(0, 13_900)]);
        assert_eq!(
            ms(27_000, 10, 2),
            [(0, 10_000), (8000, 18_000), (16_000, 27_000)]
        );
        // 尾巴不少于半个步长：等长的段均匀分布，首段从 0 起、末段与末端对齐
        assert_eq!(ms(14_000, 10, 2), [(0, 10_000), (4000, 14_000)]);
        assert_eq!(
            ms(23_000, 10, 2),
            [(0, 10_000), (6500, 16_500), (13_000, 23_000)]
        );
        // 并入后会超过模型上限 30 秒时不并入
        assert_eq!(ms(30_500, 30, 2), [(0, 30_000), (500, 30_500)]);
        // 不重叠
        assert_eq!(ranges(30 * R, 10, 0), [(0, 10), (10, 20), (20, 30)]);
        assert_eq!(
            ms(31_000, 10, 0),
            [(0, 10_000), (10_000, 20_000), (20_000, 31_000)]
        );
        // 任何长度都不丢内容：首段从 0 起、末段到末端、相邻段相接或重叠，每段不超过模型上限
        for len in (1..200).map(|i| i * R / 7) {
            for (seg, overlap) in [(10, 2), (10, 0), (5, 4), (30, 10), (1, 0)] {
                let s = spans(len, seg, overlap);
                assert_eq!(s[0].start, 0);
                assert_eq!(s.last().unwrap().end, len);
                assert!(
                    s.windows(2)
                        .all(|p| p[1].start <= p[0].end && p[1].start > p[0].start)
                );
                assert!(
                    s.iter()
                        .all(|x| x.end - x.start <= MAX_SEGMENT_S as usize * R)
                );
            }
        }
    }

    #[test]
    fn short_and_edge_cases() {
        assert!(spans(0, 10, 2).is_empty());
        assert_eq!(
            spans(5 * R, 10, 2),
            [Span {
                start: 0,
                end: 5 * R
            }]
        );
        assert_eq!(
            spans(10 * R, 10, 2),
            [Span {
                start: 0,
                end: 10 * R
            }]
        );
        // 重叠不小于段长时按段长减 1 秒，步长 1 秒
        assert_eq!(effective_overlap(10, 12), 9);
        assert_eq!(effective_overlap(1, 2), 0);
        assert_eq!(ranges(12 * R, 10, 15), [(0, 10), (1, 11), (2, 12)]);
        let s = Span {
            start: R * 3 + R / 2,
            end: R * 13 + R / 2,
        };
        assert_eq!((s.start_ms(), s.end_ms()), (3500, 13500));
    }

    #[test]
    fn silence_skipped() {
        // 0–10 秒为正弦（约 −9 dB），10–20 秒为静音，20–25 秒为很轻的噪声（约 −70 dB）
        let mut x: Vec<f32> = (0..10 * R).map(|i| 0.5 * (i as f32 * 0.05).sin()).collect();
        x.extend(std::iter::repeat_n(0.0, 10 * R));
        x.extend((0..5 * R).map(|i| if i % 2 == 0 { 3e-4 } else { -3e-4 }));
        let db = rms_db(&x[..10 * R]);
        assert!((db + 9.03).abs() < 0.1, "{db}");
        assert_eq!(rms_db(&[0.0; 4]), f64::NEG_INFINITY);
        // 段（尾巴 7 秒不少于半个步长，等长均匀分布）：0–10、7.5–17.5、15–25；7.5–17.5 含 2.5 秒正弦，仍高于阈值
        let kept: Vec<(u64, u64)> = segments(&x, 10, 2, -60.0)
            .iter()
            .map(|s| (s.start_ms(), s.end_ms()))
            .collect();
        assert_eq!(kept, [(0, 10_000), (7500, 17_500)]);
        // 阈值更低时轻噪声段保留
        assert_eq!(segments(&x, 10, 2, -80.0).len(), 3);
    }

    #[test]
    fn probe_output() {
        let json = r#"{"streams": [
            {"index": 0, "codec_type": "video", "codec_name": "mjpeg"},
            {"index": 1, "codec_type": "audio", "codec_name": "aac", "sample_rate": "32000", "channels": 2,
             "duration": "9.007000", "bit_rate": "96925", "tags": {"language": "und"}}],
          "format": {"format_name": "mov,mp4,m4a,3gp,3g2,mj2", "duration": "9.007000", "bit_rate": "98984",
             "tags": {"creation_time": "2026-10-01T08:30:00.000000Z", "com.apple.quicktime.location.ISO6709": "+31.2+121.4/"}}}"#;
        let info = parse_probe(&serde_json::from_str(json).unwrap()).unwrap();
        assert_eq!(
            info,
            AudioInfo {
                duration_ms: Some(9007),
                codec: Some("aac".into()),
                sample_rate: Some(32000),
                channels: Some(2),
                bit_rate: Some(98984),
                container: Some("mov,mp4,m4a,3gp,3g2,mj2".into()),
                created: Some("2026-10-01T08:30:00.000000Z".into()),
                has_gps: true,
            }
        );
        let meta: Value = serde_json::from_str(&info.meta_json()).unwrap();
        assert_eq!(meta["duration_ms"], 9007);
        assert_eq!(meta["has_gps"], true);
        // 坐标不写入
        assert!(!info.meta_json().contains("121.4"));

        // 时长只在音轨上、没有标签
        let json = r#"{"streams": [{"codec_type": "audio", "codec_name": "pcm_s16le", "sample_rate": "16000",
            "channels": 1, "duration": "9.660000"}], "format": {"format_name": "wav"}}"#;
        let info = parse_probe(&serde_json::from_str(json).unwrap()).unwrap();
        assert_eq!(info.duration_ms, Some(9660));
        assert!(!info.has_gps && info.created.is_none() && info.bit_rate.is_none());
        let meta: Value = serde_json::from_str(&info.meta_json()).unwrap();
        assert!(meta.get("bit_rate").is_none());

        // 没有音轨
        let json = serde_json::json!({"streams": [{"codec_type": "video"}], "format": {}});
        assert!(matches!(parse_probe(&json), Err(MediaError::NoAudio)));
    }

    /// 读 16 位 PCM WAV 的数据块，按 `x / 32768` 转成 f32（与生成黄金向量的 `llama_embed.read_wav` 相同）
    fn wav_samples(bytes: &[u8]) -> Vec<f32> {
        let pos = bytes.windows(4).position(|w| w == b"data").unwrap();
        bytes[pos + 8..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| f32::from(i16::from_le_bytes(*c)) / 32768.0)
            .collect()
    }

    #[test]
    fn decode_golden_matches_wav() {
        let Some(dir) = ffmpeg_dir() else {
            return;
        };
        let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/golden/audio");
        for name in ["aud01_zh.wav", "aud07_en.wav", "aud09_env.wav"] {
            let path = golden.join(name);
            let info = probe(&dir, &path, Duration::from_secs(30)).unwrap();
            assert_eq!(info.codec.as_deref(), Some("pcm_s16le"));
            let got = decode(&dir, &path, 0, usize::MAX, Duration::from_secs(30)).unwrap();
            let want = wav_samples(&std::fs::read(&path).unwrap());
            // 16 kHz 单声道 16 位的输入：ffmpeg 不重采样，转 f32 与 x / 32768 逐位相同
            assert_eq!(got.len(), want.len(), "{name}");
            assert!(
                got.iter()
                    .zip(&want)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "{name}"
            );
            assert_eq!(
                info.duration_ms,
                Some((want.len() as u64 * 1000).div_ceil(16_000))
            );
        }
    }

    #[test]
    fn decode_limits_and_errors() {
        let Some(dir) = ffmpeg_dir() else {
            return;
        };
        let wav =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/golden/audio/aud09_env.wav");
        // 超过采样上限时立即停止
        let err = decode(&dir, &wav, 0, 16_000, Duration::from_secs(30)).unwrap_err();
        assert!(matches!(err, MediaError::TooLong), "{err}");
        // 不是音频的文件、不存在的文件：报错而不是 panic
        let tmp = std::env::temp_dir().join(format!("rummage-audio-{}.mp3", std::process::id()));
        std::fs::write(&tmp, b"not audio at all").unwrap();
        assert!(matches!(
            decode(&dir, &tmp, 0, usize::MAX, Duration::from_secs(30)),
            Err(MediaError::Decode { .. })
        ));
        assert!(probe(&dir, &tmp, Duration::from_secs(30)).is_err());
        std::fs::remove_file(&tmp).unwrap();
        assert!(matches!(
            decode(
                &dir.join("没有"),
                &wav,
                0,
                usize::MAX,
                Duration::from_secs(5)
            ),
            Err(MediaError::MissingTool(_))
        ));
    }
}
