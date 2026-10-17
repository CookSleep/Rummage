use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rummage_proto::params::{self, ParamValue};

use super::*;
use crate::selftest::{Modality, VideoSpeed, golden};

#[test]
fn fixed_size_is_stable() {
    for tier in TIERS {
        for (w, h) in [
            (1536, 1152),
            (1152, 1536),
            (1000, 1),
            (1, 1000),
            (4000, 300),
            (333, 777),
            (1080, 2400),
            (48, 48),
            (5000, 5000),
        ] {
            // 先按 GGUF 的规则缩放（与流水线相同），再求不动点
            let fitted = rummage_media::fit(
                Rgb {
                    width: w,
                    height: h,
                    data: vec![0; (w * h * 3) as usize],
                },
                tier,
            )
            .unwrap();
            let (fw, fh) = fixed_size(fitted.width, fitted.height, tier).unwrap();
            assert_eq!(
                litert_size(fw, fh, tier),
                Some((fw, fh)),
                "{w}×{h} {tier} 档"
            );
            assert_eq!((fw % SIDE, fh % SIDE), (0, 0));
            assert!(((fw / SIDE) * (fh / SIDE)) as usize <= tier as usize);
            // 不动点只会放大，不会比 GGUF 的尺寸小
            assert!(
                fw >= fitted.width && fh >= fitted.height,
                "{w}×{h} {tier} 档"
            );
        }
    }
    // 方图 140 档：GGUF 与 LiteRT 的尺寸相同
    assert_eq!(fixed_size(528, 528, 140), Some((528, 528)));
}

#[test]
fn tiers() {
    let img = |w: u32, h: u32| Rgb {
        width: w,
        height: h,
        data: Vec::new(),
    };
    assert_eq!(tier_of(&img(480, 480)).unwrap(), 140);
    assert_eq!(tier_of(&img(384, 384)).unwrap(), 70);
    assert!(tier_of(&img(960, 960)).is_err());
    assert!(check_tier(&img(480, 480), 280).is_err());
    assert!(check_tier(&img(480, 480), 70).is_err());
    assert_eq!(check_tier(&img(480, 480), 140).unwrap(), 140);
}

#[test]
fn wav_header() {
    let b = wav(&[0.5, -0.25]);
    assert_eq!(b.len(), 44 + 8);
    assert_eq!(&b[..4], b"RIFF");
    assert_eq!(u32::from_le_bytes(b[4..8].try_into().unwrap()), 36 + 8);
    assert_eq!(&b[8..16], b"WAVEfmt ");
    assert_eq!(u16::from_le_bytes([b[20], b[21]]), 3);
    assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 16_000);
    assert_eq!(&b[36..40], b"data");
    assert_eq!(f32::from_le_bytes(b[44..48].try_into().unwrap()), 0.5);
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// 开发环境中的运行库、模型与分词器；缺少时为空
fn dev_spec() -> Option<LiteRtSpec> {
    let models = repo()
        .join("src-tauri/dev-data/models")
        .join(space::LITERT_MODEL_DIR);
    let spec = LiteRtSpec {
        runtime_dir: repo().join(".cache/third-party/litert-lm/0.18.0/windows_x86_64"),
        model: models.join(space::LITERT_MODEL.name),
        tokenizer: models.join(space::TOKENIZER.name),
        threads: std::thread::available_parallelism().map_or(4, |n| n.get() as u32),
    };
    let ok = spec.runtime_dir.is_dir() && spec.model.is_file() && spec.tokenizer.is_file();
    if !ok {
        eprintln!(
            "跳过：缺少 LiteRT 运行库或模型（先运行 scripts/fetch-litert.ps1 与 fetch-models.ps1 -Gguf none -LiteRt）"
        );
    }
    ok.then_some(spec)
}

#[test]
fn counts_with_special_tokens() {
    let Some(spec) = dev_spec() else { return };
    let tok = Tokenizer::from_file(&spec.tokenizer).unwrap();
    let ids = tok.encode("hello", true).unwrap().get_ids().to_vec();
    assert_eq!((ids.first(), ids.last()), (Some(&2), Some(&1)));
}

/// 黄金样本逐条用 LiteRT 计算：数值检查、与 GGUF 参考向量的余弦（不同空间，只作参考）与单条耗时。
/// 图片分别按 70 与 140 档，视频按“找说话内容”（6 帧带音频）与“找物体”（2 帧）
#[test]
#[ignore = "需要本机的 LiteRT 运行库与模型"]
fn litert_golden() {
    let Some(spec) = dev_spec() else { return };
    let t = Instant::now();
    let backend = LiteRtBackend::new(&spec).unwrap();
    eprintln!("加载：{:.1} 秒", t.elapsed().as_secs_f64());
    let dir = repo().join("assets/golden");
    for (tier, video) in [
        (
            140,
            VideoSpeed {
                window_s: 6,
                frames: 6,
                audio: true,
            },
        ),
        (
            70,
            VideoSpeed {
                window_s: 2,
                frames: 2,
                audio: false,
            },
        ),
    ] {
        let mut values: BTreeMap<String, ParamValue> = params::defaults();
        values.insert("image_tokens".into(), ParamValue::Int(tier));
        values.insert("video_frame_tokens".into(), ParamValue::Int(tier));
        let g = golden::load(&dir, &values, video, 100_000_000).unwrap();
        let mut by: BTreeMap<String, Vec<(f64, f64)>> = BTreeMap::new();
        for s in g.samples {
            // 第一次用到某种媒体时引擎才加载对应的编码器，先算一遍预热
            let _ = backend.embed(s.input.clone(), Priority::Index).unwrap();
            let t = Instant::now();
            let e = backend.embed(s.input, Priority::Index).unwrap();
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(e.vector.len(), 768, "{}", s.id);
            let cos = vector::cosine(&e.vector, &s.want);
            let label = match s.modality {
                Modality::Text => "文本".to_string(),
                Modality::Image => format!("图像 {tier} 档"),
                Modality::Audio => "音频".to_string(),
                Modality::Video => format!("视频 {} 帧 {tier} 档", video.frames),
            };
            by.entry(label).or_default().push((ms, cos));
        }
        for (label, mut v) in by {
            v.sort_by(|a, b| a.0.total_cmp(&b.0));
            let min_cos = v.iter().map(|x| x.1).fold(f64::INFINITY, f64::min);
            eprintln!(
                "{label}：{} 条，耗时中位数 {:.0} 毫秒，与 GGUF 参考向量的余弦最小 {min_cos:.4}",
                v.len(),
                v[v.len() / 2].0
            );
        }
    }
}
