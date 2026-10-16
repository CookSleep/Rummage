//! 文档的内嵌媒体：按作业要算的部分（`jobs.parts`）分开替换正文与内嵌媒体的片段。
use rummage_index::{PART_MEDIA, PART_TEXT};

use super::*;

fn fixture(name: &str) -> Vec<u8> {
    fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../rummage-extract/fixtures/media")
            .join(name),
    )
    .unwrap()
}

impl Env {
    /// 某个模态的片段 ID
    fn segment_ids(&self, modality: &str) -> Vec<i64> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT id FROM segments WHERE modality = ?1 ORDER BY id")
            .unwrap();
        stmt.query_map([modality], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// 把文档重新放回、只算 `parts`，等它算完；返回这次推理的片段数
    fn redo(&self, parts: u8) -> usize {
        let before = self.stub.embeds.load(Ordering::Relaxed);
        self.conn()
            .execute(
                "UPDATE jobs SET stage = 'pending', parts = ?1",
                [i64::from(parts)],
            )
            .unwrap();
        self.pipeline.as_ref().unwrap().wake();
        self.wait_until(|s| {
            s.state == IndexState::Idle
                && s.pending == 0
                && self.stored("图文.docx")
                && self.stub.embeds.load(Ordering::Relaxed) > before
        });
        self.stub.embeds.load(Ordering::Relaxed) - before
    }
}

#[test]
fn document_media_by_part() {
    let mut e = env(&[("图文.docx", &fixture("images.docx"))]);
    e.start(true, true);
    let s = e.wait_idle(0);
    assert_eq!((s.done, s.failed), (1, 0));
    let text = e.segment_ids("text");
    let images = e.segment_ids("image");
    assert!(!text.is_empty());
    // 两张不同的照片；重复出现的、小图标与页眉中的图跳过
    assert_eq!(images.len(), 2);
    assert_eq!(
        e.strings("SELECT json_extract(locator_json, '$.embed.part') FROM segments WHERE modality = 'image' ORDER BY id"),
        ["word/media/image2.jpg", "word/media/image4.jpg"]
    );
    assert_eq!(
        e.strings("SELECT DISTINCT v.params_version FROM vectors v JOIN segments s ON s.id = v.segment_id WHERE s.modality = 'image'"),
        ["image-v1-t140"]
    );
    let marker = e.strings("SELECT meta_json FROM files");
    assert!(
        marker[0].contains("docmedia-v1-p128-n100-r3-mp100-mb200"),
        "{marker:?}"
    );
    assert!(marker[0].contains("image-v1-t140"), "{marker:?}");

    // 只算内嵌媒体：只推理两张图，正文片段原样保留
    assert_eq!(e.redo(PART_MEDIA), 2);
    assert_eq!(e.segment_ids("text"), text);
    let images = e.segment_ids("image");
    assert_eq!(images.len(), 2);
    // 只算正文：内嵌片段原样保留
    assert_eq!(e.redo(PART_TEXT), text.len());
    assert_eq!(e.segment_ids("image"), images);
    assert_eq!(e.segment_ids("text").len(), text.len());
    // 全部
    assert_eq!(e.redo(PART_TEXT | PART_MEDIA), text.len() + 2);
    assert_eq!(
        e.count("SELECT COUNT(*) FROM segments"),
        (text.len() + 2) as i64
    );
}

#[test]
fn document_without_mmproj_keeps_marker_short() {
    // 没有图片与音视频模型时不抽取，标记只有过滤参数，装好后由重建补算
    let mut e = env(&[("图文.docx", &fixture("images.docx"))]);
    e.start(true, false);
    e.wait_idle(0);
    assert!(e.segment_ids("image").is_empty());
    assert_eq!(
        e.strings("SELECT json_extract(meta_json, '$.media') FROM files"),
        [r#"["docmedia-v1-p128-n100-r3-mp100-mb200"]"#]
    );
}

#[test]
fn presentation_audio_and_video() {
    if !has_ffmpeg() {
        return;
    }
    let mut e = env(&[("演示.pptx", &fixture("media.pptx"))]);
    e.start(true, true);
    let s = e.wait_idle(0);
    assert_eq!((s.done, s.failed), (1, 0));
    assert_eq!(
        e.strings("SELECT DISTINCT modality FROM segments ORDER BY modality"),
        ["audio", "image", "text", "video_v"]
    );
    // 不压缩存放的视频用 subfile 读，压缩的音频解压到临时目录，用完删除
    assert_eq!(
        e.strings("SELECT DISTINCT json_extract(locator_json, '$.embed.slide') || ' ' || modality FROM segments WHERE modality <> 'text' ORDER BY 1"),
        ["1 image", "2 video_v", "3 audio"]
    );
    assert!(
        e.count("SELECT COUNT(*) FROM segments WHERE modality = 'video_v' AND json_extract(locator_json, '$.end_ms') > 0")
            > 0
    );
    let tmp = e.data.path().join("tmp").join("embed");
    assert!(!tmp.exists() || fs::read_dir(&tmp).unwrap().next().is_none());
}

/// 同一份文档里的音视频都超过内存额度：第一个照常处理，之后这份文档已持有额度，超额的跳过，不会互相等待而卡住
#[test]
fn oversized_media_in_one_document() {
    if !has_ffmpeg() {
        return;
    }
    let mut e = env(&[]);
    e.images_param = Some(false);
    e.start(true, true);
    let shared = &e.pipeline.as_ref().unwrap().shared;
    shared.media_mem.limit.store(1, Ordering::Relaxed);
    e.put("演示.pptx", &fixture("media.pptx"));
    e.scan();
    let s = e.wait_idle(0);
    assert_eq!((s.done, s.failed), (1, 0));
    // 分两批：第一批只有视频，第二批从断点接着算音频，最后两个都有，标记写成完成
    let media =
        e.strings("SELECT DISTINCT modality FROM segments WHERE modality <> 'text' ORDER BY 1");
    assert_eq!(media, ["audio", "video_v"]);
    let meta = e.strings("SELECT meta_json FROM files");
    assert!(
        meta[0].contains("\"media\"") && !meta[0].contains("media_partial"),
        "{meta:?}"
    );
}

/// 内嵌图片损坏：文档照常完成、正文可搜，出现在“失败文件”列表中，计数一致；重试只重算内嵌媒体；文件修改后从列表中消失
#[test]
fn broken_media_listed_as_failed() {
    let mut e = env(&[("坏图.docx", &fixture("broken.docx"))]);
    e.start(true, true);
    let s = e.wait_until(|s| s.state == IndexState::Idle && s.pending == 0 && s.failed == 1);
    assert_eq!((s.total, s.done), (1, 0));
    assert!(!e.segment_ids("text").is_empty());
    assert_eq!(e.segment_ids("image").len(), 1);
    let page = lock(&e.db).failed_files(None, 10).unwrap();
    assert_eq!((page.total, page.retryable), (1, 1));
    assert_eq!(
        page.files[0].reason.as_deref(),
        Some("文档中有 1 个图片或音视频无法识别，正文可以正常搜索。")
    );
    // 重试：只重算内嵌媒体（推理 1 张好图），坏图仍无法识别
    let before = e.stub.embeds.load(Ordering::Relaxed);
    let id = page.files[0].file_id;
    assert_eq!(lock(&e.db).retry_failed(Some(id)).unwrap(), 1);
    e.pipeline.as_ref().unwrap().wake();
    let s = e.wait_until(|s| {
        s.state == IndexState::Idle
            && s.pending == 0
            && e.stub.embeds.load(Ordering::Relaxed) > before
    });
    assert_eq!(e.stub.embeds.load(Ordering::Relaxed) - before, 1);
    assert_eq!(s.failed, 1);
    // 换成完好的文件：重新排队，算完后不再在列表中
    thread::sleep(Duration::from_millis(20));
    e.put("坏图.docx", &fixture("images.docx"));
    e.scan();
    let s = e.wait_until(|s| s.state == IndexState::Idle && s.pending == 0 && s.done == 1);
    assert_eq!(s.failed, 0);
    assert_eq!(lock(&e.db).failed_files(None, 10).unwrap().total, 0);
}

#[test]
fn lease_without_waiting() {
    let mem = Arc::new(prep::MediaMem::default());
    mem.limit.store(100, Ordering::Relaxed);
    let stop = AtomicBool::new(false);
    // 没有别的占用时总能拿到，即使超额
    let mut big = mem.lease(150, false, &stop).ok().unwrap();
    assert!(matches!(
        mem.lease(1, false, &stop),
        Err(prep::MediaFail::NoMemory)
    ));
    // 缩放后只留缩放后的大小，又有余量
    big.shrink_to(50);
    let small = mem.lease(40, false, &stop).ok().unwrap();
    drop((big, small));
    assert!(mem.lease(100, false, &stop).is_ok());
}

/// 有别的申请在等额度时，已持有额度的文档不再不等待地申请，先让给等待的一方
#[test]
fn lease_yields_to_waiters() {
    let mem = Arc::new(prep::MediaMem::default());
    mem.limit.store(100, Ordering::Relaxed);
    let stop = Arc::new(AtomicBool::new(false));
    let held = mem.lease(80, true, &stop).ok().unwrap();
    // 另一个文件要 50，超额，等待
    let (m, s) = (mem.clone(), stop.clone());
    let waiter = thread::spawn(move || m.lease(50, true, &s).is_ok());
    let deadline = Instant::now() + Duration::from_secs(5);
    while mem.waiting.load(Ordering::Relaxed) == 0 {
        assert!(Instant::now() < deadline, "等待者没有开始等待");
        thread::sleep(Duration::from_millis(10));
    }
    // 10 本来放得下，但有人在等，不等待的申请让出
    assert!(matches!(
        mem.lease(10, false, &stop),
        Err(prep::MediaFail::NoMemory)
    ));
    drop(held);
    assert!(waiter.join().unwrap());
    assert_eq!(mem.waiting.load(Ordering::Relaxed), 0);
}

/// 图片、音频、视频的设置改了：正在处理的文档也作废，按新设置重算其内嵌媒体
#[test]
fn media_settings_void_documents_in_flight() {
    let mut e = env(&[("图文.docx", &fixture("images.docx"))]);
    *lock(&e.stub.hold) = true;
    e.start(true, true);
    e.wait_until(|_| e.job("图文.docx").is_some_and(|j| j.0 == "embedding"));
    let id: i64 = e.count("SELECT id FROM files");
    e.set_and_requeue("embed_min_px", ParamValue::Int(64));
    let p = e.pipeline.as_ref().unwrap();
    assert!(lock(&p.shared.stale).contains(&id));
    *lock(&e.stub.hold) = false;
    e.stub.cv.notify_all();
    e.wait_idle(0);
    // 作废的结果放回后按新设置重算，标记是新设置的
    let marker = e.strings("SELECT meta_json FROM files");
    assert!(marker[0].contains("docmedia-v1-p64-"), "{marker:?}");
}
