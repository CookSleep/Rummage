//! MCP 服务（`rummage-core mcp`）的集成测试：手写 JSON-RPC 走 stdio。
//! - 旧版握手（`initialize`，2025-11-25）与新版（`server/discover`、逐请求 `_meta`，2026-07-28）都能用；
//! - `tools/list` 的工具与注解（只读、会移除索引的、幂等的）、title；参数不对时返回 `isError` 并附正确写法；
//! - 临时数据目录中的真实 `search_files`（不放模型，只靠文件名命中；含按文件夹与子文件夹筛选）与 `index_status`，以及查看与操作应用的
//!   工具（文件夹的增改删、参数的查看与修改、暂停与恢复、失败文件）；
//! - stdin 关闭后进程退出。
//!
//! 不需要模型与推理；第三项会在临时数据目录中启动核心进程，测试结束时请它退出。
#![cfg(windows)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use rummage_core::client::{self, Client};
use rummage_proto::{ParamValue, Reply, Request};
use serde_json::{Value, json};

struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    out: BufReader<ChildStdout>,
}

impl Mcp {
    fn spawn(data: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rummage-core"))
            .arg("mcp")
            .arg("--data-dir")
            .arg(data)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let out = BufReader::new(child.stdout.take().unwrap());
        Self { child, stdin, out }
    }

    fn send(&mut self, msg: Value) {
        let stdin = self.stdin.as_mut().expect("stdin 已关闭");
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    }

    /// 发出请求并读到同一 id 的回复，中间的通知跳过
    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(
                self.out.read_line(&mut line).unwrap() > 0,
                "MCP 进程关闭了 stdout"
            );
            let msg: Value = serde_json::from_str(&line).unwrap();
            if msg["id"] == id {
                return msg;
            }
        }
    }

    /// 关闭 stdin 后，进程应在 5 秒内正常退出
    fn close(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "{status}");
                return;
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                panic!("stdin 关闭后 MCP 进程没有退出");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// 全部工具，按名称排序
const TOOLS: [&str; 14] = [
    "add_folder",
    "get_hit_context",
    "get_settings",
    "index_status",
    "list_failed_files",
    "list_folders",
    "pause_indexing",
    "remove_folder",
    "resume_indexing",
    "retry_failed_files",
    "save_search_results",
    "search_files",
    "set_setting",
    "update_folder",
];

/// 只读的工具
const READ_ONLY: [&str; 6] = [
    "get_hit_context",
    "get_settings",
    "index_status",
    "list_failed_files",
    "list_folders",
    "search_files",
];

/// 新版协议每个请求都带的 `_meta`
fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "1"},
    })
}

fn initialize(m: &mut Mcp) -> Value {
    let r = m.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "1"},
        }),
    );
    m.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    r
}

/// 工具调用结果的文本与是否出错
fn text_of(r: &Value) -> (String, bool) {
    let res = &r["result"];
    let text = res["content"][0]["text"].as_str().unwrap_or_default();
    (text.to_string(), res["isError"] == true)
}

#[test]
fn legacy_handshake_and_tools() {
    let dir = tempfile::tempdir().unwrap();
    let mut m = Mcp::spawn(dir.path());
    let r = initialize(&mut m);
    assert_eq!(r["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(r["result"]["serverInfo"]["name"], "rummage");
    assert!(
        r["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("search_files")
    );

    let r = m.request(2, "tools/list", json!({}));
    let tools = r["result"]["tools"].as_array().unwrap();
    let mut names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort();
    assert_eq!(names, TOOLS);
    for t in tools {
        let name = t["name"].as_str().unwrap();
        let a = &t["annotations"];
        assert_eq!(a["readOnlyHint"], READ_ONLY.contains(&name), "{t}");
        if !READ_ONLY.contains(&name) {
            assert_eq!(a["destructiveHint"], name == "remove_folder", "{t}");
            assert!(a["idempotentHint"].is_boolean(), "{t}");
        }
        assert_eq!(a["openWorldHint"], false, "{t}");
        assert!(t["title"].as_str().is_some_and(|s| !s.is_empty()), "{t}");
        assert!(t["description"].as_str().unwrap().len() > 100, "{t}");
        assert!(!t["inputSchema"].to_string().contains("null"), "{t}");
    }
    let search = tools.iter().find(|t| t["name"] == "search_files").unwrap();
    let props = &search["inputSchema"]["properties"];
    for p in [
        "query",
        "image_path",
        "kinds",
        "folders",
        "limit",
        "offset",
        "include_weak",
    ] {
        assert!(props.get(p).is_some(), "缺少参数 {p}");
    }
    assert_eq!(props["limit"]["maximum"], 50);

    // 参数不对：isError，附正确写法；不需要连接核心
    let r = m.request(
        3,
        "tools/call",
        json!({"name": "search_files", "arguments": {}}),
    );
    let (text, err) = text_of(&r);
    assert!(err && text.contains(r#"{"query": "#), "{text}");
    let r = m.request(
        4,
        "tools/call",
        json!({"name": "search_files", "arguments": {"query": "x", "limit": 51}}),
    );
    let (text, err) = text_of(&r);
    assert!(err && text.contains("from 1 to 50"), "{text}");
    // 数据目录不对：MCP 自己的错误用英文、加前缀
    let r = m.request(
        5,
        "tools/call",
        json!({"name": "index_status", "arguments": {}}),
    );
    let (text, err) = text_of(&r);
    assert!(
        err && text.starts_with("Rummage: ") && text.contains("is not a Rummage data directory"),
        "{text}"
    );
    m.close();
}

#[test]
fn modern_discover() {
    let dir = tempfile::tempdir().unwrap();
    let mut m = Mcp::spawn(dir.path());
    let r = m.request(1, "server/discover", json!({"_meta": meta()}));
    let versions = r["result"]["supportedVersions"].as_array().unwrap();
    assert!(versions.contains(&json!("2026-07-28")), "{r}");
    assert!(versions.contains(&json!("2025-11-25")), "{r}");
    let r = m.request(2, "tools/list", json!({"_meta": meta()}));
    assert_eq!(
        r["result"]["tools"].as_array().unwrap().len(),
        TOOLS.len(),
        "{r}"
    );
    let r = m.request(
        3,
        "tools/call",
        json!({"name": "get_hit_context", "arguments": {"hit_id": 0}, "_meta": meta()}),
    );
    let (text, err) = text_of(&r);
    assert!(err && text.contains(r#"{"hit_id": "#), "{text}");
    m.close();
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

/// 测试结束（含失败）时结束核心进程
struct Core(Child);

impl Drop for Core {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn real_search() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("数据");
    let files = root.path().join("文件");
    fs::create_dir_all(&data).unwrap();
    fs::create_dir_all(&files).unwrap();
    fs::write(
        files.join("发票归档说明.txt"),
        "每月把发票扫描后放进对应月份的文件夹。",
    )
    .unwrap();
    fs::write(files.join("购物清单.txt"), "牛奶、鸡蛋、面包").unwrap();

    let core = Core(
        Command::new(env!("CARGO_BIN_EXE_rummage-core"))
            .arg("--data-dir")
            .arg(&data)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut c = connect(&data).await;
    let mut id = 0;
    let mut call = async |c: &mut Client, body: Request| {
        id += 1;
        client::request(c, id, body).await.unwrap().unwrap()
    };
    for (key, value) in [
        ("pause_on_battery", ParamValue::Bool(false)),
        ("pause_on_fullscreen", ParamValue::Bool(false)),
        ("min_free_disk_gb", ParamValue::Int(1)),
    ] {
        let body = Request::SetParam {
            key: key.into(),
            value,
        };
        assert_eq!(call(&mut c, body).await, Reply::Done);
    }
    let body = Request::AddRoot {
        path: files.to_string_lossy().into_owned(),
        excludes: Vec::new(),
    };
    assert!(matches!(call(&mut c, body).await, Reply::Root { .. }));
    let deadline = Instant::now() + Duration::from_secs(60);
    let folder = loop {
        let Reply::Roots { roots } = call(&mut c, Request::ListRoots).await else {
            panic!("应回复文件夹列表");
        };
        if roots[0].files == 2 {
            break roots[0].path.clone();
        }
        assert!(Instant::now() < deadline, "扫描没有完成");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    // MCP 连接已在运行的核心
    let mut m = Mcp::spawn(&data);
    initialize(&mut m);
    let r = m.request(
        2,
        "tools/call",
        json!({"name": "search_files", "arguments": {"query": "发票归档说明"}}),
    );
    let (text, err) = text_of(&r);
    assert!(!err && text.contains("发票归档说明.txt"), "{text}");
    assert!(text.contains("matched: file name"), "{text}");
    assert!(!text.contains("购物清单"), "{text}");
    let r = m.request(
        3,
        "tools/call",
        json!({"name": "index_status", "arguments": {}}),
    );
    let (text, err) = text_of(&r);
    assert!(
        !err && text.contains(&format!("  {folder} (2 files)")),
        "{text}"
    );
    // 已索引的文件夹或其中的子文件夹（大小写可以不同）
    let r = m.request(
        4,
        "tools/call",
        json!({"name": "search_files", "arguments": {"query": "发票归档说明", "folders": [folder.to_uppercase()]}}),
    );
    let (text, err) = text_of(&r);
    assert!(!err && text.contains("发票归档说明.txt"), "{text}");
    let sub = format!(r"{folder}\子文件夹");
    let r = m.request(
        4,
        "tools/call",
        json!({"name": "search_files", "arguments": {"query": "发票归档说明", "folders": [sub]}}),
    );
    let (text, err) = text_of(&r);
    assert!(!err && !text.contains("发票归档说明.txt"), "{text}");
    // 不在已索引的文件夹中：列出可用的
    let r = m.request(
        4,
        "tools/call",
        json!({"name": "search_files", "arguments": {"query": "发票", "folders": [r"C:\别处"]}}),
    );
    let (text, err) = text_of(&r);
    assert!(err && text.contains(&folder), "{text}");

    // 查看与操作应用
    let mut tool = |id, name: &str, args: Value| {
        let r = m.request(id, "tools/call", json!({"name": name, "arguments": args}));
        text_of(&r)
    };
    let (text, err) = tool(10, "list_folders", json!({}));
    assert!(
        !err && text.contains(&format!("  {folder} (2 files)"))
            && text.starts_with("Content recognition: document, image, audio, video"),
        "{text}"
    );
    let other = root.path().join("其他");
    fs::create_dir_all(&other).unwrap();
    let other = other.to_string_lossy().into_owned();
    let (text, err) = tool(
        11,
        "add_folder",
        json!({"path": other, "excludes": ["*.tmp", "[坏"]}),
    );
    assert!(
        err && text.starts_with("Rummage: 排除规则 \"[坏\""),
        "{text}"
    );
    let (text, err) = tool(
        12,
        "add_folder",
        json!({"path": other, "excludes": ["*.tmp"], "kinds": ["document", "image"]}),
    );
    assert!(!err && text.contains("excludes: *.tmp"), "{text}");
    assert!(
        text.contains("Content recognition: document, image (shared"),
        "{text}"
    );
    let (text, err) = tool(13, "add_folder", json!({"path": "相对路径"}));
    assert!(err && text.contains("absolute"), "{text}");
    let (text, err) = tool(14, "update_folder", json!({"path": other, "excludes": []}));
    assert!(!err && text.contains("excludes: none"), "{text}");
    let (text, err) = tool(
        15,
        "update_folder",
        json!({"path": r"C:\别处", "excludes": []}),
    );
    assert!(err && text.contains(&folder), "{text}");
    let (text, err) = tool(16, "remove_folder", json!({"path": other}));
    assert!(!err && text.contains("No files were deleted"), "{text}");
    let (text, _) = tool(17, "list_folders", json!({}));
    assert!(
        text.contains("Content recognition: document, image (shared") && !text.contains("其他"),
        "{text}"
    );

    let (text, err) = tool(20, "get_settings", json!({"key": "result_limit"}));
    assert!(
        !err && text.starts_with("result_limit = 100 | default 100"),
        "{text}"
    );
    let (text, err) = tool(
        21,
        "set_setting",
        json!({"key": "result_limit", "value": "200"}),
    );
    assert_eq!((text.as_str(), err), ("Set result_limit to 200.", false));
    let (text, err) = tool(
        22,
        "set_setting",
        json!({"key": "result_limit", "value": 5}),
    );
    assert!(err && text.contains("integer 10 to 500"), "{text}");
    let (text, err) = tool(23, "set_setting", json!({"key": "no_such_key", "value": 1}));
    assert!(err && text.contains("get_settings"), "{text}");
    let (text, err) = tool(
        24,
        "set_setting",
        json!({"key": "chunk_tokens", "value": 256}),
    );
    assert!(
        !err && text.contains("rebuild the document index"),
        "{text}"
    );

    let (text, err) = tool(30, "pause_indexing", json!({"minutes": 30}));
    assert!(
        !err && text.starts_with("Indexing paused until 20"),
        "{text}"
    );
    let (text, _) = tool(31, "index_status", json!({}));
    assert!(text.contains("paused by the user"), "{text}");
    let (text, err) = tool(32, "resume_indexing", json!({}));
    assert!(!err && text.starts_with("Indexing resumed."), "{text}");
    let (text, err) = tool(33, "list_failed_files", json!({}));
    assert_eq!((text.as_str(), err), ("No files failed to index.", false));
    let (text, err) = tool(34, "retry_failed_files", json!({}));
    assert_eq!(
        (text.as_str(), err),
        ("No failed files can be retried.", false)
    );
    m.close();

    let pid = core.0.id();
    assert!(
        client::shutdown(c).await.unwrap(),
        "核心进程 {pid} 没有退出"
    );
}
