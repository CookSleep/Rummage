//! 用 CPU 跑完整的核心进程建索引：
//! 1. 自造 3 个 `.txt` / `.md`，加上从 `assets/golden/images` 复制的 2 张图片（其中一张复制两份），
//!    用 `AddRoot` 建索引，等到全部完成；核对向量数量，图片向量与黄金向量的余弦不低于 0.9999。
//! 2. 另一个数据目录中放更多文件，建索引中途结束核心进程，再启动，确认能续跑完成。
//! 3. 同上，但结束前先向核心发关机通知（`WM_ENDSESSION`）：处理中的作业应已放回待处理，重启后不计失败。
//!
//! 需要本机的 CPU 运行库与模型，缺少时跳过并提示。模型以硬链接放进 `target/tmp` 下的临时数据目录（与仓库同盘），
//! 不复制、不改动 `src-tauri/dev-data/models/`；临时数据目录中没有自检记录，只用 CPU，不加载任何 GPU 后端：
//!   cargo test -p rummage-core --test pipeline -- --ignored --nocapture
#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use rummage_core::client::{self, Client};
use rummage_core::{MAX_LINE, read_line, write_line};
use rummage_proto::{
    CPU_DEVICE, ClientMsg, CoreMsg, Event, IndexState, IndexStatus, ParamValue, Reply, Request,
    decode, encode,
};

const MODEL_DIR: &str = "embeddinggemma-2-gguf";
const MODEL_FILES: [&str; 2] = [
    "embeddinggemma-2-Q8_0.gguf",
    "mmproj-embeddinggemma-2-Q8_0.gguf",
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// 新建数据目录并硬链接模型；做不到时返回 false
fn prepare_data(data: &Path) -> bool {
    let _ = fs::remove_dir_all(data);
    let dst = data.join("models").join(MODEL_DIR);
    fs::create_dir_all(&dst).unwrap();
    let src = repo().join("src-tauri/dev-data/models").join(MODEL_DIR);
    for name in MODEL_FILES {
        if let Err(err) = fs::hard_link(src.join(name), dst.join(name)) {
            println!("跳过：无法把 {name} 硬链接到临时数据目录（{err}）");
            return false;
        }
    }
    true
}

fn spawn_core(data: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_rummage-core"))
        .arg("--data-dir")
        .arg(data)
        .spawn()
        .unwrap()
}

async fn connect(data: &Path) -> Client {
    let pipe = rummage_core::pipe_name(data).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        // 上次被结束的核心留下的旧令牌会被拒绝，等新核心写入
        if let Ok(token) = rummage_core::read_token(data)
            && let Ok(c) = client::connect(&pipe, &token).await
        {
            return c;
        }
        assert!(Instant::now() < deadline, "连不上核心进程");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// 发出请求并等待回复，期间收到的 `IndexProgress` 计入 `events`
async fn call(c: &mut Client, id: u64, body: Request, events: &mut usize) -> Reply {
    let msg = ClientMsg::Request { id, body };
    write_line(&mut c.writer, &encode(&msg)).await.unwrap();
    let mut buf = Vec::new();
    loop {
        let line = read_line(&mut c.reader, &mut buf, MAX_LINE)
            .await
            .unwrap()
            .expect("核心进程断开了连接");
        match decode::<CoreMsg>(&line).unwrap() {
            CoreMsg::Response { id: got, result } if got == id => return result.unwrap(),
            CoreMsg::Event {
                body: Event::IndexProgress(_),
            } => *events += 1,
            _ => {}
        }
    }
}

/// 关掉自动暂停，测试结果不受本机电源、全屏与磁盘剩余空间影响；参数存在数据库中，重启核心后仍然有效
async fn no_auto_pause(c: &mut Client, events: &mut usize) {
    for (key, value) in [
        ("pause_on_battery", ParamValue::Bool(false)),
        ("pause_on_fullscreen", ParamValue::Bool(false)),
        ("min_free_disk_gb", ParamValue::Int(1)),
    ] {
        let body = Request::SetParam {
            key: key.into(),
            value,
        };
        assert_eq!(call(c, 0, body, events).await, Reply::Done);
    }
}

/// 每 500 毫秒查一次进度，直到 `done` 返回真
async fn poll(
    c: &mut Client,
    id: &mut u64,
    events: &mut usize,
    done: impl Fn(&IndexStatus) -> bool,
) -> IndexStatus {
    let deadline = Instant::now() + Duration::from_secs(1200);
    loop {
        *id += 1;
        let Reply::Index(s) = call(c, *id, Request::IndexStatus, events).await else {
            panic!("应回复索引进度");
        };
        if done(&s) {
            return s;
        }
        assert!(Instant::now() < deadline, "等待索引超时：{s:?}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn finished(s: &IndexStatus, total: u64) -> bool {
    s.state == IndexState::Idle && s.total == total && s.done + s.failed == total
}

fn golden(id: &str) -> Vec<f32> {
    let path = repo().join("assets/golden/vectors-gguf-q8-2188ac1deca4b77d.json");
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    let item = v["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
        .unwrap();
    item["vector"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap() as f32)
        .collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let dot: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum();
    let n = |v: &[f32]| v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    dot / (n(a) * n(b))
}

const PARAGRAPHS: &[&str] = &[
    "本地语义搜索的第一步是扫描用户选择的文件夹，记录每个文件的路径、大小和修改时间。",
    "读取正文时先判断编码，带字节顺序标记的按标记解码，其余的先尝试 UTF-8，再猜测是否为国标码。",
    "切片按标题和段落进行，较长的段落再按句子切开，每个片段都记录在原文中的字符位置和行号。",
    "如果某个文件处理失败，只记录错误并继续处理下一个，不会中断整个索引任务。",
    "所有数据都保存在本机，不会上传到任何服务器；使用电池供电时，后台索引会自动暂停。",
];

fn write_text_files(dir: &Path) {
    fs::write(
        dir.join("设计说明.md"),
        format!(
            "# 索引流水线\n\n{}\n\n## 容错\n\n{}\n\n## 隐私\n\n{}\n",
            PARAGRAPHS[2], PARAGRAPHS[3], PARAGRAPHS[4]
        ),
    )
    .unwrap();
    fs::write(dir.join("会议记录.txt"), PARAGRAPHS[..3].join("\n\n")).unwrap();
    fs::write(
        dir.join("readme.txt"),
        "This folder holds notes about the local semantic search tool.\n\nIt indexes text, code and images on this computer.",
    )
    .unwrap();
}

fn copy_image(dir: &Path, golden_file: &str, name: &str) {
    fs::copy(
        repo().join("assets/golden/images").join(golden_file),
        dir.join(name),
    )
    .unwrap();
}

/// 在新的数据目录中建索引，完成几个文件后结束核心进程（`end_session` 时先发关机通知），再启动并等到全部完成；
/// 返回全部日志
async fn interrupt_and_resume(
    data: &Path,
    files: &Path,
    total: u64,
    end_session: bool,
    id: &mut u64,
    events: &mut usize,
) -> String {
    let mut core = spawn_core(data);
    let mut c = connect(data).await;
    no_auto_pause(&mut c, events).await;
    // 全新的数据目录记为未完成引导，不建内容索引
    assert_eq!(
        call(&mut c, 0, Request::FinishSetup, events).await,
        Reply::Done
    );
    // 没有自检记录：只用 CPU，免得完成引导后在后台自动检测显卡
    let body = Request::SetPreferredDevice {
        device: CPU_DEVICE.into(),
    };
    assert_eq!(call(&mut c, 0, body, events).await, Reply::Done);
    let body = Request::AddRoot {
        path: files.to_string_lossy().into_owned(),
        excludes: Vec::new(),
    };
    assert!(matches!(
        call(&mut c, 0, body, events).await,
        Reply::Root { .. }
    ));
    let s = poll(&mut c, id, events, |s| {
        s.total == total && s.done >= 3 && s.done < total
    })
    .await;
    if end_session {
        // 与系统关机时相同的 WM_QUERYENDSESSION、WM_ENDSESSION
        rummage_core::send_session_end(core.id()).unwrap();
        let conn = rusqlite::Connection::open(data.join("index.db")).unwrap();
        let active: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE stage IN ('preprocessing', 'embedding')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        println!("关机通知后处理中的作业：{active}");
        assert_eq!(active, 0);
    }
    core.kill().unwrap();
    core.wait().unwrap();
    println!("已在中途结束核心进程（关机通知：{end_session}）：{s:?}");
    drop(c);

    let t = Instant::now();
    let mut core = spawn_core(data);
    let mut c = connect(data).await;
    let s = poll(&mut c, id, events, |s| finished(s, total)).await;
    println!(
        "重启后续跑用时 {:.1} 秒，进度：{s:?}",
        t.elapsed().as_secs_f64()
    );
    assert_eq!((s.done, s.failed), (total, 0), "{s:?}");
    assert!(client::shutdown(c).await.unwrap());
    assert!(core.wait().unwrap().success());
    // 启动时日志可能已轮转，查看全部日志文件
    fs::read_dir(data.join("logs"))
        .unwrap()
        .flatten()
        .map(|e| fs::read_to_string(e.path()).unwrap_or_default())
        .collect()
}

#[tokio::test]
#[ignore = "需要本机的运行库与模型，CPU 推理耗时较长"]
async fn index_and_resume_on_cpu() {
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
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("pipeline-{}", std::process::id()));

    // 1. 建索引，核对数量与图片余弦
    let data = base.join("data");
    if !prepare_data(&data) {
        return;
    }
    let files = base.join("files");
    fs::create_dir_all(&files).unwrap();
    write_text_files(&files);
    copy_image(&files, "img02_photo_corgi.jpg", "柯基.jpg");
    copy_image(&files, "img02_photo_corgi.jpg", "柯基-副本.jpg");
    copy_image(&files, "img08_photo_receipt.jpg", "小票.jpg");

    let mut core = spawn_core(&data);
    let mut c = connect(&data).await;
    let (mut id, mut events) = (0, 0);
    no_auto_pause(&mut c, &mut events).await;
    // 全新的数据目录记为未完成引导，不建内容索引
    assert_eq!(
        call(&mut c, 0, Request::FinishSetup, &mut events).await,
        Reply::Done
    );
    // 没有自检记录：只用 CPU，免得完成引导后在后台自动检测显卡
    let body = Request::SetPreferredDevice {
        device: CPU_DEVICE.into(),
    };
    assert_eq!(call(&mut c, 0, body, &mut events).await, Reply::Done);
    let t = Instant::now();
    let body = Request::AddRoot {
        path: files.to_string_lossy().into_owned(),
        excludes: Vec::new(),
    };
    assert!(matches!(
        call(&mut c, 0, body, &mut events).await,
        Reply::Root { .. }
    ));
    let s = poll(&mut c, &mut id, &mut events, |s| finished(s, 6)).await;
    let elapsed = t.elapsed();
    println!("建索引用时 {:.1} 秒，进度：{s:?}", elapsed.as_secs_f64());
    assert_eq!((s.done, s.failed), (6, 0), "{s:?}");
    assert_eq!(s.backend.as_deref(), Some(rummage_embed::cpu_name()));
    // 进度事件最多每 500 毫秒一次，查询期间应收到过
    assert!(events > 0);
    assert!(client::shutdown(c).await.unwrap());
    assert!(core.wait().unwrap().success());

    let conn = rusqlite::Connection::open(data.join("index.db")).unwrap();
    let rows: Vec<(String, String, i64, i64)> = conn
        .prepare(
            "SELECT f.name, f.kind, COUNT(s.id), COUNT(v.segment_id) FROM files f
             LEFT JOIN segments s ON s.file_id = f.id LEFT JOIN vectors v ON v.segment_id = s.id
             GROUP BY f.id ORDER BY f.kind, f.name",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    for r in &rows {
        println!("{:<16} {:<9} 片段 {:>2}  向量 {:>2}", r.0, r.1, r.2, r.3);
        assert!(r.2 >= 1 && r.2 == r.3, "{r:?}");
    }
    assert_eq!(rows.len(), 6);
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM vectors WHERE dim = 768", [], |r| {
            r.get(0)
        })
        .unwrap();
    println!("向量共 {total} 条");
    let images: Vec<(String, Vec<u8>)> = conn
        .prepare(
            "SELECT f.name, v.vec FROM files f JOIN segments s ON s.file_id = f.id
             JOIN vectors v ON v.segment_id = s.id WHERE f.kind = 'image' ORDER BY f.name",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(images.len(), 3);
    for (name, blob) in &images {
        let want = if name.starts_with("柯基") {
            golden("img02_photo_corgi")
        } else {
            golden("img08_photo_receipt")
        };
        let cos = cosine(&rummage_index::blob_to_vec(blob).unwrap(), &want);
        println!("{name} 与黄金向量的余弦 {cos:.7}");
        assert!(cos >= 0.9999, "{name}：{cos}");
    }
    drop(conn);

    // 2. 中途结束核心进程后续跑
    let data = base.join("data2");
    if !prepare_data(&data) {
        return;
    }
    let files = base.join("files2");
    fs::create_dir_all(&files).unwrap();
    write_text_files(&files);
    for i in 0..40 {
        let text: Vec<&str> = (0..8)
            .map(|k| PARAGRAPHS[(i + k) % PARAGRAPHS.len()])
            .collect();
        fs::write(files.join(format!("笔记{i:02}.txt")), text.join("\n\n")).unwrap();
    }
    copy_image(&files, "img02_photo_corgi.jpg", "柯基.jpg");
    copy_image(&files, "img08_photo_receipt.jpg", "小票.jpg");
    let total = 45;

    let log = interrupt_and_resume(&data, &files, total, false, &mut id, &mut events).await;
    let recovered = log.lines().find(|l| l.contains("上次退出时正在处理"));
    println!("核心日志：{}", recovered.unwrap_or("（无）"));
    assert!(recovered.is_some(), "重启时应恢复中断的作业");

    // 3. 中途收到关机通知（WM_ENDSESSION）后进程被系统结束：不算崩溃，不计失败
    let data = base.join("data3");
    if !prepare_data(&data) {
        return;
    }
    let log = interrupt_and_resume(&data, &files, total, true, &mut id, &mut events).await;
    assert!(!log.contains("上次退出时正在处理"), "关机不应计失败：{log}");

    fs::remove_dir_all(&base).unwrap();
}
