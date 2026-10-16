//! 黄金视频窗口的两层核对，在指定显卡上计算，推理在可强行结束的推理子进程中进行（`LocalBackend`）：
//! 1. 黄金样本的输入是预先抽好的 JPEG 帧与 WAV：按 `rummage_media::video::assemble` 组装（帧按 70 档缩放，
//!    取“找说话内容”变体 `vid01_f6_av` 的 70 档黄金向量），
//!    向量与黄金向量的余弦须 ≥ 0.999，核对组装顺序、时间戳与音频段；
//! 2. 改用建索引的做法，从原视频（在仓库外）一次解码抽帧、解码音轨，按同一窗口（1–7 秒）
//!    组装，只报告余弦：取帧时刻与缩放、JPEG 压缩都与黄金样本不同（见 `rummage_media::video`）。
//!
//! 需要本机的运行库、模型、ffmpeg 与原视频，缺少时跳过：
//!   $env:RUMMAGE_GPU="4080 SUPER"; cargo test -p rummage-core --release --test video_golden -- --ignored --nocapture

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rummage_embed::space::{MMPROJ, MODEL, MODEL_DIR};
use rummage_embed::{
    EmbeddingBackend, Gpu, Input, Limits, LocalBackend, Priority, WorkerSpec, list_devices, vector,
};
use rummage_media::video::{self, Window, WindowOpts};
use rummage_media::{audio, ffmpeg};

#[test]
#[ignore = "需要本机的运行库、模型、ffmpeg、原视频与显卡"]
fn ffmpeg_frames_near_golden_vectors() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let runtime = root.join(".cache/third-party/llama.cpp/b11461/win-vulkan-x64");
    let models = root.join("src-tauri/dev-data/models").join(MODEL_DIR);
    let ff = root
        .join(".cache/third-party/ffmpeg")
        .join(ffmpeg::FFMPEG_BUILD);
    let src = root.join(".cache/golden/vid01.mp4");
    for p in [&runtime, &models, &ff, &src] {
        if !p.exists() {
            println!("跳过：找不到 {}", p.display());
            return;
        }
    }
    let Ok(want_gpu) = std::env::var("RUMMAGE_GPU") else {
        println!("跳过：没有用 RUMMAGE_GPU 指定显卡");
        return;
    };
    let exe = Path::new(env!("CARGO_BIN_EXE_rummage-core"));
    let limits = Limits::from_params(&BTreeMap::new());
    let gpu = list_devices(exe, &runtime, &limits)
        .unwrap()
        .into_iter()
        .find(|g| g.description.contains(&want_gpu))
        .expect("找不到指定的显卡");
    let backend = LocalBackend::new(
        WorkerSpec {
            exe: exe.to_path_buf(),
            runtime_dir: runtime.clone(),
            model: models.join(MODEL.name),
            mmproj: Some(models.join(MMPROJ.name)),
            threads: 6,
            gpu: Some(Gpu {
                description: gpu.description.clone(),
                pci_id: gpu.pci_id.clone(),
            }),
        },
        limits,
    )
    .unwrap();

    // 黄金样本：窗口从 1 秒起 6 秒、6 帧，时间戳为 00:01 … 00:07（assets/golden/video/vid01/window.json）
    // 取这一档的黄金向量
    const TOKENS: u32 = 70;
    let opts = WindowOpts {
        window_s: 6,
        overlap_s: 0,
        frames: 6,
    };
    let info = video::probe(&ff, &src, Duration::from_secs(30)).unwrap();
    let size = video::frame_size(info.width, info.height, TOKENS).unwrap();
    let t = Instant::now();
    let frames: Vec<Arc<rummage_media::Rgb>> = video::extract(
        &ff,
        &src,
        size,
        opts.grid(),
        100,
        true,
        Some(1.0),
        Duration::from_secs(60),
    )
    .unwrap()
    .into_iter()
    .map(Arc::new)
    .collect();
    println!(
        "抽帧 {} 帧，{:.2} 秒",
        frames.len(),
        t.elapsed().as_secs_f64()
    );
    let samples = audio::decode(&ff, &src, 0, usize::MAX, Duration::from_secs(60)).unwrap();
    let win = Window {
        start_ms: 1000,
        end_ms: 7000,
        len_ms: 6000,
        frames: (0..6).collect(),
        stamps: ["00:01", "00:02", "00:03", "00:04", "00:05", "00:07"]
            .map(String::from)
            .to_vec(),
    };
    let golden: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(format!(
            "assets/golden/vectors-gguf-q8-{}.json",
            &MODEL.sha256[..16]
        )))
        .unwrap(),
    )
    .unwrap();
    let dir = root.join("assets/golden/video/vid01");
    let golden_frames: Vec<Arc<rummage_media::Rgb>> = (0..6)
        .map(|i| {
            let path = dir.join(format!("frame{i}.jpg"));
            Arc::new(rummage_media::prepare(&path, TOKENS, 100_000_000).unwrap())
        })
        .collect();
    // 黄金 WAV 按 x / 32768 读出，与 ffmpeg 解码 16 位 PCM 逐位相同（rummage_media::audio 的测试），首尾相接即窗口音频
    let golden_audio: Vec<f32> = (0..6)
        .flat_map(|i| {
            audio::decode(
                &ff,
                &dir.join(format!("audio{i}.wav")),
                0,
                usize::MAX,
                Duration::from_secs(30),
            )
            .unwrap()
        })
        .collect();
    let golden_win = Window {
        start_ms: 0,
        ..win.clone()
    };
    let id = "vid01_f6_av";
    let want: Vec<f32> = golden["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == id && v["tokens"] == TOKENS)
        .unwrap()["vector"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap() as f32)
        .collect();
    let embed = |parts| {
        let input = Input::Video {
            parts,
            preprocess: rummage_embed::space::video_preprocess(6, 0, 6, TOKENS, true),
            max_tokens: 1024,
        };
        let t = Instant::now();
        let got = backend.embed(input, Priority::Index).unwrap().vector;
        (
            vector::cosine(&got, &want),
            t.elapsed().as_secs_f64() * 1000.0,
        )
    };
    let (cos, ms) = embed(video::assemble(
        &golden_win,
        &golden_frames,
        Some(&golden_audio[..]),
    ));
    println!("{id}：黄金帧组装，与黄金向量的余弦 {cos:.6}，推理 {ms:.0} 毫秒");
    assert!(cos >= 0.999, "{id} 黄金帧组装的余弦 {cos}");
    let (cos, ms) = embed(video::assemble(&win, &frames, Some(&samples[..])));
    println!("{id}：一次解码抽帧，与黄金向量的余弦 {cos:.6}，推理 {ms:.0} 毫秒");
    assert!(cos > 0.95, "{id} 余弦 {cos}");
}
