use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rummage_proto::params::{self, ParamValue};

use super::*;
use crate::selftest::{self, Modality, VideoSpeed, golden};
use crate::{
    EmbeddingBackend, Limits, LiteRtArgs, LocalBackend, Priority, WorkerSpec, space, vector,
};

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

/// 开发环境中的推理子进程、运行库、模型与分词器（CPU 后端）；缺少时为空
fn dev_spec() -> Option<WorkerSpec> {
    let models = repo()
        .join("src-tauri/dev-data/models")
        .join(space::LITERT_MODEL_DIR);
    let model = models.join(space::LITERT_MODEL.name);
    let spec = WorkerSpec {
        exe: repo().join("target/debug/rummage-core.exe"),
        runtime_dir: repo().join(".cache/third-party/litert-lm/0.18.0/windows_x86_64"),
        model: model.clone(),
        mmproj: Some(model),
        threads: 4,
        gpu: None,
        litert: Some(LiteRtArgs {
            accel: Accel::Cpu,
            tokenizer: models.join(space::TOKENIZER.name),
            cache_dir: None,
        }),
    };
    let ok = spec.exe.is_file()
        && spec.runtime_dir.is_dir()
        && spec.model.is_file()
        && spec.litert.as_ref().unwrap().tokenizer.is_file();
    if !ok {
        eprintln!(
            "跳过：缺少 rummage-core.exe、LiteRT 运行库或模型（先 cargo build -p rummage-core，运行 scripts/fetch-litert.ps1 与 fetch-models.ps1 -Gguf none -LiteRt）"
        );
    }
    ok.then_some(spec)
}

#[test]
fn counts_with_special_tokens() {
    let Some(spec) = dev_spec() else { return };
    let tok = Tokenizer::from_file(&spec.litert.unwrap().tokenizer).unwrap();
    let ids = tok.encode("hello", true).unwrap().get_ids().to_vec();
    assert_eq!((ids.first(), ids.last()), (Some(&2), Some(&1)));
}

/// 黄金样本逐条经推理子进程用 LiteRT 计算：数值检查、与 LiteRT 参考向量的余弦（同一台电脑上应几乎相同）与单条耗时。
/// 图片分别按 70 与 140 档，视频按“找说话内容”（6 帧带音频）与“找物体”（2 帧）
#[test]
#[ignore = "需要本机的 LiteRT 运行库与模型"]
fn litert_golden() {
    let Some(spec) = dev_spec() else { return };
    let t = Instant::now();
    let backend = LocalBackend::new(spec, Limits::from_params(&params::defaults())).unwrap();
    backend.start().unwrap();
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
        let g = golden::load(&dir, &values, video, 100_000_000, golden::Refs::Litert).unwrap();
        let mut by: BTreeMap<String, Vec<(f64, f64)>> = BTreeMap::new();
        for s in g.samples {
            // 第一次用到某种媒体时引擎才加载对应的编码器，先算一遍预热
            let _ = backend.embed(s.input.clone(), Priority::Index).unwrap();
            let t = Instant::now();
            let e = backend.embed(s.input, Priority::Index).unwrap();
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(e.vector.len(), 768, "{}", s.id);
            let cos = vector::cosine(&e.vector, &s.want);
            assert!(cos > 0.9999, "{} 与参考向量的余弦 {cos}", s.id);
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
                "{label}：{} 条，耗时中位数 {:.0} 毫秒，与参考向量的余弦最小 {min_cos:.6}",
                v.len(),
                v[v.len() / 2].0
            );
        }
    }
}

/// 生成 LiteRT 的参考向量 `assets/golden/vectors-litert-<模型哈希前 16 位>.json`：经推理子进程用 LiteRT CPU 计算，
/// 图片与视频帧按 70、140 两档，视频按清单中的三个变体（与三个视频预设对应）。样本或预处理改动后重新生成
#[test]
#[ignore = "需要本机的 LiteRT 运行库与模型；会改写黄金样本目录中的参考向量"]
fn write_litert_golden() {
    let Some(spec) = dev_spec() else { return };
    let backend = LocalBackend::new(spec, Limits::from_params(&params::defaults())).unwrap();
    let dir = repo().join("assets/golden");
    let variants = [(2, 2, false), (4, 4, false), (6, 6, true)];
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut seen: Vec<(String, Option<u32>)> = Vec::new();
    for tier in TIERS {
        for (window_s, frames, audio) in variants {
            let mut values: BTreeMap<String, ParamValue> = params::defaults();
            values.insert("image_tokens".into(), ParamValue::Int(tier.into()));
            values.insert("video_frame_tokens".into(), ParamValue::Int(tier.into()));
            let video = VideoSpeed {
                window_s,
                frames,
                audio,
            };
            let g = golden::inputs(&dir, &values, video, u64::MAX).unwrap();
            for s in g.samples {
                let (modality, tokens) = match &s.modality {
                    Modality::Text => ("text", None),
                    Modality::Image => ("image", Some(tier)),
                    Modality::Audio => ("audio", None),
                    Modality::Video if audio => ("video_av", Some(tier)),
                    Modality::Video => ("video_v", Some(tier)),
                };
                if seen.contains(&(s.id.clone(), tokens)) {
                    continue;
                }
                let e = backend.embed(s.input, Priority::Index).unwrap();
                assert_eq!(e.vector.len(), 768, "{}", s.id);
                seen.push((s.id.clone(), tokens));
                out.push(serde_json::json!({
                    "id": s.id,
                    "modality": modality,
                    "tokens": tokens,
                    "vector": e.vector,
                }));
            }
        }
    }
    let file = serde_json::json!({
        "format": "每条向量是 768 个 float32（以能无损还原为 float32 的最短十进制写出），已 L2 归一化。",
        "runtime": "litert-lm 0.18.0（PyPI litert-lm-api 轮子中的 litert-lm.dll），CPU 后端，激活 FLOAT32，经 rummage-core worker --litert cpu 计算",
        "model_sha256": space::LITERT_MODEL.sha256,
        "mmproj_sha256": space::LITERT_MODEL.sha256,
        "image_tiers": TIERS,
        "video_frame_tiers": TIERS,
        "manifest_sha256": sha256_hex(&std::fs::read(dir.join("manifest.json")).unwrap()),
        "vectors": out,
    });
    let path = dir.join(golden::Refs::Litert.file_name());
    std::fs::write(
        &path,
        serde_json::to_string(&file).unwrap()
            + "
",
    )
    .unwrap();
    eprintln!("已写入 {}，{} 条", path.display(), seen.len());
}

/// 自检走 LiteRT（`Options::litert`）：CPU 候选经推理子进程计算，与 LiteRT 参考向量比较后通过并有测速结果
#[test]
#[ignore = "需要本机的 LiteRT 运行库与模型"]
fn litert_selftest_cpu() {
    let Some(spec) = dev_spec() else { return };
    let l = spec.litert.clone().unwrap();
    let dir = std::env::temp_dir().join(format!("rummage-litert-selftest-{}", std::process::id()));
    let files = selftest::LiteRtFiles {
        runtime_dir: spec.runtime_dir.clone(),
        tokenizer: l.tokenizer,
        cache_dir: dir.join("litert"),
        npu: None,
    };
    let opts = selftest::Options {
        exe: spec.exe.clone(),
        golden_dir: repo().join("assets/golden"),
        cpu_runtime: PathBuf::new(),
        vulkan_runtime: None,
        model: spec.model.clone(),
        mmproj: spec.model.clone(),
        cache_file: dir.join("selftest.json"),
        use_cache: false,
        save: true,
        images: true,
        auto: false,
        cancel: Default::default(),
        only: None,
        video: VideoSpeed {
            window_s: 2,
            frames: 2,
            audio: false,
        },
        litert: Some(files),
    };
    let mut values = params::defaults();
    values.insert("image_tokens".into(), ParamValue::Int(140));
    let report = selftest::run(
        &opts,
        &[selftest::Candidate::Cpu],
        &values,
        &[],
        &mut |_, _| {},
    )
    .unwrap();
    let r = &report.candidates[0];
    assert_eq!(r.status, selftest::Status::Passed, "{:?}", r.reason);
    assert!(r.samples.iter().all(|s| s.cosine > 0.9999));
    assert!(r.speed.is_some());
    assert_eq!(report.selected, Some(0));
    let _ = std::fs::remove_dir_all(&dir);
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
