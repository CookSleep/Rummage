//! 测速：按当前参数处理一个文件（一段）的耗时（`DeviceSpeed`），选择设备与引导中的预估都用它。
//! 只在设备通过正确性检查后进行，只计时、不比较结果。
//! - 文本：黄金文本样本只有一二十个 token，远短于切片，总是把它们的正文拼接成一段 `chunk_tokens` 长的文档（加文档前缀）另测。
//! - 图片：黄金样本按当前的 `image_tokens` 档位计算（`golden`），总是直接用计时那一遍中图片样本的耗时（`Plan::Reuse`）。
//! - 视频：当前的视频设置（窗口、帧数、音频）与所选的黄金视频变体相同、帧档位也相同时同样复用；否则（自定义了窗口长度、
//!   帧数或音频）另测（`Plan::Measure`），按当前的视频设置（`VideoSpeed`）与 `video_frame_tokens` 用黄金视频窗口的素材组装一个片段。
//! - 音频：样本时长与 `audio_segment_s` 相当时复用，否则把样本拼接成 `audio_segment_s` 长的一段另测。
//! - 另测只针对开着识别的类型（`index_video` 等）。另测的输入在 GPU 上各算两遍、取最小值：第一次遇到新的档位时可能要编译着色器；
//!   CPU 上各算一遍，多个输入取最小值。
//! - 长图分段（`long_image_tokens`）不测：分段数因图而异，预估按图片耗时的倍数估算。
//! - 测速用到的参数值记入结果（`DeviceSpeed::params`），参数之后被修改时界面据此提示。
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use rummage_media::audio::SAMPLE_RATE;
use rummage_media::video::{self, VideoPart, WindowOpts};
use rummage_proto::DeviceSpeed;
use rummage_proto::params::{self, ParamValue};

use super::candidate::median;
use super::golden::{self, Golden, VideoInput};
use super::{Modality, VideoSpeed};
use crate::local::LocalBackend;
use crate::{EmbeddingBackend, Error, Input, Priority, space};

/// 音频样本时长（中位数）与分段长度相差不超过这么多秒时算一致：样本是 5–10 秒的整段音频，与默认的 10 秒一段相当
const AUDIO_TOLERANCE_S: f64 = 1.0;

/// 一类输入怎么得到耗时
pub(super) enum Plan {
    /// 与黄金样本一致：这些样本在计时那一遍的耗时取中位数
    Reuse(Vec<String>),
    /// 另测这些输入，取最小值
    Measure(Vec<Input>),
}

/// 测速的安排，所有候选设备相同
pub(crate) struct SpeedPlan {
    /// 黄金文本样本的正文，拼接到 `chunk_tokens` 长
    pub(super) text: String,
    pub(super) chunk_tokens: usize,
    /// 为空时不测这一类（只测文本，或视频、音频参数与黄金样本不一致且没有开启识别）
    pub(super) image: Option<Plan>,
    pub(super) audio: Option<Plan>,
    pub(super) video: Option<Plan>,
    pub(super) params: BTreeMap<String, ParamValue>,
}

/// 按当前参数安排测速（见模块说明）。`images` 为否（只测文本）时只测文本
pub(super) fn plan(
    dir: &Path,
    golden: &Golden,
    values: &BTreeMap<String, ParamValue>,
    vs: VideoSpeed,
    images: bool,
    max_pixels: u64,
) -> Result<SpeedPlan, Error> {
    let m = golden::manifest(dir)?;
    let text: String = m.texts.iter().map(|t| format!("{}\n", t.text)).collect();
    let mut plan = SpeedPlan {
        text,
        chunk_tokens: params::int(values, "chunk_tokens") as usize,
        image: None,
        audio: None,
        video: None,
        params: BTreeMap::new(),
    };
    let mut keys = vec!["chunk_tokens", "index_threads"];
    if !images {
        plan.params = snapshot(values, &keys);
        return Ok(plan);
    }
    let on = |key: &str| params::flag(values, key);
    let ids = |modality: Modality| -> Vec<String> {
        golden
            .samples
            .iter()
            .filter(|s| s.modality == modality)
            .map(|s| s.id.clone())
            .collect()
    };

    plan.image = Some(Plan::Reuse(ids(Modality::Image)));
    keys.push("image_tokens");

    let seg = params::int(values, "audio_segment_s") as usize;
    let clips = m
        .audios
        .iter()
        .map(|a| golden::wav(dir, &a.file))
        .collect::<Result<Vec<_>, _>>()?;
    let secs = median(
        clips
            .iter()
            .map(|c| c.len() as f64 / f64::from(SAMPLE_RATE))
            .collect(),
    );
    plan.audio = if secs.is_some_and(|s| (s - seg as f64).abs() <= AUDIO_TOLERANCE_S) {
        Some(Plan::Reuse(ids(Modality::Audio)))
    } else if on("index_audio") {
        // 样本依次循环拼接到一段的长度
        let n = seg * SAMPLE_RATE as usize;
        let samples: Vec<f32> = clips.iter().flatten().copied().cycle().take(n).collect();
        Some(Plan::Measure(vec![Input::Audio {
            samples,
            preprocess: space::AUDIO_PREPROCESS.to_string(),
        }]))
    } else {
        None
    };
    if plan.audio.is_some() {
        keys.push("audio_segment_s");
    }

    let frame_tokens = params::int(values, "video_frame_tokens") as u32;
    plan.video = match m.videos.first() {
        Some(_) if golden.video == Some(vs) && frame_tokens == golden.video_tokens => {
            Some(Plan::Reuse(ids(Modality::Video)))
        }
        Some(v) if on("index_video") => {
            // 帧最多的变体带全了窗口的画面帧与音频段
            let Some(full) = v.variants.iter().max_by_key(|x| x.frames) else {
                return Err(golden::bad("清单中的视频样本没有输入。".to_string()));
            };
            let mut frames = Vec::new();
            let mut track = Vec::new();
            for p in &full.inputs {
                match p {
                    VideoInput::Image { file } => {
                        let path = dir.join(file);
                        let rgb = rummage_media::prepare(&path, frame_tokens, max_pixels).map_err(
                            |err| {
                                golden::bad(format!("无法读取图像 \"{}\"：{err}", path.display()))
                            },
                        )?;
                        frames.push(Arc::new(rgb));
                    }
                    VideoInput::Audio { file } => track.extend(golden::wav(dir, file)?),
                    VideoInput::Text { .. } => {}
                }
            }
            if frames.is_empty() {
                return Err(golden::bad("清单中的视频样本没有画面帧。".to_string()));
            }
            let opts = WindowOpts {
                window_s: vs.window_s,
                overlap_s: 0,
                frames: vs.frames,
            };
            // 至少一个窗口（`video::windows`）
            let w = &video::windows(u64::from(vs.window_s) * 1000, opts)[0];
            let parts: Vec<VideoPart> = video::assemble(w, &frames, vs.audio.then_some(&track[..]));
            Some(Plan::Measure(vec![Input::Video {
                parts,
                preprocess: space::video_preprocess(
                    vs.window_s,
                    0,
                    vs.frames,
                    frame_tokens,
                    vs.audio,
                ),
                max_tokens: usize::MAX,
            }]))
        }
        _ => None,
    };
    if plan.video.is_some() {
        keys.extend(["video_preset", "video_frame_tokens"]);
        // “自定义”时窗口取自高级项
        if params::int(values, "video_preset") == 0 {
            keys.extend(["video_window_s", "video_frames", "video_audio"]);
        }
    }
    plan.params = snapshot(values, &keys);
    Ok(plan)
}

fn snapshot(values: &BTreeMap<String, ParamValue>, keys: &[&str]) -> BTreeMap<String, ParamValue> {
    keys.iter()
        .map(|k| (k.to_string(), params::value(values, k)))
        .collect()
}

/// 测速出错时的样本说明（如“测速 图片：”）与错误
pub(super) type SpeedError = (String, Error);

/// 按安排测速。`times` 为计时那一遍各样本的耗时（毫秒）；`gpu` 时另测的输入各算两遍。
/// `stop` 置位（检测被中止）时返回 `Ok(None)`
pub(super) fn measure(
    backend: &LocalBackend,
    plan: &SpeedPlan,
    times: &[(String, f64)],
    gpu: bool,
    stop: &AtomicBool,
) -> Result<Option<DeviceSpeed>, SpeedError> {
    let doc = |text: String| Input::Document { title: None, text };
    let count = |text: &str| {
        backend
            .count_tokens(&doc(text.to_string()), Priority::Index)
            .map_err(|err| ("测速 文本：".to_string(), err))
    };
    // 先重复到超过所需长度，再按字符比例截短（前缀与特殊 token 只占一次，截短后约为 `chunk_tokens`）
    let base = count(&plan.text)?.max(1);
    let long = plan.text.repeat(plan.chunk_tokens.div_ceil(base) + 1);
    let total = count(&long)?.max(1);
    let keep = long.chars().count() * plan.chunk_tokens / total;
    let text = doc(long.chars().take(keep).collect());

    let rounds = if gpu { 2 } else { 1 };
    let timed = |label: &str, inputs: &[Input]| -> Result<Option<f64>, SpeedError> {
        let mut best: Option<f64> = None;
        for _ in 0..rounds {
            for input in inputs {
                if stop.load(Ordering::Relaxed) {
                    return Ok(None);
                }
                let t = Instant::now();
                backend
                    .embed(input.clone(), Priority::Index)
                    .map_err(|err| (format!("测速 {label}："), err))?;
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                best = Some(best.map_or(ms, |b| b.min(ms)));
            }
        }
        Ok(best)
    };
    let get = |label: &str, plan: &Option<Plan>| -> Result<Option<f64>, SpeedError> {
        match plan {
            None => Ok(None),
            Some(Plan::Reuse(ids)) => Ok(median(
                times
                    .iter()
                    .filter(|(id, _)| ids.contains(id))
                    .map(|(_, ms)| *ms)
                    .collect(),
            )),
            Some(Plan::Measure(inputs)) => timed(label, inputs),
        }
    };
    let speed = DeviceSpeed {
        text_ms: timed("文本", &[text])?,
        image_ms: get("图片", &plan.image)?,
        audio_ms: get("音频", &plan.audio)?,
        video_ms: get("视频", &plan.video)?,
        params: plan.params.clone(),
    };
    Ok((!stop.load(Ordering::Relaxed)).then_some(speed))
}
