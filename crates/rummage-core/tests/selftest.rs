//! 用 CPU 跑 `rummage-core selftest --skip-gpu --no-cache`，检查全部样本通过、报告含耗时、结果写入缓存。
//! 需要本机的 CPU 运行库与模型，缺少时跳过并提示；只用 `win-cpu-x64` 运行库，不加载任何 GPU 后端：
//!   cargo test -p rummage-core --test selftest -- --ignored --nocapture

use std::path::Path;
use std::process::Command;

#[test]
#[ignore = "需要本机的运行库与模型，CPU 推理耗时较长"]
fn cpu_selftest_passes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let runtime_dir = root.join(".cache/third-party/llama.cpp/b11461/win-cpu-x64");
    let model_dir = root.join("src-tauri/dev-data/models/embeddinggemma-2-gguf");
    for p in [&runtime_dir, &model_dir] {
        if !p.exists() {
            println!("跳过：找不到 {}", p.display());
            return;
        }
    }
    let data_dir =
        std::env::temp_dir().join(format!("rummage-core-selftest-{}", std::process::id()));
    let out = Command::new(env!("CARGO_BIN_EXE_rummage-core"))
        .arg("selftest")
        .arg("--data-dir")
        .arg(&data_dir)
        .arg("--golden")
        .arg(root.join("assets/golden"))
        .arg("--runtime-dir")
        .arg(&runtime_dir)
        .arg("--model-dir")
        .arg(&model_dir)
        .args(["--skip-gpu", "--no-cache"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    println!("{stdout}");
    eprintln!("{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "退出状态 {}", out.status);
    assert!(stdout.contains("[通过] CPU："), "CPU 应通过");
    assert!(!stdout.contains("[未通过]"));
    assert!(stdout.contains("选中：CPU"));
    // 15 条文本、5 张图像、3 段音频与 2 个视频窗口，CPU 与参考向量同一后端，余弦应接近 1。
    let cosines: Vec<f64> = stdout
        .lines()
        .filter_map(|l| l.split("余弦 ").nth(1))
        .map(|v| v.trim().parse().unwrap())
        .collect();
    assert_eq!(cosines.len(), 25);
    assert!(cosines.iter().all(|&c| c >= 0.9999), "{cosines:?}");
    let timing = stdout
        .lines()
        .find(|l| l.contains("单条耗时中位数"))
        .unwrap();
    assert_eq!(timing.matches("毫秒").count(), 5, "{timing}");

    let cache = std::fs::read_to_string(data_dir.join("cache/selftest.json")).unwrap();
    let cache: serde_json::Value = serde_json::from_str(&cache).unwrap();
    assert_eq!(cache["cached"]["report"]["selected"], 0);
    assert_eq!(cache["blacklist"].as_array().unwrap().len(), 0);
    std::fs::remove_dir_all(&data_dir).unwrap();
}
