//! 流水线的端到端测试：用推理替身代替推理子进程，在临时目录中建索引并核对数据库。
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Instant, SystemTime};

use rummage_embed::{Backend, Embedding, Priority, vector};
use rummage_index::{normalize_root, scan_root};
use rummage_proto::params;

use super::*;

mod docmedia;
mod failures;
mod gpu;
mod hold;
mod media;
mod pause;
mod rebuild;
mod wake;

/// `stuck` 开头的文件同时在处理的个数与最大值
static STUCK_ACTIVE: AtomicUsize = AtomicUsize::new(0);
static STUCK_MAX: AtomicUsize = AtomicUsize::new(0);

/// 预处理中的测试钩子（算出 quick_hash 之后）：文件名以 slow 开头时睡 2 秒，以 stuck 开头时睡 0.5 秒并记录
/// 同时在处理的个数，以 panic 开头时 panic
pub(super) fn hook(name: &str) {
    if name.starts_with("slow") {
        thread::sleep(Duration::from_secs(2));
    }
    if name.starts_with("stuck") {
        let n = STUCK_ACTIVE.fetch_add(1, Ordering::SeqCst) + 1;
        STUCK_MAX.fetch_max(n, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(500));
        STUCK_ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
    if name.starts_with("panic") {
        panic!("模拟预处理时 panic");
    }
}

const CORGI: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/golden/images/img02_photo_corgi.jpg"
);

/// 推理替身：返回由输入长度决定的固定向量；正文含 FAIL 时推理失败
#[derive(Default)]
struct Stub {
    embeds: AtomicUsize,
    entered: AtomicUsize,
    /// 为真时推理阻塞，直到改回假
    hold: Mutex<bool>,
    cv: Condvar,
    /// 下一次推理返回设备丢失
    lose: AtomicBool,
    /// 下一次查询返回后端失效（模拟推理子进程超时、崩溃）
    fail_query: AtomicBool,
    /// 查询的计算次数
    queries: AtomicUsize,
    /// 查询返回错误向量的剩余次数（模拟唤醒后结果异常）
    bad_queries: AtomicUsize,
    /// `restart` 的调用次数
    restarts: AtomicUsize,
    /// 为真时查询阻塞，直到改回假（与 `hold` 共用 `cv`）
    hold_queries: Mutex<bool>,
    /// 带音频的视频窗口超过 token 上限
    av_too_long: AtomicBool,
    /// 收到的视频窗口的预处理参数版本
    videos: Mutex<Vec<String>>,
}

/// 替身由输入长度决定的向量
fn stub_vector(seed: usize) -> Vec<f32> {
    let mut v = vec![0.01f32; 768];
    v[seed % 768] = 1.0;
    vector::normalize(v).unwrap()
}

impl EmbeddingBackend for Stub {
    fn embed(&self, input: Input, _: Priority) -> Result<Embedding, EmbedError> {
        let cfg = SpaceConfig::current(Backend::Cpu);
        // 查询不受 hold 阻塞，模拟插队
        if let Input::Query(q) = &input {
            self.queries.fetch_add(1, Ordering::Relaxed);
            let mut held = lock(&self.hold_queries);
            while *held {
                held = self.cv.wait(held).unwrap();
            }
            drop(held);
            if self.lose.swap(false, Ordering::Relaxed) {
                return Err(EmbedError::DeviceLost("模拟设备丢失".into()));
            }
            if self.fail_query.swap(false, Ordering::Relaxed) {
                return Err(EmbedError::Unavailable("模拟推理超时".into()));
            }
            let bad = self
                .bad_queries
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
                .is_ok();
            return Ok(Embedding {
                vector: stub_vector(q.len() + usize::from(bad)),
                space: cfg.text(),
            });
        }
        self.entered.fetch_add(1, Ordering::Relaxed);
        let mut held = lock(&self.hold);
        while *held {
            held = self.cv.wait(held).unwrap();
        }
        drop(held);
        if self.lose.swap(false, Ordering::Relaxed) {
            return Err(EmbedError::DeviceLost("模拟设备丢失".into()));
        }
        let (seed, space) = match &input {
            Input::Image { rgb, tokens } => (rgb.data.len(), cfg.image(*tokens)),
            Input::Audio {
                samples,
                preprocess,
            } => (samples.len(), cfg.media(preprocess)),
            Input::Video {
                parts, preprocess, ..
            } => {
                lock(&self.videos).push(preprocess.clone());
                if preprocess.contains("-av") && self.av_too_long.load(Ordering::Relaxed) {
                    return Err(EmbedError::TooLong("模拟超过上限".into()));
                }
                (parts.len(), cfg.media(preprocess))
            }
            Input::Document { text, .. } => {
                if text.contains("FAIL") {
                    return Err(EmbedError::Inference("模拟推理失败。".into()));
                }
                if text.contains("NAN") {
                    return Err(EmbedError::BadVector("模拟算出 NaN".into()));
                }
                (text.len(), cfg.text())
            }
            _ => unreachable!("索引不编码查询"),
        };
        self.embeds.fetch_add(1, Ordering::Relaxed);
        Ok(Embedding {
            vector: stub_vector(seed),
            space,
        })
    }

    fn restart(&self) -> Result<(), EmbedError> {
        self.restarts.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn count_tokens(&self, input: &Input, _: Priority) -> Result<usize, EmbedError> {
        let text = match input {
            Input::Document { text, .. } => text.as_str(),
            _ => "",
        };
        Ok(12 + text.chars().count() / 2)
    }
}

/// 显卡引擎的推理替身：转给 `Stub`，文本向量的空间换成 Vulkan 后端的（只用于文本）
struct GpuStub(Arc<Stub>);

impl EmbeddingBackend for GpuStub {
    fn embed(&self, input: Input, priority: Priority) -> Result<Embedding, EmbedError> {
        let mut e = self.0.embed(input, priority)?;
        e.space = SpaceConfig::current(Backend::Vulkan).text();
        Ok(e)
    }

    fn count_tokens(&self, input: &Input, priority: Priority) -> Result<usize, EmbedError> {
        self.0.count_tokens(input, priority)
    }
}

struct Env {
    pipeline: Option<Pipeline>,
    stub: Arc<Stub>,
    builds: Arc<AtomicUsize>,
    db: Arc<Mutex<Db>>,
    events: broadcast::Sender<Event>,
    root: crate::TestDir,
    data: crate::TestDir,
    root_id: i64,
    /// 单文件时限：只有超时测试用得很短，其他测试留足余量（debug 构建下解码缩放大图较慢）
    timeout: Duration,
    /// 覆盖参数 `index_threads`
    threads: Option<i64>,
    /// 覆盖参数 `index_images`
    images_param: Option<bool>,
    /// 覆盖其他参数
    extra: Vec<(&'static str, ParamValue)>,
    /// 创建后端的次数超过它之后都失败
    max_builds: usize,
    /// 为真时模拟系统日志中查到显卡驱动事件
    driver_events: Arc<AtomicBool>,
    /// 后端工厂可以建显卡引擎（`GpuStub`）；默认要求不用显卡
    gpu: bool,
}

fn env(files: &[(&str, &[u8])]) -> Env {
    let data = crate::test_dir();
    let root = crate::test_dir();
    let db = Db::open(data.path()).unwrap();
    let root_id = db.add_root(&normalize_root(root.path()).unwrap()).unwrap();
    let e = Env {
        pipeline: None,
        stub: Arc::new(Stub::default()),
        builds: Arc::new(AtomicUsize::new(0)),
        db: Arc::new(Mutex::new(db)),
        events: broadcast::channel(256).0,
        root,
        data,
        root_id,
        timeout: Duration::from_secs(20),
        threads: None,
        images_param: None,
        extra: Vec::new(),
        max_builds: usize::MAX,
        driver_events: Arc::new(AtomicBool::new(false)),
        gpu: false,
    };
    for (name, bytes) in files {
        e.put(name, bytes);
    }
    e.scan();
    e
}

impl Env {
    fn put(&self, name: &str, bytes: &[u8]) {
        fs::write(self.root.path().join(name), bytes).unwrap();
    }

    fn scan(&self) {
        let limits = crate::TEST_LIMITS;
        scan_root(
            &self.db,
            self.root_id,
            limits,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
        if let Some(p) = &self.pipeline {
            p.wake();
        }
    }

    fn model(&self, name: &str) -> PathBuf {
        self.data.path().join("models").join(name)
    }

    fn start(&mut self, model: bool, mmproj: bool) {
        fs::create_dir_all(self.data.path().join("models")).unwrap();
        if model {
            fs::write(self.model("m.gguf"), b"x").unwrap();
        }
        if mmproj {
            fs::write(self.model("p.gguf"), b"x").unwrap();
        }
        let (stub, builds, max_builds) = (self.stub.clone(), self.builds.clone(), self.max_builds);
        let gpu = self.gpu;
        let found = self.driver_events.clone();
        let opts = Options {
            model: self.model("m.gguf"),
            mmproj: self.model("p.gguf"),
            selftest_cache: self.data.path().join("cache").join("selftest.json"),
            ffmpeg: ffmpeg_dir(),
            embed_tmp: self.data.path().join("tmp").join("embed"),
            available: |path, _| path.is_file(),
            factory: Box::new(move |spec| {
                assert!(gpu || spec.gpu.is_none());
                if builds.fetch_add(1, Ordering::Relaxed) >= max_builds {
                    return Err(EmbedError::Spawn("模拟启动失败".into()));
                }
                if spec.gpu.is_some() {
                    return Ok(Arc::new(GpuStub(stub.clone())) as Arc<dyn EmbeddingBackend>);
                }
                Ok(stub.clone() as Arc<dyn EmbeddingBackend>)
            }),
            driver_events: Box::new(move |_| {
                Ok(if found.load(Ordering::Relaxed) {
                    vec!["Display 事件 4101（2026-10-13T01:02:03Z）".into()]
                } else {
                    Vec::new()
                })
            }),
            crashed: |_| Ok(true),
            file_timeout: Some(self.timeout),
            gpus: Vec::new,
            litert: None,
        };
        let mut values = params::defaults();
        if let Some(n) = self.threads {
            values.insert("index_threads".into(), ParamValue::Int(n));
        }
        if let Some(on) = self.images_param {
            values.insert("index_images".into(), ParamValue::Bool(on));
        }
        for (k, v) in &self.extra {
            values.insert(k.to_string(), *v);
        }
        let params = Arc::new(Mutex::new(values));
        let index = VIndex::load(&lock(&self.db)).unwrap();
        self.pipeline = Some(Pipeline::start(
            self.db.clone(),
            Arc::new(Mutex::new(index)),
            params,
            self.events.clone(),
            opts,
            Default::default(),
            None,
        ));
    }

    fn status(&self) -> IndexStatus {
        self.pipeline.as_ref().unwrap().status().unwrap()
    }

    /// 等到空闲且待处理的作业数为 `pending`
    fn wait_idle(&self, pending: u64) -> IndexStatus {
        self.wait_until(|s| s.state == IndexState::Idle && s.pending == pending)
    }

    fn wait_until(&self, done: impl Fn(&IndexStatus) -> bool) -> IndexStatus {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let s = self.status();
            if done(&s) {
                return s;
            }
            assert!(Instant::now() < deadline, "等待索引完成超时：{s:?}");
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// 另开一个连接查询
    fn conn(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.data.path().join("index.db")).unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        self.conn().query_row(sql, [], |row| row.get(0)).unwrap()
    }

    fn strings(&self, sql: &str) -> Vec<String> {
        let conn = self.conn();
        let mut stmt = conn.prepare(sql).unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// (stage, attempts, last_error)；没有作业时为空
    fn job(&self, name: &str) -> Option<(String, i64, Option<String>)> {
        self.conn()
            .query_row(
                "SELECT j.stage, j.attempts, j.last_error FROM jobs j JOIN files f ON f.id = j.file_id
                 WHERE f.name = ?1",
                [name],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .ok()
    }

    fn stored(&self, name: &str) -> bool {
        self.job(name).is_some_and(|j| j.0 == "stored")
    }
}

impl Drop for Env {
    /// 等各线程释放数据库，临时目录才能删掉
    fn drop(&mut self) {
        if let Some(p) = self.pipeline.take() {
            p.stop();
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Arc::strong_count(&self.db) > 1 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
    }
}

#[test]
fn text_and_image_stored() {
    let corgi = fs::read(CORGI).unwrap();
    let mut e = env(&[
        (
            "说明.md",
            "# 安装\n\n第一段文字。\n\n## Windows\n\n第二段文字。\n".as_bytes(),
        ),
        ("笔记.txt", "今天整理了索引流水线的设计。".as_bytes()),
        ("main.rs", b"fn main() {\n    println!(\"hi\");\n}\n"),
        ("狗.jpg", &corgi),
    ]);
    let mut rx = e.events.subscribe();
    e.start(true, true);
    let s = e.wait_idle(0);
    // 代码文件只按文件名索引，没有作业
    assert_eq!((s.total, s.done, s.failed), (3, 3, 0));
    assert_eq!(e.job("main.rs"), None);
    assert_eq!(s.by_kind["image"].done, 1);
    assert_eq!(s.by_kind["document"].done, 2);
    assert_eq!(s.backend.as_deref(), Some(rummage_embed::cpu_name()));

    let cfg = SpaceConfig::current(Backend::Cpu);
    assert_eq!(
        e.strings(
            "SELECT id || ' ' || preprocess || ' ' || ifnull(mmproj_sha256, '-') FROM spaces
             ORDER BY preprocess"
        ),
        [
            format!("{} image-v1-t280 {}", cfg.image(280), space::MMPROJ.sha256),
            format!("{} text-v1 -", cfg.text()),
        ]
    );
    let segments = e.count("SELECT COUNT(*) FROM segments");
    assert!(segments >= 3);
    assert_eq!(
        e.count("SELECT COUNT(*) FROM vectors WHERE dim = 768 AND length(vec) = 1536"),
        segments
    );
    assert_eq!(e.stub.embeds.load(Ordering::Relaxed) as i64, segments);
    assert_eq!(
        e.count("SELECT COUNT(*) FROM files WHERE status = 'stored' AND quick_hash IS NOT NULL"),
        3
    );
    // 文件 ID 随 quick_hash 记下，只在 NTFS 上有
    #[cfg(windows)]
    assert_eq!(
        e.count(
            "SELECT COUNT(*) FROM files WHERE status = 'stored' AND volume_file_id IS NOT NULL"
        ),
        3
    );
    assert_eq!(
        e.strings("SELECT name FROM files WHERE content_hash IS NOT NULL"),
        ["狗.jpg"]
    );
    // 图片记录解码后的宽高，其他文件没有
    let img = rummage_media::decode(std::path::Path::new(CORGI), 100_000_000).unwrap();
    assert_eq!(
        e.strings(
            "SELECT name || ' ' || width || 'x' || height FROM files WHERE width IS NOT NULL"
        ),
        [format!("狗.jpg {}x{}", img.width, img.height)]
    );
    assert_eq!(
        e.strings(
            "SELECT s.modality || s.locator_json || ifnull(s.text, '-') || v.params_version
               || v.space_id
             FROM segments s JOIN files f ON f.id = s.file_id JOIN vectors v ON v.segment_id = s.id
             WHERE f.name = '狗.jpg'"
        ),
        [format!("image{{}}-image-v1-t280{}", cfg.image(280))]
    );
    assert_eq!(
        e.count(&format!(
            "SELECT COUNT(*) FROM vectors WHERE params_version = 'text-v1-t64-c384-o15-m64' AND space_id = '{}'",
            cfg.text()
        )),
        segments - 1
    );
    // 文本片段的定位：字符与行的范围、Markdown 的标题路径
    let locs = e.strings(
        "SELECT s.locator_json FROM segments s JOIN files f ON f.id = s.file_id
         WHERE f.name = '说明.md' ORDER BY s.id",
    );
    let first: serde_json::Value = serde_json::from_str(&locs[0]).unwrap();
    for key in ["char_start", "char_end", "line_start", "line_end"] {
        assert!(first[key].is_u64(), "{key}：{first}");
    }
    assert!(first["heading"].is_string(), "{first}");
    assert!(locs.iter().any(|l| l.contains("安装")), "{locs:?}");

    // 空闲后广播过带全量计数的进度
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match rx.try_recv() {
            Ok(Event::IndexProgress(p)) if p.state == IndexState::Idle && p.done == 3 => break,
            Ok(_) => {}
            Err(_) => thread::sleep(Duration::from_millis(20)),
        }
        assert!(Instant::now() < deadline, "没有收到空闲时的进度事件");
    }
}

/// `width`×`height` 的 24 位 BMP（自下而上存行，行按 4 字节对齐），颜色随位置变化
fn bmp(width: u32, height: u32) -> Vec<u8> {
    let row = (width * 3).div_ceil(4) * 4;
    let size = 54 + row * height;
    let mut out = Vec::with_capacity(size as usize);
    out.extend_from_slice(b"BM");
    for v in [size, 0, 54, 40] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    for y in 0..height {
        for x in 0..width {
            out.extend_from_slice(&[x as u8, y as u8, 128]);
        }
        out.resize(out.len() + (row - width * 3) as usize, 0);
    }
    out
}

#[test]
fn text_only_without_mmproj() {
    let corgi = fs::read(CORGI).unwrap();
    let mut e = env(&[("a.txt", "内容".as_bytes()), ("b.jpg", &corgi)]);
    e.start(true, false);
    let s = e.wait_until(|s| s.state == IndexState::NoMmproj && s.pending == 1);
    assert_eq!(s.by_kind["image"].pending, 1);
    assert!(e.stored("a.txt"));
    assert_eq!(e.job("b.jpg").unwrap().0, "pending");
}

/// 开发缓存中的 ffmpeg（`scripts/fetch-ffmpeg.ps1`）
fn ffmpeg_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.cache/third-party/ffmpeg")
        .join(rummage_media::ffmpeg::FFMPEG_BUILD)
}

/// 找不到 ffmpeg 时跳过并提示
fn has_ffmpeg() -> bool {
    let ok = cfg!(windows) && ffmpeg_dir().join("ffmpeg.exe").is_file();
    if !ok {
        println!(
            "跳过：找不到 {}，请先运行 scripts/fetch-ffmpeg.ps1",
            ffmpeg_dir().display()
        );
    }
    ok
}

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../rummage-extract/fixtures/");

#[test]
fn xberg_documents() {
    let read = |name: &str| fs::read(format!("{FIXTURES}{name}")).unwrap();
    let mut e = env(&[
        ("a.pdf", &read("sample.pdf")),
        ("b.pptx", &read("sample.pptx")),
        ("c.docx", &read("sample.docx")),
        ("locked.pdf", &read("encrypted.pdf")),
        ("bad.pdf", b"%PDF-1.4\n not really"),
    ]);
    e.start(true, true);
    e.wait_idle(0);
    for name in ["a.pdf", "b.pptx", "c.docx"] {
        assert!(e.stored(name), "{name}");
    }
    let locators = |name: &str| {
        e.strings(&format!(
            "SELECT s.locator_json FROM segments s JOIN files f ON f.id = s.file_id
             WHERE f.name = '{name}' ORDER BY s.id"
        ))
    };
    let pdf = locators("a.pdf");
    assert!(pdf[0].contains(r#""page_start":1"#), "{pdf:?}");
    assert!(pdf.last().unwrap().contains(r#""page_end":2"#), "{pdf:?}");
    assert!(pdf.iter().all(|l| !l.contains("line_start")));
    assert!(locators("b.pptx")[0].contains(r#""slide_start":1"#));
    let docx = locators("c.docx");
    // 引言短于最短分段长度，并入下一节，标题路径取正文较长的一方
    assert!(
        docx[0].contains(r#""heading":"安装指南 > Windows""#),
        "{docx:?}"
    );
    assert!(docx.iter().all(|l| !l.contains("page_")));
    // 加密：只保留元数据，不计失败次数、不进黑名单；坏文件照常计失败
    assert_eq!(
        e.job("locked.pdf"),
        Some(("failed".into(), 0, Some(rummage_index::ENCRYPTED.into())))
    );
    assert!(locators("locked.pdf").is_empty());
    assert_eq!(e.job("bad.pdf").unwrap().1, 3);
    assert_eq!(e.strings("SELECT COUNT(*) || '' FROM blacklist"), ["1"]);
}

/// 自检记录中选中的 GPU（只用于构造引擎，不加载任何 GPU 后端）
fn fake_gpu() -> selftest::CandidateReport {
    selftest::CandidateReport {
        backend: Backend::Vulkan,
        name: "Vulkan0".into(),
        description: "测试显卡".into(),
        pci_id: Some("0000:01:00.0".into()),
        driver: Some("1.0".into()),
        status: selftest::Status::Passed,
        reason: None,
        samples: Vec::new(),
        init_ms: None,
        text_ms: None,
        image_ms: None,
        audio_ms: None,
        video_ms: None,
        speed: None,
        driver_error: false,
        device_lost: false,
        transient: false,
        stuck: false,
    }
}

/// 等到推理替身被调用过
fn wait_entered(e: &Env) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while e.stub.entered.load(Ordering::Relaxed) == 0 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
}

fn release_hold(e: &Env) {
    *lock(&e.stub.hold) = false;
    e.stub.cv.notify_all();
}

/// 绑定替身的“GPU”引擎，向量空间沿用当前的 CPU 引擎
fn gpu_engine(stub: &Arc<Stub>, cpu: &Engine) -> Arc<Engine> {
    Arc::new(Engine {
        backend: stub.clone(),
        gpu: Some(fake_gpu()),
        device: "测试显卡".into(),
        images: true,
        audio: true,
        video: true,
        text_space: cpu.text_space.clone(),
        cfg: cpu.cfg.clone(),
        failed: AtomicBool::new(false),
        driver_failed: AtomicBool::new(false),
        retired: AtomicBool::new(false),
        released: Released(Arc::new(AtomicBool::new(false))),
    })
}

impl Env {
    /// 改参数并按新设置放回重建（核心修改影响索引的参数时的做法）
    fn set_and_requeue(&self, key: &str, value: ParamValue) -> BTreeMap<String, usize> {
        let p = self.pipeline.as_ref().unwrap();
        lock(&p.shared.params).insert(key.into(), value);
        p.requeue(&crate::pipeline::void_kinds(key)).unwrap()
    }
}
