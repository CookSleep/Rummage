//! 黄金样本：读取 `assets/golden` 的清单与当前模型的参考向量。
//! 参考向量覆盖全部档位组合：图片在每个 `image_tokens` 档位各一条，视频窗口的每个变体（与三个视频预设对应）在每个
//! `video_frame_tokens` 档位各一条，两者都按编号与 `tokens` 查找。自检只取与当前参数对应的组合（`Pick`），
//! 参考向量中没有当前档位时报黄金样本错误。
//! 取文本、纯图像、音频与视频窗口项；文本按 kind 构造输入，并核对拼接前缀后与清单中生成参考向量时的
//! `input` 一致；图像预先按当前档位缩放，音频预先读成采样，都不计入自检耗时。
//! 视频窗口取一个变体：帧数与音频和当前视频设置相同的那个，没有（自定义的帧数或音频）时取默认预设对应的那个；
//! 按它的 `inputs` 原样组装（时间戳、音频段、画面帧，见 `rummage_media::video`），画面帧是清单给出的 JPEG，
//! 按当前视频帧档位缩放；预处理参数版本按该变体的窗口、帧数与音频计算；参考向量编号为 `<窗口编号>_<变体编号>`，如 `vid01_f2_v`。
//! 音频是 16 kHz 单声道 16 位 PCM WAV（README），这里直接读 WAV 的数据块并按 `x / 32768` 转 f32，与生成参考向量的
//! `llama_embed.read_wav` 相同，不依赖 ffmpeg；建索引时 ffmpeg 解码这几个文件得到的采样与此逐位相同（`rummage_media::audio` 的测试）。
//! 测速（`speed`）也用这里的清单与素材。

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use rummage_media::video::VideoPart;
use rummage_proto::params::{self, ParamValue};
use serde::Deserialize;

use super::speed::SpeedPlan;
use super::{Modality, VideoSpeed};
use crate::{Error, Input, prefix, space};

#[derive(Deserialize)]
pub(super) struct Manifest {
    pub(super) texts: Vec<TextItem>,
    pub(super) images: Vec<ImageItem>,
    pub(super) audios: Vec<ImageItem>,
    pub(super) videos: Vec<VideoItem>,
}

#[derive(Deserialize)]
pub(super) struct VideoItem {
    id: String,
    /// 按“找物体”“找动作”“找说话内容”的顺序排列，与预设编号 1–3 对应
    pub(super) variants: Vec<VideoVariant>,
}

/// 视频窗口的一种输入：取窗口的前 `frames` 帧，按 `window_seconds` 长的窗口计
#[derive(Deserialize)]
pub(super) struct VideoVariant {
    id: String,
    pub(super) frames: u32,
    pub(super) window_seconds: u32,
    pub(super) audio: bool,
    pub(super) inputs: Vec<VideoInput>,
}

impl VideoVariant {
    fn speed(&self) -> VideoSpeed {
        VideoSpeed {
            window_s: self.window_seconds,
            frames: self.frames,
            audio: self.audio,
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum VideoInput {
    Text { text: String },
    Image { file: String },
    Audio { file: String },
}

#[derive(Deserialize)]
pub(super) struct TextItem {
    id: String,
    kind: String,
    #[serde(default)]
    title: Option<String>,
    pub(super) text: String,
    /// 生成参考向量时实际编码的文本（含前缀）。
    input: String,
}

/// 图像与音频项
#[derive(Deserialize)]
pub(super) struct ImageItem {
    id: String,
    pub(super) file: String,
}

#[derive(Deserialize)]
struct VectorsFile {
    model_sha256: String,
    mmproj_sha256: String,
    vectors: Vec<VectorItem>,
}

#[derive(Deserialize)]
struct VectorItem {
    id: String,
    /// 图像与视频窗口的视觉 token 档位；文本与音频没有
    #[serde(default)]
    tokens: Option<u32>,
    vector: Vec<f32>,
}

/// 取哪个档位组合
struct Pick {
    image_tokens: u32,
    video_tokens: u32,
    /// 当前的视频设置；为空时取默认预设对应的变体
    video: Option<VideoSpeed>,
}

impl Pick {
    fn new(values: &BTreeMap<String, ParamValue>, video: Option<VideoSpeed>) -> Pick {
        Pick {
            image_tokens: params::int(values, "image_tokens") as u32,
            video_tokens: params::int(values, "video_frame_tokens") as u32,
            video,
        }
    }
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
    /// 所选视频变体的窗口、帧数与音频；清单中没有视频窗口时为空
    video: Option<VideoSpeed>,
}

pub(super) fn bad(msg: String) -> Error {
    Error::Golden(msg)
}

/// 帧数与音频和当前视频设置相同的变体，没有时取默认预设对应的变体
fn variant(variants: &[VideoVariant], video: Option<VideoSpeed>) -> Option<&VideoVariant> {
    let same = video.and_then(|vs| {
        variants
            .iter()
            .find(|x| x.frames == vs.frames && x.audio == vs.audio)
    });
    let preset = params::int(&params::defaults(), "video_preset") as usize;
    same.or_else(|| variants.get(preset.checked_sub(1)?))
}

fn parse(manifest: &str, vectors: &str, pick: &Pick) -> Result<Parsed, Error> {
    let m: Manifest =
        serde_json::from_str(manifest).map_err(|err| bad(format!("清单无法解析：{err}。")))?;
    let v: VectorsFile =
        serde_json::from_str(vectors).map_err(|err| bad(format!("参考向量无法解析：{err}。")))?;
    if v.model_sha256 != space::MODEL.sha256 || v.mmproj_sha256 != space::MMPROJ.sha256 {
        return Err(bad("参考向量不是用当前钉定的模型文件算出的。".to_string()));
    }
    let want = |id: &str, tokens: Option<u32>| {
        v.vectors
            .iter()
            .find(|x| x.id == id && x.tokens == tokens)
            .map(|x| x.vector.clone())
            .ok_or_else(|| match tokens {
                Some(t) => bad(format!("参考向量中没有样本 {id} 的 {t} 档。")),
                None => bad(format!("参考向量中没有样本 {id}。")),
            })
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
            want: want(&t.id, None)?,
            id: t.id,
            source: Source::Text(input),
        });
    }
    for i in m.images {
        items.push(Item {
            want: want(&i.id, Some(pick.image_tokens))?,
            id: i.id,
            source: Source::Image(i.file),
        });
    }
    for a in m.audios {
        items.push(Item {
            want: want(&a.id, None)?,
            id: a.id,
            source: Source::Audio(a.file),
        });
    }
    let mut video = None;
    for vid in m.videos {
        let Some(var) = variant(&vid.variants, pick.video) else {
            return Err(bad(format!("视频样本 {} 缺少默认预设对应的输入。", vid.id)));
        };
        let id = format!("{}_{}", vid.id, var.id);
        let version = space::video_preprocess(
            var.window_seconds,
            0,
            var.frames,
            pick.video_tokens,
            var.audio,
        );
        video = Some(var.speed());
        items.push(Item {
            want: want(&id, Some(pick.video_tokens))?,
            id,
            source: Source::Video(var.inputs.clone(), version),
        });
    }
    Ok(Parsed { items, video })
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
    /// 所选的视频帧档位（即当前参数）与视频变体的窗口、帧数与音频（没有视频样本时为空），测速据此判断能否复用
    /// 样本耗时；图像样本总是按当前的 `image_tokens` 档位（见各样本的 `Input::Image`）
    pub video_tokens: u32,
    pub video: Option<VideoSpeed>,
    /// 测速的安排；`run` 在读取样本后填写
    pub speed: Option<SpeedPlan>,
}

/// 清单（不读参考向量），测速用
pub(super) fn manifest(dir: &Path) -> Result<Manifest, Error> {
    let path = dir.join("manifest.json");
    let text = fs::read_to_string(&path)
        .map_err(|err| bad(format!("无法读取 \"{}\"：{err}", path.display())))?;
    serde_json::from_str(&text).map_err(|err| bad(format!("清单无法解析：{err}。")))
}

/// 读取并核对清单与当前模型的参考向量，不读图像文件。
fn read(dir: &Path, pick: &Pick) -> Result<Parsed, Error> {
    let read = |name: &str| {
        fs::read_to_string(dir.join(name))
            .map_err(|err| bad(format!("无法读取 \"{}\"：{err}", dir.join(name).display())))
    };
    let manifest = read("manifest.json")?;
    let vectors = read(&format!(
        "vectors-gguf-q8-{}.json",
        &space::MODEL.sha256[..16]
    ))?;
    parse(&manifest, &vectors, pick)
}

/// 第一条文本样本：唤醒后验证推理引擎用，不解码图像。
pub(super) fn first_text(dir: &Path) -> Result<Sample, Error> {
    read(dir, &Pick::new(&params::defaults(), None))?
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

pub(super) fn wav(dir: &Path, file: &str) -> Result<Vec<f32>, Error> {
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

/// 读取清单与当前模型的参考向量中与当前参数对应的组合（见模块说明），并预先缩放全部图像、读出全部音频（不计入耗时）。
/// `video` 为当前的视频设置
pub(super) fn load(
    dir: &Path,
    values: &BTreeMap<String, ParamValue>,
    video: VideoSpeed,
    max_pixels: u64,
) -> Result<Golden, Error> {
    let pick = Pick::new(values, Some(video));
    let parsed = read(dir, &pick)?;
    let mut samples = Vec::new();
    for item in parsed.items {
        let (modality, input) = match item.source {
            Source::Text(input) => (Modality::Text, input),
            Source::Image(file) => {
                let path = dir.join(&file);
                let rgb = rummage_media::prepare(&path, pick.image_tokens, max_pixels)
                    .map_err(|err| bad(format!("无法读取图像 \"{}\"：{err}", path.display())))?;
                let input = Input::Image {
                    rgb,
                    tokens: pick.image_tokens,
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
                            let rgb = rummage_media::prepare(&path, pick.video_tokens, max_pixels)
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
    Ok(Golden {
        samples,
        video_tokens: pick.video_tokens,
        video: parsed.video,
        speed: None,
    })
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

    fn pick(image: u32, video: u32, vs: Option<VideoSpeed>) -> Pick {
        Pick {
            image_tokens: image,
            video_tokens: video,
            video: vs,
        }
    }

    const fn vs(window_s: u32, frames: u32, audio: bool) -> VideoSpeed {
        VideoSpeed {
            window_s,
            frames,
            audio,
        }
    }

    /// 所选视频样本的编号与预处理参数版本
    fn video_of(p: &Parsed) -> (&str, &str) {
        p.items
            .iter()
            .find_map(|i| match &i.source {
                Source::Video(_, version) => Some((i.id.as_str(), version.as_str())),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn parses_golden() {
        let (manifest, vectors) = golden_files();
        // 参考向量覆盖全部档位组合：图片 5 档、视频 3 个变体 × 2 档
        let v: serde_json::Value = serde_json::from_str(&vectors).unwrap();
        assert_eq!(v["vectors"].as_array().unwrap().len(), 49);
        let tiers = |key: &str| -> Vec<u32> {
            v[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_u64().unwrap() as u32)
                .collect()
        };
        assert_eq!(tiers("image_tiers"), [70, 140, 280, 560, 1120]);
        assert_eq!(tiers("video_frame_tiers"), [70, 140]);

        let p = parse(&manifest, &vectors, &pick(280, 140, Some(vs(2, 2, false)))).unwrap();
        let count = |f: fn(&Source) -> bool| p.items.iter().filter(|i| f(&i.source)).count();
        assert_eq!(
            (
                count(|s| matches!(s, Source::Text(_))),
                count(|s| matches!(s, Source::Image(_))),
                count(|s| matches!(s, Source::Audio(_))),
                count(|s| matches!(s, Source::Video(..)))
            ),
            (15, 5, 3, 1)
        );
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
        // 不同档位取不同的参考向量
        let other = parse(&manifest, &vectors, &pick(1120, 70, Some(vs(2, 2, false)))).unwrap();
        assert_ne!(p.items[15].want, other.items[15].want);
        assert_ne!(p.items[23].want, other.items[23].want);
        assert_eq!(p.items[0].want, other.items[0].want);

        // “找物体”：2 帧纯画面
        assert_eq!(p.video, Some(vs(2, 2, false)));
        assert_eq!(
            video_of(&p),
            (
                "vid01_f2_v",
                space::video_preprocess(2, 0, 2, 140, false).as_str()
            )
        );
        let Source::Video(inputs, _) = &p.items[23].source else {
            panic!("第 24 项应为视频窗口")
        };
        assert_eq!(inputs.len(), 2);
        assert!(inputs.iter().all(|x| matches!(x, VideoInput::Image { .. })));
        // “找说话内容”：6 帧带音频，时间戳、音频段、时间戳、画面帧
        let p = parse(&manifest, &vectors, &pick(280, 70, Some(vs(6, 6, true)))).unwrap();
        assert_eq!(
            video_of(&p),
            (
                "vid01_f6_av",
                space::video_preprocess(6, 0, 6, 70, true).as_str()
            )
        );
        let Source::Video(inputs, _) = &p.items[23].source else {
            panic!("第 24 项应为视频窗口")
        };
        assert_eq!(inputs.len(), 24);
        assert!(matches!(&inputs[0], VideoInput::Text { text } if text == "00:01"));
        assert!(matches!(&inputs[1], VideoInput::Audio { .. }));
        assert!(matches!(&inputs[3], VideoInput::Image { .. }));
    }

    #[test]
    fn picks_video_variant() {
        let (manifest, vectors) = golden_files();
        let picked = |video| {
            let p = parse(&manifest, &vectors, &pick(280, 140, video)).unwrap();
            (video_of(&p).0.to_string(), p.video.unwrap())
        };
        // 预设：帧数与音频相同的变体
        assert_eq!(picked(Some(vs(4, 4, false))).0, "vid01_f4_v");
        // 自定义：帧数与音频相同时取它，窗口长度不同照样取，记下的是变体自己的窗口
        assert_eq!(
            picked(Some(vs(8, 4, false))),
            ("vid01_f4_v".to_string(), vs(4, 4, false))
        );
        // 没有相同的变体时取默认预设“找物体”
        assert_eq!(picked(Some(vs(6, 6, false))).0, "vid01_f2_v");
        assert_eq!(picked(Some(vs(4, 4, true))).0, "vid01_f2_v");
        assert_eq!(picked(None).0, "vid01_f2_v");
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
        let err = |m: &str, v: &str| match parse(m, v, &pick(280, 140, None)) {
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
        // 参考向量中没有当前档位
        let missing = |p: Pick| match parse(&manifest, &vectors, &p) {
            Err(Error::Golden(msg)) => msg,
            _ => panic!("应当失败"),
        };
        assert!(missing(pick(100, 140, None)).contains("100 档"));
        assert!(missing(pick(280, 280, None)).contains("vid01_f2_v 的 280 档"));
    }
}
