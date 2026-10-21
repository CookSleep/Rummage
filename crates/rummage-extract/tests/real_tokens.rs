//! 用 CPU 推理子进程的真实分词计数切一段自造的中文样本，核对每片都不超过上限、大部分接近目标长度。
//! 需要本机的运行库、模型和已构建的 rummage-core：
//!   cargo build -p rummage-core
//!   cargo test -p rummage-extract --test real_tokens -- --ignored --nocapture
//! 缺少任何一项时跳过并提示。只用 `win-cpu-x64` 运行库，不加载任何 GPU 后端。

use std::collections::BTreeMap;
use std::path::Path;

use rummage_embed::{EmbeddingBackend, Input, Limits, LocalBackend, Priority, WorkerSpec};
use rummage_extract::{ChunkOpts, MAX_TOKENS, TextKind, chunk};

const SENTENCES: &[&str] = &[
    "本地语义搜索的第一步是扫描用户选择的文件夹，记录每个文件的路径、大小和修改时间。",
    "扫描时会跳过系统目录、临时文件和网盘中尚未下载的占位文件，避免触发不必要的下载。",
    "读取正文时先判断编码，带字节顺序标记的按标记解码，其余的先尝试 UTF-8，再猜测是否为国标码。",
    "切片按标题和段落进行，较长的段落再按句子切开。",
    "每个片段都记录在原文中的字符位置和行号，搜索结果可以直接定位到对应的段落。",
    "向量由嵌入模型计算，查询和文档使用不同的任务前缀，这一点对检索效果影响很大。",
    "如果某个文件处理失败，只记录错误并继续处理下一个，不会中断整个索引任务。",
    "索引完成后，用户输入一句自然语言描述，程序就能找出意思相近的文档、图片和音视频。",
    "为了节省内存，检索时先用较短的向量粗排，再用完整的向量精排。",
    "所有数据都保存在本机，不会上传到任何服务器。",
    "当电脑使用电池供电时，后台索引会自动暂停，以免影响续航。",
    "用户可以在设置中调整切片长度和重叠比例，修改后需要重建受影响的索引。",
];

#[test]
#[ignore = "需要本机的运行库、模型与已构建的 rummage-core"]
fn real_token_counts() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let exe = root.join(format!(
        "target/debug/rummage-core{}",
        std::env::consts::EXE_SUFFIX
    ));
    let runtime_dir = root.join(".cache/third-party/llama.cpp/b11461/win-cpu-x64");
    let model =
        root.join("src-tauri/dev-data/models/embeddinggemma-2-gguf/embeddinggemma-2-Q8_0.gguf");
    for p in [&exe, &runtime_dir, &model] {
        if !p.exists() {
            println!(
                "跳过：找不到 {}（rummage-core 需先 cargo build -p rummage-core）",
                p.display()
            );
            return;
        }
    }
    let spec = WorkerSpec {
        exe,
        runtime_dir,
        model,
        mmproj: None,
        threads: 6,
        gpu: None,
        litert: None,
    };
    let backend = LocalBackend::new(spec, Limits::from_params(&BTreeMap::new())).unwrap();

    // 约 3000 字：句子轮流取用，每段 3 到 7 句。
    let mut text = String::new();
    let mut i = 0;
    let mut para = 0;
    while text.chars().count() < 3000 {
        for _ in 0..3 + para % 5 {
            text.push_str(SENTENCES[i % SENTENCES.len()]);
            i += 1;
        }
        text.push_str("\n\n");
        para += 1;
    }
    println!("样本 {} 字，{para} 段", text.chars().count());

    let title = "索引流程说明";
    let input = |s: &str| Input::Document {
        title: Some(title.to_string()),
        text: s.to_string(),
    };
    let opts = ChunkOpts::from_params(&BTreeMap::new());
    let mut calls = 0;
    let mut count = |s: &str| {
        calls += 1;
        backend.count_tokens(&input(s), Priority::Index)
    };
    let chunks = chunk(&text, TextKind::Text, &opts, &mut count).unwrap();
    println!("计数调用 {calls} 次");

    let mut counts = Vec::new();
    for c in &chunks {
        let n = backend
            .count_tokens(&input(&c.text), Priority::Index)
            .unwrap();
        println!(
            "字符 {:5}..{:5}  行 {:3}..{:3}  {n} token",
            c.char_start, c.char_end, c.line_start, c.line_end
        );
        assert!(n <= MAX_TOKENS, "片段有 {n} 个 token，超过上限");
        counts.push(n);
    }
    // 最后一片可以较短；其余大部分应在目标的 75% 以上。
    let body = &counts[..counts.len() - 1];
    let near = body.iter().filter(|&&n| n * 4 >= opts.target * 3).count();
    println!(
        "{} 片，除最后一片外 {near}/{} 片达到目标的 75%",
        counts.len(),
        body.len()
    );
    assert!(chunks.len() >= 3);
    assert!(
        near * 3 >= body.len() * 2,
        "接近目标长度的片段太少：{counts:?}"
    );
}
