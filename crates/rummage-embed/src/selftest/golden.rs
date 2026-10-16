//! 黄金样本：读取 `assets/golden` 的清单与当前模型的参考向量。
//! 取文本、纯图像、音频与视频窗口项；文本按 kind 构造输入，并核对拼接前缀后与清单中生成参考向量时的
//! `input` 一致；图像预先按参考向量的档位缩放，音频预先读成采样，都不计入自检耗时。
//! 视频窗口按清单的 `inputs_av` / `inputs_v` 原样组装（时间戳、音频段、画面帧，见 `rummage_media::video`），
//! 画面帧是清单给出的 JPEG，按视频帧的档位缩放；参考向量为 `<编号>_av` 与 `<编号>_v`。
//! 音频是 16 kHz 单声道 16 位 PCM WAV（README），这里直接读 WAV 的数据块并按 `x / 32768` 转 f32，与生成参考向量的
//! `llama_embed.read_wav` 相同，不依赖 ffmpeg；建索引时 ffmpeg 解码这几个文件得到的采样与此逐位相同（`rummage_media::audio` 的测试）。

use std::fs;
use std::path::Path;
use std::sync::Arc;

use rummage_media::video::VideoPart;
use serde::Deserialize;

use super::Modality;
use crate::{Error, Input, prefix, space};

#[derive(Deserialize)]
struct Manifest {
    texts: Vec<TextItem>,
    images: Vec<ImageItem>,
    audios: Vec<ImageItem>,
    videos: Vec<VideoItem>,
}

#[derive(Deserialize)]
struct VideoItem {
    id: String,
    window_seconds: f64,
    frames: u32,
    inputs_av: Vec<VideoInput>,
    inputs_v: Vec<VideoInput>,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum VideoInput {
    Text { text: String },
    Image { file: String },
    Audio { file: String },
}

#[derive(Deserialize)]
struct TextItem {
    id: String,
    kind: String,
    #[serde(default)]
    title: Option<String>,
    text: String,
    /// 生成参考向量时实际编码的文本（含前缀）。
    input: String,
}

/// 图像与音频项
#[derive(Deserialize)]
struct ImageItem {
    id: String,
    file: String,
}

#[derive(Deserialize)]
struct VectorsFile {
    model_sha256: String,
    mmproj_sha256: String,
    vision_tokens_per_image: u32,
    video_vision_tokens_per_image: u32,
    vectors: Vec<VectorItem>,
}

#[derive(Deserialize)]
struct VectorItem {
    id: String,
    vector: Vec<f32>,
}

enum Source {
    Text(Input),
    /// 相对黄金样本目录的路径。
    Image(String),
    Audio(String),
    /// 视频窗口的输入序列与预处理参数版本
    Video(Vec<VideoInput>, String),
}

struct Item {
    id: String,
    source: Source,
    want: Vec<f32>,
}

struct Parsed {
    items: Vec<Item>,
    image_tokens: u32,
    video_tokens: u32,
}

fn bad(msg: String) -> Error {
    Error::Golden(msg)
}

fn parse(manifest: &str, vectors: &str) -> Result<Parsed, Error> {
    let m: Manifest =
        serde_json::from_str(manifest).map_err(|err| bad(format!("清单无法解析：{err}。")))?;
    let v: VectorsFile =
        serde_json::from_str(vectors).map_err(|err| bad(format!("参考向量无法解析：{err}。")))?;
    if v.model_sha256 != space::MODEL.sha256 || v.mmproj_sha256 != space::MMPROJ.sha256 {
        return Err(bad("参考向量不是用当前钉定的模型文件算出的。".to_string()));
    }
    let want = |id: &str| {
        v.vectors
            .iter()
            .find(|x| x.id == id)
            .map(|x| x.vector.clone())
            .ok_or_else(|| bad(format!("参考向量中没有样本 {id}。")))
    };
    let mut items = Vec::new();
    for t in m.texts {
        let (input, encoded) = match t.kind.as_str() {
            "query" => (Input::Query(t.text.clone()), prefix::query(&t.text)),
            "document" => (
                Input::Document {
                    title: t.title.clone(),
                    text: t.text.clone(),
                },
                prefix::document(t.title.as_deref(), &t.text),
            ),
            kind => return Err(bad(format!("样本 {} 的类型“{kind}”无法识别。", t.id))),
        };
        // 前缀写法与生成参考向量时不同，比较就没有意义。
        if encoded != t.input {
            return Err(bad(format!(
                "样本 {} 拼接前缀后与清单中的输入不一致。",
                t.id
            )));
        }
        items.push(Item {
            want: want(&t.id)?,
            id: t.id,
            source: Source::Text(input),
        });
    }
    for i in m.images {
        items.push(Item {
            want: want(&i.id)?,
            id: i.id,
            source: Source::Image(i.file),
        });
    }
    for a in m.audios {
        items.push(Item {
            want: want(&a.id)?,
            id: a.id,
            source: Source::Audio(a.file),
        });
    }
    for vid in m.videos {
        for (audio, inputs) in [(true, vid.inputs_av), (false, vid.inputs_v)] {
            let id = format!("{}_{}", vid.id, if audio { "av" } else { "v" });
            // 黄金样本按自己的档位计算，不受参数 `video_frame_tokens` 影响
            let version = space::video_preprocess(
                vid.window_seconds.round() as u32,
                0,
                vid.frames,
                v.video_vision_tokens_per_image,
                audio,
            );
            items.push(Item {
                want: want(&id)?,
                id,
                source: Source::Video(inputs, version),
            });
        }
    }
    Ok(Parsed {
        items,
        image_tokens: v.vision_tokens_per_image,
        video_tokens: v.video_vision_tokens_per_image,
    })
}

/// 一条黄金样本；唤醒后验证推理引擎也用它（`wake_sample`）
pub struct Sample {
    pub id: String,
    pub modality: Modality,
    pub input: Input,
    pub want: Vec<f32>,
}

pub(super) struct Golden {
    pub samples: Vec<Sample>,
}

/// 读取并核对清单与当前模型的参考向量，不读图像文件。
fn read(dir: &Path) -> Result<Parsed, Error> {
    let read = |name: &str| {
        fs::read_to_string(dir.join(name))
            .map_err(|err| bad(format!("无法读取 \"{}\"：{err}", dir.join(name).display())))
    };
    let manifest = read("manifest.json")?;
    let vectors = read(&format!(
        "vectors-gguf-q8-{}.json",
        &space::MODEL.sha256[..16]
    ))?;
    parse(&manifest, &vectors)
}

/// 第一条文本样本：唤醒后验证推理引擎用，不解码图像。
pub(super) fn first_text(dir: &Path) -> Result<Sample, Error> {
    read(dir)?
        .items
        .into_iter()
        .find_map(|item| match item.source {
            Source::Text(input) => Some(Sample {
                id: item.id,
                modality: Modality::Text,
                input,
                want: item.want,
            }),
            Source::Image(_) | Source::Audio(_) | Source::Video(..) => None,
        })
        .ok_or_else(|| bad("清单中没有文本样本。".to_string()))
}

/// 黄金样本的 WAV（见模块说明）；不是 16 kHz 单声道 16 位 PCM 时为空
fn read_wav(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.get(..4)? != b"RIFF" || bytes.get(8..12)? != b"WAVE" {
        return None;
    }
    let le16 = |b: &[u8], i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let mut pos = 12;
    let mut pcm16 = false;
    while let Some(head) = bytes.get(pos..pos + 8) {
        let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
        let body = bytes.get(pos + 8..pos + 8 + len)?;
        match &head[..4] {
            b"fmt " if body.len() >= 16 => {
                let rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                pcm16 = le16(body, 0) == 1
                    && le16(body, 2) == 1
                    && rate == 16_000
                    && le16(body, 14) == 16;
            }
            b"data" => {
                let samples = body.as_chunks::<2>().0.iter();
                return pcm16.then(|| {
                    samples
                        .map(|c| f32::from(i16::from_le_bytes(*c)) / 32768.0)
                        .collect()
                });
            }
            _ => {}
        }
        // 块按偶数字节对齐
        pos += 8 + len + (len & 1);
    }
    None
}

fn wav(dir: &Path, file: &str) -> Result<Vec<f32>, Error> {
    let path = dir.join(file);
    fs::read(&path)
        .ok()
        .and_then(|b| read_wav(&b))
        .ok_or_else(|| {
            bad(format!(
                "无法读取音频 \"{}\"，应为 16 kHz 单声道 16 位 PCM WAV。",
                path.display()
            ))
        })
}

/// 读取清单与当前模型的参考向量，并预先缩放全部图像、读出全部音频（不计入耗时）。
pub(super) fn load(dir: &Path, max_pixels: u64) -> Result<Golden, Error> {
    let parsed = read(dir)?;
    let mut samples = Vec::new();
    for item in parsed.items {
        let (modality, input) = match item.source {
            Source::Text(input) => (Modality::Text, input),
            Source::Image(file) => {
                let path = dir.join(&file);
                let rgb = rummage_media::prepare(&path, parsed.image_tokens, max_pixels)
                    .map_err(|err| bad(format!("无法读取图像 \"{}\"：{err}", path.display())))?;
                let input = Input::Image {
                    rgb,
                    tokens: parsed.image_tokens,
                };
                (Modality::Image, input)
            }
            Source::Audio(file) => {
                let input = Input::Audio {
                    samples: wav(dir, &file)?,
                    preprocess: space::AUDIO_PREPROCESS.to_string(),
                };
                (Modality::Audio, input)
            }
            Source::Video(inputs, preprocess) => {
                let mut parts = Vec::with_capacity(inputs.len());
                for p in inputs {
                    parts.push(match p {
                        VideoInput::Text { text } => VideoPart::Text(text),
                        VideoInput::Image { file } => {
                            let path = dir.join(&file);
                            let rgb =
                                rummage_media::prepare(&path, parsed.video_tokens, max_pixels)
                                    .map_err(|err| {
                                        bad(format!("无法读取图像 \"{}\"：{err}", path.display()))
                                    })?;
                            VideoPart::Frame(Arc::new(rgb))
                        }
                        VideoInput::Audio { file } => VideoPart::Audio(wav(dir, &file)?),
                    });
                }
                // 自检只比较向量，不受建索引时的 token 上限约束
                let input = Input::Video {
                    parts,
                    preprocess,
                    max_tokens: usize::MAX,
                };
                (Modality::Video, input)
            }
        };
        samples.push(Sample {
            id: item.id,
            modality,
            input,
            want: item.want,
        });
    }
    Ok(Golden { samples })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn golden_files() -> (String, String) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/golden");
        let manifest = fs::read_to_string(dir.join("manifest.json")).unwrap();
        let vectors = fs::read_to_string(dir.join(format!(
            "vectors-gguf-q8-{}.json",
            &space::MODEL.sha256[..16]
        )))
        .unwrap();
        (manifest, vectors)
    }

    #[test]
    fn parses_golden() {
        let (manifest, vectors) = golden_files();
        let p = parse(&manifest, &vectors).unwrap();
        assert_eq!(p.image_tokens, 140);
        let texts = p
            .items
            .iter()
            .filter(|i| matches!(i.source, Source::Text(_)))
            .count();
        let audios = p
            .items
            .iter()
            .filter(|i| matches!(i.source, Source::Audio(_)))
            .count();
        let videos = p
            .items
            .iter()
            .filter(|i| matches!(i.source, Source::Video(..)))
            .count();
        assert_eq!(
            (
                texts,
                p.items.len() - texts - audios - videos,
                audios,
                videos
            ),
            (15, 5, 3, 2)
        );
        // 黄金样本始终按自己的档位（视频帧 70）
        assert_eq!(p.video_tokens, 70);
        let Source::Video(inputs, version) = &p.items[23].source else {
            panic!("第 24 项应为视频窗口")
        };
        assert_eq!(p.items[23].id, "vid01_av");
        assert_eq!(version, "video-v2-w6-o0-f6-t70-av");
        // 时间戳、音频段、时间戳、画面帧
        assert!(matches!(&inputs[0], VideoInput::Text { text } if text == "00:01"));
        assert!(matches!(&inputs[1], VideoInput::Audio { .. }));
        assert!(matches!(&inputs[3], VideoInput::Image { .. }));
        assert_eq!(inputs.len(), 24);
        assert!(p.items.iter().all(|i| i.want.len() == 768));
        assert!(matches!(&p.items[0].source, Source::Text(Input::Query(_))));
        let Source::Image(file) = &p.items[15].source else {
            panic!("第 16 项应为图像")
        };
        assert!(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/golden")
                .join(file)
                .exists()
        );
    }

    #[test]
    fn first_text_sample() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/golden");
        let s = first_text(&dir).unwrap();
        assert_eq!(s.modality, Modality::Text);
        assert!(matches!(s.input, Input::Query(_)));
        assert_eq!(s.want.len(), 768);
        assert!(first_text(&dir.join("不存在")).is_err());
    }

    #[test]
    fn loads_audio_samples() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/golden");
        let wav = fs::read(dir.join("audio/aud09_env.wav")).unwrap();
        let samples = read_wav(&wav).unwrap();
        // 5.00 秒
        assert_eq!(samples.len(), 80_000);
        assert!(samples.iter().all(|x| (-1.0..1.0).contains(x)));
        assert!(read_wav(b"RIFF\0\0\0\0WAVEdata").is_none());
        assert!(read_wav(&wav[..40]).is_none());
        // 不是 16 位时不读
        let mut bad = wav.clone();
        let fmt = bad.windows(4).position(|w| w == b"fmt ").unwrap();
        bad[fmt + 8 + 14] = 8;
        assert!(read_wav(&bad).is_none());
    }

    #[test]
    fn rejects_bad_golden() {
        let (manifest, vectors) = golden_files();
        let err = |m: &str, v: &str| match parse(m, v) {
            Err(Error::Golden(msg)) => msg,
            Err(other) => panic!("错误类型不对：{other}"),
            Ok(_) => panic!("应当失败"),
        };
        assert!(err(&manifest.replacen("\"query\"", "\"code\"", 1), &vectors).contains("无法识别"));
        assert!(
            err(
                &manifest.replacen("task: search result", "task: x", 1),
                &vectors
            )
            .contains("不一致")
        );
        assert!(err(&manifest, &vectors.replacen("\"q01\"", "\"q00\"", 1)).contains("q01"));
        assert!(err(&manifest, &vectors.replace(space::MODEL.sha256, "00")).contains("模型文件"));
        assert!(err("{", &vectors).contains("清单"));
    }
}
