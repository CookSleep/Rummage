//! LiteRT-LM 推理后端（`LiteRtBackend`）：手机端用的另一套向量空间（`SpaceConfig::litert`），在电脑上只用于评测。
//!
//! - 引擎在本进程内运行（`rummage_litert::Engine`，只用 CPU 后端），一把锁串行处理请求；创建后端时即加载模型。
//! - 文本：前缀仍由本 crate 拼接（`prefix`），`<bos>`、`<eos>` 由引擎插入。超过最长输入（`MAX_INPUT`）时报错，不截断。
//!   数 token（`count_tokens`）用 `tokenizers` 加载主模型仓库的 `tokenizer.json`，与引擎内置的分词器相同，结果含 `<bos>`、`<eos>`。
//! - 图像：只支持 70、140 两档（模型只有这两种视觉签名），按输入的档位逐次指定。调用方已按 GGUF 的规则缩放
//!   （`rummage_media::fit`），这里再缩放到引擎预处理的不动点尺寸（`fixed_size`），无损编码成 PNG，引擎解码后不再缩放。
//! - 音频：16 kHz 单声道采样封装成 32 位浮点 WAV 字节，梅尔谱由引擎计算。
//! - 视频窗口：文字、音频、画面帧按原顺序交错传入；token 数按 GGUF 的计法估算（每帧 `宽/48 × 高/48 + 2`，
//!   音频每 40 毫秒一个加 2），超过上限返回 `Error::TooLong`，与 llama.cpp 后端一样让带音频的视频改为只看画面。
//! - 绝不使用 GPU 或任何加速器：LiteRT 的 WebGPU 后端曾让外接显卡驱动反复超时重置、只能重启。

use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use rummage_litert::{Engine, Part, Settings};
use rummage_media::Rgb;
use rummage_media::video::VideoPart;
use tokenizers::Tokenizer;

use crate::local::{MAX_AUDIO_SAMPLES, prefixed};
use crate::space::{self, SpaceConfig};
use crate::{Embedding, EmbeddingBackend, Error, Input, Priority, prefix, vector};

/// 最长输入（token），与 llama.cpp 后端的上下文相同；引擎默认取模型文件中的 1024
const MAX_INPUT: usize = 2048;
/// LiteRT 模型支持的视觉 token 档位
const TIERS: [u32; 2] = [70, 140];
/// 每个视觉 token 对应的边长（patch 16 × 池化 3），与模型文件的元数据一致
const SIDE: u32 = 48;
/// 每 40 毫秒（640 个采样）一个音频 token
const AUDIO_SAMPLES_PER_TOKEN: usize = 640;
/// 线程数上限：i7-12700H 上线程越多越慢（XNNPack），2–20 线程实测文本 85–542 毫秒、140 档图像 1.85–2.29 秒，
/// 4 线程以内最快；音频编码器 2 线程最快（10 秒 1.19 秒，4 线程 1.87 秒，20 线程 6.97 秒）
const MAX_THREADS: u32 = 4;
const MAX_AUDIO_THREADS: u32 = 2;

/// 创建 `LiteRtBackend` 所需的文件与线程数
#[derive(Debug, Clone)]
pub struct LiteRtSpec {
    /// `litert-lm.dll` / `liblitert-lm.so` 所在的目录
    pub runtime_dir: PathBuf,
    /// `.litertlm` 模型文件
    pub model: PathBuf,
    /// `tokenizer.json`
    pub tokenizer: PathBuf,
    /// 建立索引线程数（参数 `index_threads`），按 `MAX_THREADS` 截断
    pub threads: u32,
}

pub struct LiteRtBackend {
    engine: Mutex<Engine>,
    tokenizer: Tokenizer,
    cfg: SpaceConfig,
}

impl LiteRtBackend {
    /// 加载分词器与模型；模型大小不对时报错
    pub fn new(spec: &LiteRtSpec) -> Result<Self, Error> {
        space::LITERT_MODEL.check(&spec.model)?;
        space::TOKENIZER.check(&spec.tokenizer)?;
        let tokenizer = Tokenizer::from_file(&spec.tokenizer)
            .map_err(|err| Error::InitFailed(format!("无法加载分词器：{err}")))?;
        let engine = Engine::new(
            &spec.runtime_dir,
            &Settings {
                model: &spec.model,
                threads: spec.threads.clamp(1, MAX_THREADS),
                audio_threads: spec.threads.clamp(1, MAX_AUDIO_THREADS),
                max_input: MAX_INPUT as u32,
                vision_tokens: TIERS[1],
            },
        )
        .map_err(|err| Error::InitFailed(err.to_string()))?;
        Ok(LiteRtBackend {
            engine: Mutex::new(engine),
            tokenizer,
            cfg: SpaceConfig::litert(),
        })
    }

    /// 文本的 token 数；`special` 时含 `<bos>`、`<eos>`
    fn tokens(&self, text: &str, special: bool) -> Result<usize, Error> {
        self.tokenizer
            .encode(text, special)
            .map(|e| e.len())
            .map_err(|err| Error::InvalidInput(format!("无法分词：{err}")))
    }

    fn run(&self, parts: &[Part], tier: u32) -> Result<Vec<f32>, Error> {
        let raw = self
            .engine
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .embed(parts, tier)
            .map_err(|err| match err {
                rummage_litert::Error::InvalidInput(msg) => Error::InvalidInput(msg),
                err => Error::Inference(err.to_string()),
            })?;
        vector::normalize(raw)
    }

    /// 文本、画面帧与音频交错的输入：估算 token 数（见模块说明），超过 `max_tokens` 时返回 `Error::TooLong`。
    /// 一次请求只有一个档位：不低于 `min_tier`、能放下所有帧的档位，各帧都按它缩放到不动点
    fn interleaved(
        &self,
        parts: &[VideoPart],
        max_tokens: usize,
        min_tier: u32,
    ) -> Result<Vec<f32>, Error> {
        let mut tier = min_tier;
        for p in parts {
            if let VideoPart::Frame(rgb) = p {
                tier = tier.max(tier_of(rgb)?);
            }
        }
        let mut total = 2;
        let mut owned = Vec::with_capacity(parts.len());
        for p in parts {
            match p {
                VideoPart::Text(t) => {
                    total += self.tokens(t, false)?;
                    owned.push(Owned::Text(t.clone()));
                }
                VideoPart::Frame(rgb) => {
                    total += image_tokens(rgb) + 2;
                    owned.push(Owned::Bytes(png(rgb, tier)?, true));
                }
                VideoPart::Audio(samples) => {
                    check_audio(samples)?;
                    total += samples.len().div_ceil(AUDIO_SAMPLES_PER_TOKEN) + 2;
                    owned.push(Owned::Bytes(wav(samples), false));
                }
            }
        }
        if total > max_tokens.min(MAX_INPUT) {
            return Err(Error::TooLong(format!(
                "输入约有 {total} 个 token，超过上限 {}",
                max_tokens.min(MAX_INPUT)
            )));
        }
        let parts: Vec<Part> = owned
            .iter()
            .map(|o| match o {
                Owned::Text(t) => Part::Text(t),
                Owned::Bytes(b, true) => Part::Image(b),
                Owned::Bytes(b, false) => Part::Audio(b),
            })
            .collect();
        self.run(&parts, tier)
    }

    fn handle(&self, input: Input) -> Result<Embedding, Error> {
        let (vector, space) = match input {
            Input::Image { rgb, tokens } => {
                let t = check_tier(&rgb, tokens)?;
                let v = self.run(&[Part::Image(&png(&rgb, t)?)], t)?;
                (v, self.cfg.image(tokens))
            }
            Input::ImageQuery { rgb, tokens, text } => {
                let t = check_tier(&rgb, tokens)?;
                let v = if text.trim().is_empty() {
                    self.run(&[Part::Image(&png(&rgb, t)?)], t)?
                } else {
                    let parts = [
                        VideoPart::Text(prefix::query(&text)),
                        VideoPart::Frame(std::sync::Arc::new(rgb)),
                    ];
                    self.interleaved(&parts, MAX_INPUT, t)
                        .map_err(|err| match err {
                            Error::InvalidInput(msg) | Error::TooLong(msg) => {
                                Error::QueryTooLong(msg)
                            }
                            err => err,
                        })?
                };
                (v, self.cfg.image(tokens))
            }
            Input::Audio {
                samples,
                preprocess,
            } => {
                check_audio(&samples)?;
                let v = self.run(&[Part::Audio(&wav(&samples))], TIERS[1])?;
                (v, self.cfg.media(&preprocess))
            }
            Input::Video {
                parts,
                preprocess,
                max_tokens,
            } => (
                self.interleaved(&parts, max_tokens, TIERS[0])?,
                self.cfg.media(&preprocess),
            ),
            text => {
                let query = matches!(text, Input::Query(_));
                let text = prefixed(&text).expect("图像、音频与视频以外的输入都是文本");
                let n = self.tokens(&text, true)?;
                if n > MAX_INPUT {
                    let msg = format!("文本有 {n} 个 token，超过上限 {MAX_INPUT}");
                    return Err(if query {
                        Error::QueryTooLong(msg)
                    } else {
                        Error::InvalidInput(msg)
                    });
                }
                (self.run(&[Part::Text(&text)], TIERS[1])?, self.cfg.text())
            }
        };
        Ok(Embedding { vector, space })
    }
}

impl EmbeddingBackend for LiteRtBackend {
    fn embed(&self, input: Input, _priority: Priority) -> Result<Embedding, Error> {
        self.handle(input)
    }

    fn count_tokens(&self, input: &Input, _priority: Priority) -> Result<usize, Error> {
        let Some(text) = prefixed(input) else {
            return Err(Error::InvalidInput(
                "只能计算文本输入的 token 数。".to_string(),
            ));
        };
        self.tokens(&text, true)
    }
}

enum Owned {
    Text(String),
    /// 编码后的字节；为真时是图像，否则是音频
    Bytes(Vec<u8>, bool),
}

fn image_tokens(rgb: &Rgb) -> usize {
    (rgb.width / SIDE) as usize * (rgb.height / SIDE) as usize
}

/// 能放下这张图的最小档位；超过最大档位时报错
fn tier_of(rgb: &Rgb) -> Result<u32, Error> {
    let n = image_tokens(rgb);
    TIERS.into_iter().find(|t| n <= *t as usize).ok_or_else(|| {
        Error::InvalidInput(format!(
            "图像 {}×{} 有 {n} 个视觉 token，超过 {} 档。",
            rgb.width, rgb.height, TIERS[1]
        ))
    })
}

/// 输入的档位须是 LiteRT 支持的档位，图像须在档位之内
fn check_tier(rgb: &Rgb, tokens: u32) -> Result<u32, Error> {
    if !TIERS.contains(&tokens) {
        return Err(Error::InvalidInput(format!(
            "LiteRT 只支持 70 与 140 档，不支持 {tokens} 档。"
        )));
    }
    if image_tokens(rgb) > tokens as usize {
        return Err(Error::InvalidInput(format!(
            "图像 {}×{} 超过 {tokens} 档。",
            rgb.width, rgb.height
        )));
    }
    Ok(tokens)
}

fn check_audio(samples: &[f32]) -> Result<(), Error> {
    if samples.is_empty() || samples.len() > MAX_AUDIO_SAMPLES {
        return Err(Error::InvalidInput(format!(
            "音频有 {} 个采样，应在 1 到 {MAX_AUDIO_SAMPLES} 之间。",
            samples.len()
        )));
    }
    Ok(())
}

/// LiteRT 的图像预处理按 `tier` 档给出的尺寸（v0.18.0 `GetAspectRatioPreservingSize`，float 运算与 C++ 相同）；
/// 尺寸为 0 或超过像素预算时为空
fn litert_size(width: u32, height: u32, tier: u32) -> Option<(u32, u32)> {
    let max_patches = tier * 9;
    let total_px = (width as i32).checked_mul(height as i32)? as f32;
    let target_px = (max_patches * 16 * 16) as f32;
    let factor = (target_px / total_px).sqrt();
    let side = SIDE as i32;
    let mut th = (factor * height as f32 / side as f32).floor() as i32 * side;
    let mut tw = (factor * width as f32 / side as f32).floor() as i32 * side;
    if th == 0 && tw == 0 {
        return None;
    }
    let max_side = (max_patches as i32 / 9) * side;
    if th == 0 {
        th = side;
        tw = ((width as f32 / height as f32).floor() as i32 * side).min(max_side);
    } else if tw == 0 {
        tw = side;
        th = ((height as f32 / width as f32).floor() as i32 * side).min(max_side);
    }
    if (th * tw) as f32 > target_px {
        return None;
    }
    Some((tw as u32, th as u32))
}

/// 从 `(width, height)` 出发、引擎预处理不再改变的尺寸：反复套用 `litert_size` 直到不变。
/// 每一步只会把边长放大（像素数不超过预算），所以很快停下
pub(crate) fn fixed_size(width: u32, height: u32, tier: u32) -> Option<(u32, u32)> {
    let mut size = litert_size(width, height, tier)?;
    for _ in 0..16 {
        let next = litert_size(size.0, size.1, tier)?;
        if next == size {
            return Some(size);
        }
        size = next;
    }
    None
}

/// 缩放到不动点尺寸后无损编码为 PNG
fn png(rgb: &Rgb, tier: u32) -> Result<Vec<u8>, Error> {
    let (w, h) = fixed_size(rgb.width, rgb.height, tier).ok_or_else(|| {
        Error::InvalidInput(format!(
            "图像 {}×{} 的尺寸无法处理。",
            rgb.width, rgb.height
        ))
    })?;
    if (w, h) == (rgb.width, rgb.height) {
        return Ok(rummage_media::png(rgb));
    }
    let img = rummage_media::resize(rgb.clone(), w, h)
        .map_err(|err| Error::InvalidInput(err.to_string()))?;
    Ok(rummage_media::png(&img))
}

/// 16 kHz 单声道 32 位浮点 WAV（WAVE_FORMAT_IEEE_FLOAT）
fn wav(samples: &[f32]) -> Vec<u8> {
    let data = (samples.len() * 4) as u32;
    let rate = rummage_media::audio::SAMPLE_RATE;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests;
