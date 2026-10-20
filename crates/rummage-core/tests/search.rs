//! 用 CPU 跑完整的核心进程建索引后检索：样本与 `tests/pipeline.rs` 第 1 部分相同
//! （自造的 `.txt` / `.md`，加上从 `assets/golden/images` 复制的柯基（两份）与小票），核对：
//! - 文本查询“柯基在草地上接飞盘”时柯基图片排第一；
//! - 与 `设计说明.md` 某段内容对应的查询，该文档排进前 3；
//! - 文件名的一部分（不少于 3 个字走 FTS，不足 3 个字走 LIKE）能命中；
//! - 用小票图片以图搜图，排第一的是它自己或它的副本；
//!
//! 并打印各查询的耗时。需要本机的 CPU 运行库与模型，缺少时跳过并提示；模型以硬链接放进临时数据目录，只用 CPU：
//!   cargo test -p rummage-core --test search -- --ignored --nocapture
#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use rummage_core::client::{self, Client};
use rummage_core::{MAX_LINE, read_line, write_line};
use rummage_proto::{
    CPU_DEVICE, ClientMsg, CoreMsg, IndexState, ParamValue, Reply, Request, SearchHit, decode,
    encode,
};

const MODEL_DIR: &str = "embeddinggemma-2-gguf";
const MODEL_FILES: [&str; 2] = [
    "embeddinggemma-2-Q8_0.gguf",
    "mmproj-embeddinggemma-2-Q8_0.gguf",
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

async fn connect(data: &Path) -> Client {
    let pipe = rummage_core::pipe_name(data).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(token) = rummage_core::read_token(data)
            && let Ok(c) = client::connect(&pipe, &token).await
        {
            return c;
        }
        assert!(Instant::now() < deadline, "连不上核心进程");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn call(c: &mut Client, id: u64, body: Request) -> Reply {
    let msg = ClientMsg::Request { id, body };
    write_line(&mut c.writer, &encode(&msg)).await.unwrap();
    let mut buf = Vec::new();
    loop {
        let line = read_line(&mut c.reader, &mut buf, MAX_LINE)
            .await
            .unwrap()
            .expect("核心进程断开了连接");
        if let CoreMsg::Response { id: got, result } = decode::<CoreMsg>(&line).unwrap()
            && got == id
        {
            return result.unwrap();
        }
    }
}

fn query(text: Option<&str>, image_path: Option<&Path>, kinds: &[&str]) -> Request {
    Request::Search {
        text: text.map(String::from),
        image_path: image_path.map(|p| p.to_string_lossy().into_owned()),
        similar: None,
        kinds: kinds.iter().map(|k| k.to_string()).collect(),
        root_ids: Vec::new(),
        since: None,
        until: None,
        limit: None,
        snippets: false,
        supersede: false,
    }
}

fn text(q: &str) -> Request {
    query(Some(q), None, &[])
}

/// 返回结果与耗时，并打印前 5 名
async fn search(c: &mut Client, id: &mut u64, label: &str, body: Request) -> Vec<SearchHit> {
    *id += 1;
    let t = Instant::now();
    let Reply::Results {
        hits,
        took_ms,
        partial,
        semantic_unavailable,
        ..
    } = call(c, *id, body).await
    else {
        panic!("应回复搜索结果");
    };
    println!(
        "{label}：核心用时 {took_ms} 毫秒，往返 {:.0} 毫秒，partial = {partial}，semantic_unavailable = {semantic_unavailable}",
        t.elapsed().as_secs_f64() * 1000.0
    );
    assert!(!partial && !semantic_unavailable);
    for (i, h) in hits.iter().take(5).enumerate() {
        println!(
            "  {}. {:<16} 分数 {:.4}  余弦 {:<8} 片段 {}  文件名命中 {}",
            i + 1,
            h.name,
            h.score,
            h.cosine.map_or("-".into(), |c| format!("{c:.4}")),
            h.segments.len(),
            h.name_hit
        );
    }
    hits
}

#[tokio::test]
#[ignore = "需要本机的运行库与模型，CPU 推理耗时较长"]
async fn search_on_cpu() {
    let runtime = repo().join(".cache/third-party/llama.cpp/b11461/win-cpu-x64");
    let models = repo().join("src-tauri/dev-data/models").join(MODEL_DIR);
    for p in [
        runtime.clone(),
        models.join(MODEL_FILES[0]),
        models.join(MODEL_FILES[1]),
    ] {
        if !p.exists() {
            println!("跳过：找不到 {}", p.display());
            return;
        }
    }
    let base =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("search-{}", std::process::id()));
    let data = base.join("data");
    let dst = data.join("models").join(MODEL_DIR);
    fs::create_dir_all(&dst).unwrap();
    for name in MODEL_FILES {
        if let Err(err) = fs::hard_link(models.join(name), dst.join(name)) {
            println!("跳过：无法把 {name} 硬链接到临时数据目录（{err}）");
            return;
        }
    }
    let files = base.join("files");
    fs::create_dir_all(&files).unwrap();
    fs::write(
        files.join("设计说明.md"),
        "# 索引流水线\n\n切片按标题和段落进行，较长的段落再按句子切开，每个片段都记录在原文中的字符位置和行号。\n\n\
         ## 容错\n\n如果某个文件处理失败，只记录错误并继续处理下一个，不会中断整个索引任务。\n\n\
         ## 隐私\n\n所有数据都保存在本机，不会上传到任何服务器；使用电池供电时，后台索引会自动暂停。\n",
    )
    .unwrap();
    fs::write(
        files.join("会议记录.txt"),
        "本地语义搜索的第一步是扫描用户选择的文件夹，记录每个文件的路径、大小和修改时间。\n\n\
         读取正文时先判断编码，带字节顺序标记的按标记解码，其余的先尝试 UTF-8，再猜测是否为国标码。",
    )
    .unwrap();
    fs::write(
        files.join("readme.txt"),
        "This folder holds notes about the local semantic search tool.\n\nIt indexes text, code and images on this computer.",
    )
    .unwrap();
    let golden = repo().join("assets/golden/images");
    for (src, name) in [
        ("img02_photo_corgi.jpg", "柯基.jpg"),
        ("img02_photo_corgi.jpg", "柯基-副本.jpg"),
        ("img08_photo_receipt.jpg", "小票.jpg"),
    ] {
        fs::copy(golden.join(src), files.join(name)).unwrap();
    }

    let mut core = Command::new(env!("CARGO_BIN_EXE_rummage-core"))
        .arg("--data-dir")
        .arg(&data)
        .spawn()
        .unwrap();
    let mut c = connect(&data).await;
    let mut id = 0;
    // 关掉自动暂停，测试结果不受本机电源、全屏与磁盘剩余空间影响
    for (key, value) in [
        ("pause_on_battery", ParamValue::Bool(false)),
        ("pause_on_fullscreen", ParamValue::Bool(false)),
        ("min_free_disk_gb", ParamValue::Int(1)),
    ] {
        let body = Request::SetParam {
            key: key.into(),
            value,
        };
        assert_eq!(call(&mut c, id, body).await, Reply::Done);
    }
    // 全新的数据目录记为未完成引导，不建内容索引
    assert_eq!(call(&mut c, id, Request::FinishSetup).await, Reply::Done);
    // 没有自检记录：只用 CPU，免得完成引导后在后台自动检测显卡
    let body = Request::SetPreferredDevice {
        device: CPU_DEVICE.into(),
    };
    assert_eq!(call(&mut c, id, body).await, Reply::Done);
    let body = Request::AddRoot {
        path: files.to_string_lossy().into_owned(),
        excludes: Vec::new(),
    };
    assert!(matches!(call(&mut c, id, body).await, Reply::Root { .. }));
    let deadline = Instant::now() + Duration::from_secs(1200);
    loop {
        id += 1;
        let Reply::Index(s) = call(&mut c, id, Request::IndexStatus).await else {
            panic!("应回复索引进度");
        };
        if s.state == IndexState::Idle && s.total == 6 && s.done == 6 {
            break;
        }
        assert!(s.failed == 0 && Instant::now() < deadline, "{s:?}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let hits = search(
        &mut c,
        &mut id,
        "柯基在草地上接飞盘",
        text("柯基在草地上接飞盘"),
    )
    .await;
    assert!(hits[0].name.starts_with("柯基"), "{}", hits[0].name);
    // 重复查询走缓存
    search(
        &mut c,
        &mut id,
        "柯基在草地上接飞盘（缓存）",
        text("柯基在草地上接飞盘"),
    )
    .await;

    let q = "电池供电时后台建索引会不会暂停，数据会上传吗";
    let hits = search(&mut c, &mut id, q, text(q)).await;
    let pos = hits.iter().position(|h| h.name == "设计说明.md");
    assert!(pos.is_some_and(|p| p < 3), "{pos:?}");
    let doc = &hits[pos.unwrap()];
    assert!(!doc.segments.is_empty() && doc.segments[0].locator_json.contains("line"));

    let hits = search(&mut c, &mut id, "会议记", text("会议记")).await;
    let hit = hits.iter().find(|h| h.name == "会议记录.txt").unwrap();
    assert!(hit.name_hit && hit.path.ends_with("会议记录.txt"));
    let hits = search(&mut c, &mut id, "小票（不足 3 字）", text("小票")).await;
    assert!(hits.iter().any(|h| h.name == "小票.jpg" && h.name_hit));

    let body = query(None, Some(&files.join("小票.jpg")), &[]);
    let hits = search(&mut c, &mut id, "以图搜图：小票.jpg", body).await;
    assert_eq!(hits[0].name, "小票.jpg");
    assert!(hits.iter().all(|h| h.kind == "image" && !h.name_hit));

    // 过滤：只看文档
    let body = query(Some("柯基在草地上接飞盘"), None, &["document"]);
    let hits = search(&mut c, &mut id, "柯基在草地上接飞盘（只看文档）", body).await;
    assert!(hits.iter().all(|h| h.kind == "document"));

    assert!(client::shutdown(c).await.unwrap());
    assert!(core.wait().unwrap().success());
    fs::remove_dir_all(&base).unwrap();
}
