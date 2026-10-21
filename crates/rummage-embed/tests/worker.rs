//! 用 CPU 推理子进程跑黄金样本，并测试推理进程的重启与超时。需要本机的运行库、模型和已构建的 rummage-core：
//!   cargo build -p rummage-core
//!   cargo test -p rummage-embed --test worker -- --ignored --nocapture
//! 缺少任何一项时跳过并提示。只用 `win-cpu-x64` 运行库，不加载任何 GPU 后端。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use rummage_embed::{
    EmbeddingBackend, Error, Input, Limits, LocalBackend, MAX_FAILURES, Priority, SpaceConfig,
    WorkerSpec, space, vector,
};

/// 同一后端的一致门槛。
const MIN_COSINE: f64 = 0.9999;
const THREADS: u32 = 6;
/// 取这一档的图像黄金向量
const IMAGE_TOKENS: u32 = 280;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// 缺少文件时返回 `None` 并提示。
fn spec(with_mmproj: bool) -> Option<WorkerSpec> {
    let root = root();
    let exe = root.join(format!(
        "target/debug/rummage-core{}",
        std::env::consts::EXE_SUFFIX
    ));
    let runtime_dir = root.join(".cache/third-party/llama.cpp/b11461/win-cpu-x64");
    let models = root.join("src-tauri/dev-data/models/embeddinggemma-2-gguf");
    let model = models.join("embeddinggemma-2-Q8_0.gguf");
    let mmproj = models.join("mmproj-embeddinggemma-2-Q8_0.gguf");
    for p in [&exe, &runtime_dir, &model, &mmproj] {
        if !p.exists() {
            println!(
                "跳过：找不到 {}（rummage-core 需先 cargo build -p rummage-core）",
                p.display()
            );
            return None;
        }
    }
    Some(WorkerSpec {
        exe,
        runtime_dir,
        model,
        mmproj: with_mmproj.then_some(mmproj),
        threads: THREADS,
        gpu: None,
        litert: None,
    })
}

fn default_limits() -> Limits {
    Limits::from_params(&BTreeMap::new())
}

fn json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
#[ignore = "需要本机的运行库、模型与已构建的 rummage-core，CPU 推理耗时较长"]
fn golden_texts_and_images() {
    let Some(spec) = spec(true) else { return };
    let golden_dir = root().join("assets/golden");
    let manifest = json(&golden_dir.join("manifest.json"));
    let golden = json(&golden_dir.join(format!(
        "vectors-gguf-q8-{}.json",
        &space::MODEL.sha256[..16]
    )));
    // 图像按 `IMAGE_TOKENS` 档取，文本没有档位
    let want = |id: &str, tokens: Option<u32>| -> Vec<f32> {
        golden["vectors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == id && v["tokens"].as_u64() == tokens.map(u64::from))
            .unwrap_or_else(|| panic!("黄金向量中没有 {id}"))["vector"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_f64().unwrap() as f32)
            .collect()
    };
    let backend = LocalBackend::new(spec, default_limits()).unwrap();
    let cfg = SpaceConfig::current(space::Backend::Cpu);

    let mut min_text = f64::INFINITY;
    for t in manifest["texts"].as_array().unwrap() {
        let id = t["id"].as_str().unwrap();
        let text = t["text"].as_str().unwrap().to_string();
        let input = match t["kind"].as_str().unwrap() {
            "query" => Input::Query(text),
            "document" => Input::Document {
                title: t["title"].as_str().map(str::to_string),
                text,
            },
            kind => panic!("未知的文本类型 {kind}"),
        };
        let got = backend.embed(input, Priority::Query).unwrap();
        assert_eq!(got.space, cfg.text());
        let cos = vector::cosine(&got.vector, &want(id, None));
        println!("{id:20} 余弦 {cos:.9}");
        assert!(cos >= MIN_COSINE, "{id} 余弦 {cos} 低于 {MIN_COSINE}");
        min_text = min_text.min(cos);
    }

    let rummage_proto::ParamValue::Int(mp) =
        (rummage_proto::params::def("max_image_mp").unwrap().default)()
    else {
        panic!("max_image_mp 应为整数")
    };
    let max_pixels = mp as u64 * 1_000_000;
    let mut min_image = f64::INFINITY;
    for i in manifest["images"].as_array().unwrap() {
        let id = i["id"].as_str().unwrap();
        let file = golden_dir.join(i["file"].as_str().unwrap());
        let img = rummage_media::prepare(&file, IMAGE_TOKENS, max_pixels).unwrap();
        let input = Input::Image {
            rgb: img,
            tokens: IMAGE_TOKENS,
        };
        let got = backend.embed(input, Priority::Index).unwrap();
        assert_eq!(got.space, cfg.image(IMAGE_TOKENS));
        let cos = vector::cosine(&got.vector, &want(id, Some(IMAGE_TOKENS)));
        println!("{id:20} 余弦 {cos:.9}");
        assert!(cos >= MIN_COSINE, "{id} 余弦 {cos} 低于 {MIN_COSINE}");
        min_image = min_image.min(cos);
    }
    let device = backend.device().unwrap();
    println!(
        "设备 {}（{}）；文本最小余弦 {min_text:.9}，图像最小余弦 {min_image:.9}",
        device.name, device.description
    );
    assert_eq!(device.name, "CPU");
}

fn kill_pid(pid: u32) {
    let status = if cfg!(windows) {
        Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .status()
    } else {
        Command::new("kill").args(["-9", &pid.to_string()]).status()
    };
    assert!(status.unwrap().success());
}

#[test]
#[ignore = "需要本机的运行库、模型与已构建的 rummage-core"]
fn restarts_after_kill() {
    let Some(spec) = spec(false) else { return };
    let backend = LocalBackend::new(spec, default_limits()).unwrap();
    let query = || Input::Query("猫".to_string());
    let first = backend.embed(query(), Priority::Query).unwrap();
    let pid = backend.worker_pid().unwrap();
    kill_pid(pid);
    // 等系统把进程标记为已退出。
    std::thread::sleep(Duration::from_millis(500));
    let second = backend.embed(query(), Priority::Query).unwrap();
    let new_pid = backend.worker_pid().unwrap();
    assert_ne!(new_pid, pid);
    assert!(vector::cosine(&first.vector, &second.vector) >= MIN_COSINE);
    println!("推理进程 {pid} 被结束后自动重启为 {new_pid}");
    // 只加载主模型时不能编码图像。
    let img = rummage_media::Rgb {
        width: 48,
        height: 48,
        data: vec![0; 48 * 48 * 3],
    };
    let err = backend
        .embed(
            Input::Image {
                rgb: img,
                tokens: IMAGE_TOKENS,
            },
            Priority::Index,
        )
        .unwrap_err();
    assert!(matches!(err, Error::NoMmproj), "{err}");
}

fn wait_unloaded(backend: &LocalBackend, within: Duration) {
    let deadline = std::time::Instant::now() + within;
    while backend.worker_pid().is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "空闲后没有结束推理进程"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[ignore = "需要本机的运行库、模型与已构建的 rummage-core，CPU 推理耗时较长"]
fn idle_unload_and_mmproj_on_demand() {
    let Some(spec) = spec(true) else { return };
    let limits = Limits {
        idle: Duration::from_secs(3),
        ..default_limits()
    };
    let backend = LocalBackend::new(spec, limits).unwrap();
    let corgi = root().join("assets/golden/images/img02_photo_corgi.jpg");
    let image = || Input::Image {
        rgb: rummage_media::prepare(&corgi, IMAGE_TOKENS, 100_000_000).unwrap(),
        tokens: IMAGE_TOKENS,
    };
    let query = || Input::Query("猫".to_string());

    // 默认只加载主模型；遇到图像时带上 mmproj 重启，之后保持
    backend.embed(query(), Priority::Query).unwrap();
    let text_pid = backend.worker_pid().unwrap();
    backend.embed(image(), Priority::Index).unwrap();
    let image_pid = backend.worker_pid().unwrap();
    assert_ne!(image_pid, text_pid);
    backend.embed(query(), Priority::Query).unwrap();
    backend.embed(image(), Priority::Index).unwrap();
    assert_eq!(backend.worker_pid(), Some(image_pid));
    println!("只加载主模型的进程 {text_pid}，带上 mmproj 后为 {image_pid}");

    // 空闲超时后结束；之后的请求重新拉起。重启和卸载都不计失败：循环次数超过连续失败上限仍然正常
    for k in 0..=MAX_FAILURES {
        wait_unloaded(&backend, Duration::from_secs(20));
        backend.embed(query(), Priority::Query).unwrap();
        let pid = backend.worker_pid().unwrap();
        backend.embed(image(), Priority::Index).unwrap();
        assert_ne!(backend.worker_pid(), Some(pid));
        println!("第 {} 轮：空闲卸载后重新拉起正常", k + 1);
    }

    // 修改空闲时限立即生效：改得很长后不卸载，改短后很快卸载
    backend.set_idle_timeout(Duration::from_secs(3600));
    std::thread::sleep(Duration::from_secs(5));
    assert!(backend.worker_pid().is_some());
    backend.set_idle_timeout(Duration::from_millis(200));
    wait_unloaded(&backend, Duration::from_secs(5));
}

#[test]
#[ignore = "需要本机的运行库、模型与已构建的 rummage-core"]
fn infer_timeout_kills_worker() {
    let Some(spec) = spec(false) else { return };
    let limits = Limits {
        infer_timeout: Duration::from_millis(1),
        ..default_limits()
    };
    let backend = LocalBackend::new(spec, limits).unwrap();
    for k in 0..MAX_FAILURES {
        let err = backend
            .embed(Input::Query("猫".to_string()), Priority::Query)
            .unwrap_err();
        println!("第 {} 次：{err}", k + 1);
        assert!(matches!(err, Error::InferTimeout(_)), "{err}");
        assert!(backend.worker_pid().is_none());
    }
    // 连续失败后不再拉起推理进程。
    let err = backend
        .embed(Input::Query("猫".to_string()), Priority::Query)
        .unwrap_err();
    println!("之后：{err}");
    assert!(matches!(err, Error::Unavailable(_)), "{err}");
}
