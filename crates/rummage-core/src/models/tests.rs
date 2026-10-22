//! 用本机的小型 HTTP 服务测试下载（断点续传、校验失败、文件列表接口），不访问外网
use std::io::{BufRead, BufReader};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use super::*;

/// 测试服务的行为
#[derive(Clone, Default)]
struct Opts {
    /// 文件列表接口的回复；为空时回 404
    api: Option<String>,
    /// 第一次下载请求只发出这么多字节就断开
    cut_first: Option<usize>,
    /// 不支持 Range，总是回 200 与完整内容
    no_range: bool,
    /// 收到 Range 时回 206，但 Content-Range 从 0 开始、内容是整个文件
    bad_range: bool,
}

/// 收到的请求：(路径, Range 头)
type Requests = Arc<Mutex<Vec<(String, Option<String>)>>>;

struct Server {
    base: String,
    requests: Requests,
}

fn serve(data: Vec<u8>, opts: Opts) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    thread::spawn(move || {
        let mut downloads = 0;
        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                return;
            };
            let (path, range) = read_request(&stream);
            log.lock().unwrap().push((path.clone(), range.clone()));
            let mut s = stream;
            if path.contains("/api/models/") {
                match &opts.api {
                    Some(body) => respond(&mut s, "200 OK", &[], body.as_bytes(), None),
                    None => respond(&mut s, "404 Not Found", &[], b"", None),
                }
                continue;
            }
            downloads += 1;
            let cut = if downloads == 1 { opts.cut_first } else { None };
            let start = range
                .as_deref()
                .and_then(|r| r.strip_prefix("bytes="))
                .and_then(|r| r.trim_end_matches('-').parse::<usize>().ok())
                .filter(|_| !opts.no_range);
            if opts.bad_range && range.is_some() {
                let header = format!("Content-Range: bytes 0-{}/{}", data.len() - 1, data.len());
                respond(&mut s, "206 Partial Content", &[&header], &data, cut);
                continue;
            }
            match start {
                Some(start) => {
                    let header = format!(
                        "Content-Range: bytes {start}-{}/{}",
                        data.len() - 1,
                        data.len()
                    );
                    respond(
                        &mut s,
                        "206 Partial Content",
                        &[&header],
                        &data[start..],
                        cut,
                    );
                }
                None => respond(&mut s, "200 OK", &[], &data, cut),
            }
        }
    });
    Server { base, requests }
}

fn read_request(stream: &TcpStream) -> (String, Option<String>) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let path = line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    let mut range = None;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':')
            && k.eq_ignore_ascii_case("range")
        {
            range = Some(v.trim().to_string());
        }
    }
    (path, range)
}

/// 写出回复；`cut` 不为空时声明完整长度但只写出这么多字节就断开
fn respond(s: &mut TcpStream, status: &str, headers: &[&str], body: &[u8], cut: Option<usize>) {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for h in headers {
        head.push_str(h);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    let _ = s.write_all(head.as_bytes());
    let _ = s.write_all(&body[..cut.unwrap_or(body.len()).min(body.len())]);
    let _ = s.flush();
}

fn content(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251) as u8).collect()
}

/// 内容对应的模型文件；测试中泄漏几个小对象换取 `'static`
fn model(name: &'static str, data: &[u8]) -> &'static ModelFile {
    let sha: &'static str = Box::leak(hex(&Sha256::digest(data)).into_boxed_str());
    Box::leak(Box::new(ModelFile {
        name,
        sha256: sha,
        size: data.len() as u64,
    }))
}

fn run(base: &str, files: &[&'static ModelFile], dir: &Path) -> Result<Vec<Progress>, Error> {
    install_crypto();
    let mut seen = Vec::new();
    let files: Vec<Pinned> = files.iter().map(|f| Pinned::gguf(f)).collect();
    download(
        base,
        &files,
        dir,
        &[],
        dir,
        &Route::Direct,
        &AtomicBool::new(false),
        &mut |p| seen.push(p.clone()),
    )
    .map(|()| seen)
}

#[test]
fn resumes_after_interruption() {
    let data = content(3 * 1024 * 1024 + 7);
    let file = model("m.gguf", &data);
    let server = serve(
        data.clone(),
        Opts {
            cut_first: Some(1024 * 1024),
            ..Opts::default()
        },
    );
    let dir = crate::test_dir();
    let err = run(&server.base, &[file], dir.path()).unwrap_err();
    assert!(matches!(err, Error::Failed(_)), "{err}");
    let part = dir.path().join("m.gguf.part");
    let have = fs::metadata(&part).unwrap().len();
    assert!(have > 0 && have < data.len() as u64, "{have}");
    assert!(!dir.path().join("m.gguf").exists());

    let seen = run(&server.base, &[file], dir.path()).unwrap();
    assert_eq!(fs::read(dir.path().join("m.gguf")).unwrap(), data);
    assert!(!part.exists());
    // 续传前先核对已下载的部分，再从中断处请求
    assert!(
        seen.iter()
            .any(|p| p.phase == ModelTaskPhase::Checking && p.file == "m.gguf")
    );
    assert!(
        seen.iter()
            .any(|p| p.phase == ModelTaskPhase::Downloading && p.done == have)
    );
    let reqs = server.requests.lock().unwrap();
    let last = reqs.last().unwrap();
    assert!(
        last.0
            .ends_with(&format!("/resolve/{}/m.gguf", space::COMMIT)),
        "{}",
        last.0
    );
    assert_eq!(last.1.as_deref(), Some(format!("bytes={have}-").as_str()));
    drop(reqs);

    // 文件已正确：不再联网
    let before = server.requests.lock().unwrap().len();
    run(&server.base, &[file], dir.path()).unwrap();
    assert_eq!(server.requests.lock().unwrap().len(), before);
}

#[test]
fn restarts_without_range_support() {
    let data = content(200_000);
    let file = model("m.gguf", &data);
    let server = serve(
        data.clone(),
        Opts {
            no_range: true,
            ..Opts::default()
        },
    );
    let dir = crate::test_dir();
    // 已有一段内容不对的部分：服务器回 200，从头下载
    fs::write(dir.path().join("m.gguf.part"), vec![9u8; 5000]).unwrap();
    run(&server.base, &[file], dir.path()).unwrap();
    assert_eq!(fs::read(dir.path().join("m.gguf")).unwrap(), data);
}

#[test]
fn wrong_content_range_restarts() {
    let data = content(200_000);
    let file = model("m.gguf", &data);
    let server = serve(
        data.clone(),
        Opts {
            bad_range: true,
            ..Opts::default()
        },
    );
    let dir = crate::test_dir();
    fs::write(dir.path().join("m.gguf.part"), &data[..5000]).unwrap();
    run(&server.base, &[file], dir.path()).unwrap();
    // 起点不符的 206 不当作续传写入：删掉 .part，不带 Range 从头请求
    assert_eq!(fs::read(dir.path().join("m.gguf")).unwrap(), data);
    let reqs = server.requests.lock().unwrap();
    let n = reqs.len();
    assert_eq!(reqs[n - 2].1.as_deref(), Some("bytes=5000-"));
    assert_eq!(reqs[n - 1].1, None);
}

#[test]
fn hash_mismatch_deletes_part() {
    let data = content(100_000);
    let mut wrong = data.clone();
    wrong[500] ^= 0xff;
    let file = model("m.gguf", &data);
    let server = serve(wrong, Opts::default());
    let dir = crate::test_dir();
    let err = run(&server.base, &[file], dir.path()).unwrap_err();
    assert!(err.to_string().contains("不完整或已损坏"), "{err}");
    assert!(!dir.path().join("m.gguf.part").exists());
    assert!(!dir.path().join("m.gguf").exists());
}

#[test]
fn checks_remote_listing() {
    let data = content(1000);
    let file = model("m.gguf", &data);
    let dir = crate::test_dir();
    let listing = |oid: &str| {
        Some(format!(
            r#"[{{"type":"file","path":"m.gguf","size":1000,"lfs":{{"oid":"{oid}","size":1000,"pointerSize":134}}}},
                {{"type":"file","path":"README.md","size":12}}]"#
        ))
    };
    // 与已知表一致：照常下载
    let server = serve(
        data.clone(),
        Opts {
            api: listing(file.sha256),
            ..Opts::default()
        },
    );
    run(&server.base, &[file], dir.path()).unwrap();
    assert!(server.requests.lock().unwrap()[0].0.contains(&format!(
        "/api/models/{}/tree/{}",
        space::REPO,
        space::COMMIT
    )));

    // 不一致：拒绝，不请求文件
    let other = crate::test_dir();
    let server = serve(
        data,
        Opts {
            api: listing(&"0".repeat(64)),
            ..Opts::default()
        },
    );
    let err = run(&server.base, &[file], other.path()).unwrap_err();
    assert!(err.to_string().contains("不一致"), "{err}");
    assert_eq!(server.requests.lock().unwrap().len(), 1);

    // 接口内容无法解析：按已知表下载
    let third = crate::test_dir();
    let server = serve(
        content(1000),
        Opts {
            api: Some("<html>".into()),
            ..Opts::default()
        },
    );
    run(&server.base, &[file], third.path()).unwrap();
}

#[test]
fn http_errors_are_sentences() {
    let file = model("m.gguf", &content(10));
    let dir = crate::test_dir();
    // 没有服务在监听的端口
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let err = run(&format!("http://127.0.0.1:{port}"), &[file], dir.path()).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.starts_with("连接被拒绝") && msg.ends_with('。'),
        "{msg}"
    );
    // 经过自定义代理时，连不上归因到代理服务器
    let proxy = Route::Via(Url::parse(&format!("http://127.0.0.1:{port}")).unwrap());
    let err = download(
        "http://example.invalid",
        &[Pinned::gguf(file)],
        dir.path(),
        &[],
        dir.path(),
        &proxy,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(err.to_string().starts_with("无法连接代理服务器"), "{err}");
    assert_eq!(
        status_error(StatusCode::NOT_FOUND).to_string(),
        "下载源上找不到模型文件，请换一个下载源。"
    );
}

#[test]
fn cancel_keeps_part() {
    install_crypto();
    let file = model("m.gguf", &content(10));
    let dir = crate::test_dir();
    let server = serve(content(10), Opts::default());
    let err = download(
        &server.base,
        &[Pinned::gguf(file)],
        dir.path(),
        &[],
        dir.path(),
        &Route::Direct,
        &AtomicBool::new(true),
        &mut |_| {},
    )
    .unwrap_err();
    assert_eq!(err, Error::Cancelled);
}

#[test]
fn imports_known_files() {
    let a = content(4000);
    let b = content(6000);
    let (fa, fb) = (model("a.gguf", &a), model("b.gguf", &b));
    let src = crate::test_dir();
    let dir = crate::test_dir();
    let pa = src.path().join("下载的主模型.gguf");
    let pb = src.path().join("b.gguf");
    fs::write(&pa, &a).unwrap();
    fs::write(&pb, &b).unwrap();
    let cancel = AtomicBool::new(false);
    let got = import(
        &[pa.clone(), pb.clone()],
        &[fa, fb],
        dir.path(),
        &cancel,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(
        got.iter().map(|f| f.name).collect::<Vec<_>>(),
        ["a.gguf", "b.gguf"]
    );
    assert_eq!(fs::read(dir.path().join("a.gguf")).unwrap(), a);
    assert_eq!(fs::read(dir.path().join("b.gguf")).unwrap(), b);
    assert!(!dir.path().join("a.gguf.import.part").exists());

    // 大小不认识：一个也不导入
    let odd = src.path().join("odd.gguf");
    fs::write(&odd, b"x").unwrap();
    let other = crate::test_dir();
    // 导入不碰下载用的 .part
    fs::write(other.path().join("a.gguf.part"), b"partial").unwrap();
    let err = import(
        &[pa.clone(), odd],
        &[fa, fb],
        other.path(),
        &cancel,
        &mut |_| {},
    )
    .unwrap_err();
    assert!(
        err.to_string()
            .starts_with("\"odd.gguf\" 不是 Rummage 使用的模型文件"),
        "{err}"
    );
    assert!(!other.path().join("a.gguf").exists());

    // 大小相同、内容不对：删除复制了一半的文件
    let mut bad = a.clone();
    bad[0] ^= 1;
    fs::write(&pa, &bad).unwrap();
    let err = import(&[pa], &[fa, fb], other.path(), &cancel, &mut |_| {}).unwrap_err();
    assert!(err.to_string().contains("不是所需的版本"), "{err}");
    assert!(!other.path().join("a.gguf").exists());
    assert!(!other.path().join("a.gguf.import.part").exists());
    assert_eq!(
        fs::read(other.path().join("a.gguf.part")).unwrap(),
        b"partial"
    );
}

#[test]
fn custom_source_validated() {
    assert_eq!(base_url(&ModelSource::Official).unwrap(), OFFICIAL);
    assert_eq!(base_url(&ModelSource::Mirror).unwrap(), MIRROR);
    let custom = |url: &str| base_url(&ModelSource::Custom { url: url.into() });
    assert_eq!(
        custom(" https://hf.example.com/hf/ ").unwrap(),
        "https://hf.example.com/hf"
    );
    assert!(custom("").unwrap_err().starts_with("请填写下载源地址"));
    assert!(custom("ftp://x").unwrap_err().contains("http://"));
    assert!(custom("hf-mirror.com").is_err());
    assert!(
        custom("https://x.com/?a=1")
            .unwrap_err()
            .contains("查询参数")
    );
}

#[test]
fn verifies_installed_file() {
    let a = content(5000);
    let fa = model("a.gguf", &a);
    let dir = crate::test_dir();
    let path = dir.path().join("a.gguf");
    let cancel = AtomicBool::new(false);
    let mut seen = Vec::new();
    // 不存在
    assert!(!verify(&path, fa, &cancel, &mut |_| {}).unwrap());
    fs::write(&path, &a).unwrap();
    assert!(verify(&path, fa, &cancel, &mut |p| seen.push(p.clone())).unwrap());
    assert_eq!(seen[0].phase, ModelTaskPhase::Checking);
    assert_eq!((seen[0].file, seen[0].count), ("a.gguf", 1));
    // 大小相同、内容不对
    let mut bad = a.clone();
    bad[10] ^= 1;
    fs::write(&path, &bad).unwrap();
    assert!(!verify(&path, fa, &cancel, &mut |_| {}).unwrap());
    // 取消
    cancel.store(true, Ordering::Relaxed);
    assert_eq!(
        verify(&path, fa, &cancel, &mut |_| {}).unwrap_err(),
        Error::Cancelled
    );
}

#[test]
fn removes_file_and_parts() {
    let a = content(100);
    let fa = model("a.gguf", &a);
    let dir = crate::test_dir();
    for name in ["a.gguf", "a.gguf.part", "a.gguf.import.part", "b.gguf"] {
        fs::write(dir.path().join(name), b"x").unwrap();
    }
    assert!(remove(dir.path(), fa, "图片与音视频模型").unwrap());
    let left: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(left, ["b.gguf"]);
    // 只有下载了一部分的文件
    fs::write(dir.path().join("a.gguf.part"), b"x").unwrap();
    assert!(!remove(dir.path(), fa, "图片与音视频模型").unwrap());
    assert!(!dir.path().join("a.gguf.part").exists());
    // 已经不存在时也算成功
    assert!(!remove(dir.path(), fa, "图片与音视频模型").unwrap());
}

#[test]
fn removes_only_caches_of_that_model() {
    let dir = crate::test_dir();
    for name in [
        "m.litertlm_1_2.vision_encoder.xnnpack_cache",
        "m.litertlm.text_encoder_1_2_mldrift_program_cache.bin",
        "m_SM8550.litertlm_1_2.vision_adapter.xnnpack_cache",
        "m.litertlm2",
    ] {
        fs::write(dir.path().join(name), b"x").unwrap();
    }
    remove_caches(dir.path(), "m.litertlm");
    let mut left: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            "m.litertlm2",
            "m_SM8550.litertlm_1_2.vision_adapter.xnnpack_cache"
        ]
    );
    // 目录不存在时什么也不做
    remove_caches(&dir.path().join("none"), "m.litertlm");
}

/// 文件被其他程序以不允许删除的方式打开时提示稍后重试，不删除其他文件
#[cfg(windows)]
#[test]
fn remove_reports_file_in_use() {
    use std::os::windows::fs::OpenOptionsExt;
    let a = content(100);
    let fa = model("a.gguf", &a);
    let dir = crate::test_dir();
    let path = dir.path().join("a.gguf");
    fs::write(&path, &a).unwrap();
    fs::write(dir.path().join("a.gguf.part"), b"x").unwrap();
    let held = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    let err = remove(dir.path(), fa, "图片与音视频模型").unwrap_err();
    assert_eq!(
        err.to_string(),
        "图片与音视频模型正被使用，暂时无法删除，请稍后再试。"
    );
    assert!(dir.path().join("a.gguf.part").exists());
    drop(held);
    remove(dir.path(), fa, "图片与音视频模型").unwrap();
    assert!(!path.exists());
}
