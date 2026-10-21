//! 进程优先级：核心进程降为 `BELOW_NORMAL` 后，CPU 推理子进程继承这个优先级类。
//! 测试进程本身代替核心进程调用 `lower_priority`，再用 `LocalBackend` 拉起 `rummage-core worker`。
//! 需要本机的 CPU 运行库与模型，缺少时跳过并提示；只用 `win-cpu-x64` 运行库，不加载任何 GPU 后端：
//!   cargo test -p rummage-core --test resource -- --ignored --nocapture
#![cfg(windows)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use rummage_embed::{Limits, LocalBackend, WorkerSpec};

/// 用 PowerShell 读取进程的优先级类（如 `BelowNormal`）
fn priority_class(pid: u32) -> String {
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!("(Get-Process -Id {pid}).PriorityClass"),
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
#[ignore = "需要本机的运行库与模型"]
fn worker_inherits_below_normal() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let runtime_dir = root.join(".cache/third-party/llama.cpp/b11461/win-cpu-x64");
    let model =
        root.join("src-tauri/dev-data/models/embeddinggemma-2-gguf/embeddinggemma-2-Q8_0.gguf");
    for p in [&runtime_dir, &model] {
        if !p.exists() {
            println!("跳过：找不到 {}", p.display());
            return;
        }
    }
    rummage_core::lower_priority().unwrap();
    assert_eq!(priority_class(std::process::id()), "BelowNormal");
    let spec = WorkerSpec {
        exe: PathBuf::from(env!("CARGO_BIN_EXE_rummage-core")),
        runtime_dir,
        model,
        mmproj: None,
        threads: 2,
        gpu: None,
        litert: None,
    };
    let backend = LocalBackend::new(spec, Limits::from_params(&BTreeMap::new())).unwrap();
    backend.start().unwrap();
    let pid = backend.worker_pid().unwrap();
    let class = priority_class(pid);
    println!("推理子进程 {pid} 的优先级类：{class}");
    assert_eq!(class, "BelowNormal");
}
