//! 按索引中的 ID 处理的请求：文件信息（打开、在资源管理器中显示、预览）、缩略图、
//! 视频画面帧、长图命中区域的裁剪图、片段正文与前后文。
//! 界面只能传索引中的文件 ID 或片段 ID（视频帧另带时刻），路径与长图区域由这里从数据库查出，不接受任意路径。
//! 缩略图：
//! - 设备未连接、网盘占位文件（`ensure_local`）不生成，回 `bad_request`；
//! - 按 `quick_hash` 命名，存为 `<数据目录>\thumbs\<十六进制>.webp`：内容不变就复用，内容相同的文件共用一张；
//!   文件还没算过 `quick_hash`（尚未建索引或不建内容索引）时现算一次并写回（文件未变时），自动清理据此判断仍被引用；
//! - 先写到数据目录的 `tmp\` 再改名，同一文件的并发请求不会读到写了一半的图；
//! - 内容变化后旧缩略图不再被引用，由 `data` 自动清理。
//!
//! 视频画面帧与长图裁剪图在结果显示时才生成，同样先做上面的检查，存在 `thumbs\<十六进制>\` 目录下
//! （`t<毫秒>.webp`、`r<x>_<y>_<宽>_<高>.webp`），随同一 `quick_hash` 的缩略图一起清理。生成过程串行执行（`DERIVED`），
//! 拿到锁后再看一次缓存：卡片和详情栏同时要同一帧时只生成一次，也不会同时启动一串 ffmpeg。
//!
//! 文档的内嵌图片与视频命中时，`/crop` 从文档中重新取出那张图（`embed`）。
//!
//! 文档预览：命中片段的正文，加上按字符范围排在它前后的片段中不与它重叠的部分，各取一个段落。
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rummage_index::{FileRow, SegmentRow, ensure_local, quick_hash};
use rummage_media::Region;
use rummage_proto::{ApiError, FileInfo, JobInfo, Reply, SegmentPreview, VectorSpaceInfo, params};

mod embed;

use super::setup::config;
use super::{State, api_error, db, lock, opened, unavailable};

/// 区分并发写入的临时文件名
static NEXT_TMP: AtomicU64 = AtomicU64::new(0);
/// 视频画面帧与长图裁剪图的生成串行执行（见模块说明）
static DERIVED: Mutex<()> = Mutex::new(());
/// 取一帧画面的时限：快速定位后通常不到 1 秒，网络盘或损坏的文件可能卡住
const FRAME_TIMEOUT: Duration = Duration::from_secs(20);
/// 文档预览中前后文各自的最多字符数，超出的部分用“…”代替
const CONTEXT_CHARS: usize = 300;

fn hex(quick_hash: &[u8]) -> String {
    quick_hash.iter().map(|b| format!("{b:02x}")).collect()
}

/// 缩略图的位置：按 `quick_hash` 的十六进制命名
pub(super) fn thumb_path(data_dir: &Path, quick_hash: &[u8]) -> PathBuf {
    data_dir
        .join("thumbs")
        .join(format!("{}.webp", hex(quick_hash)))
}

/// 同一内容的视频画面帧与长图裁剪图所在的目录
pub(super) fn derived_dir(data_dir: &Path, quick_hash: &[u8]) -> PathBuf {
    data_dir.join("thumbs").join(hex(quick_hash))
}

fn row(state: &State, id: i64) -> Result<FileRow, ApiError> {
    db(state)?.file(id).map_err(unavailable)?.ok_or_else(|| {
        api_error(
            ApiError::BAD_REQUEST,
            "找不到这个文件，它可能已从索引中移除。",
        )
    })
}

pub(super) fn info(state: &State, id: i64) -> Result<Reply, ApiError> {
    let f = row(state, id)?;
    let idx = db(state)?.index_info(id).map_err(unavailable)?;
    Ok(Reply::File(FileInfo {
        file_id: f.id,
        path: f.path.to_string_lossy().into_owned(),
        name: f.name,
        ext: f.ext,
        kind: f.kind,
        cloud: f.cloud,
        offline: f.offline,
        missing: f.missing,
        size: f.size,
        mtime: f.mtime,
        width: f.width,
        height: f.height,
        status: f.status,
        job: idx.job.map(|j| JobInfo {
            stage: j.stage,
            attempts: u32::try_from(j.attempts).unwrap_or(0),
            last_error: j.last_error,
        }),
        segments: u32::try_from(idx.segments).unwrap_or(0),
        indexed_at: idx.indexed_at,
        spaces: idx
            .spaces
            .into_iter()
            .map(|(preprocess, backend)| VectorSpaceInfo {
                preprocess,
                backend,
            })
            .collect(),
    }))
}

/// 片段 locator 中的字符范围 `[char_start, char_end)`
fn char_range(locator_json: &str) -> Option<(usize, usize)> {
    let v: serde_json::Value = serde_json::from_str(locator_json).ok()?;
    let start = usize::try_from(v.get("char_start")?.as_u64()?).ok()?;
    let end = usize::try_from(v.get("char_end")?.as_u64()?).ok()?;
    (start < end).then_some((start, end))
}

/// 前文：上一片中排在命中片段之前的部分，取最后一个段落，过长时只留末尾
fn before_text(prev: &str, prev_start: usize, cur_start: usize) -> Option<String> {
    let part: String = prev.chars().take(cur_start - prev_start).collect();
    let para = part.trim_end().rsplit("\n\n").next().unwrap_or("").trim();
    let n = para.chars().count();
    if n == 0 {
        return None;
    }
    if n <= CONTEXT_CHARS {
        return Some(para.to_string());
    }
    let tail: String = para.chars().skip(n - CONTEXT_CHARS).collect();
    Some(format!("…{}", tail.trim_start()))
}

/// 后文：下一片中排在命中片段之后的部分，取第一个段落，过长时只留开头
fn after_text(next: &str, next_start: usize, cur_end: usize) -> Option<String> {
    let part: String = next
        .chars()
        .skip(cur_end.saturating_sub(next_start))
        .collect();
    let para = part.trim_start().split("\n\n").next().unwrap_or("").trim();
    if para.is_empty() {
        return None;
    }
    if para.chars().count() <= CONTEXT_CHARS {
        return Some(para.to_string());
    }
    let head: String = para.chars().take(CONTEXT_CHARS).collect();
    Some(format!("{}…", head.trim_end()))
}

/// 命中片段与它的前后文：前一片是起点在它之前的最后一片，后一片是起点与终点都在它之后的第一片（相邻片段可能重叠）
fn preview(cur: SegmentRow, all: &[SegmentRow]) -> SegmentPreview {
    let mut out = SegmentPreview {
        file_id: cur.file_id,
        locator_json: cur.locator_json.clone(),
        text: cur.text,
        before: None,
        after: None,
    };
    let Some((start, end)) = char_range(&cur.locator_json) else {
        return out;
    };
    let ranged = || {
        all.iter().filter_map(|s| {
            let r = char_range(&s.locator_json)?;
            Some((r, s.text.as_deref()?))
        })
    };
    if let Some(((s, _), text)) = ranged()
        .filter(|((s, _), _)| *s < start)
        .max_by_key(|((s, _), _)| *s)
    {
        out.before = before_text(text, s, start);
    }
    if let Some(((s, _), text)) = ranged()
        .filter(|((s, e), _)| *s > start && *e > end)
        .min_by_key(|((s, _), _)| *s)
    {
        out.after = after_text(text, s, end);
    }
    out
}

pub(super) fn segment_text(state: &State, id: i64) -> Result<Reply, ApiError> {
    let db = db(state)?;
    let Some(cur) = db.segment(id).map_err(unavailable)? else {
        return Ok(Reply::Segment { segment: None });
    };
    let all = match (&cur.text, char_range(&cur.locator_json)) {
        (Some(_), Some((start, end))) => db
            .text_neighbors(cur.file_id, start, end)
            .map_err(unavailable)?,
        _ => Vec::new(),
    };
    Ok(Reply::Segment {
        segment: Some(preview(cur, &all)),
    })
}

/// 可以读取内容的文件、数据目录与文件的 `quick_hash`：设备未连接、网盘占位文件时回 `bad_request`；
/// 还没有 `quick_hash` 时现算并写回（`other` 不写回）。查完即释放数据库锁，生成图片期间不占着它
fn local_file(state: &State, id: i64) -> Result<(FileRow, &Path, Vec<u8>), ApiError> {
    let bad = |msg: String| api_error(ApiError::BAD_REQUEST, msg);
    let f = row(state, id)?;
    let dir = &opened(state)?.dir;
    if f.missing {
        return Err(bad("找不到文件所在的文件夹。".into()));
    }
    if f.offline {
        return Err(bad("文件所在的设备未连接。".into()));
    }
    ensure_local(&f.path).map_err(|err| bad(err.to_string()))?;
    let qh = match &f.quick_hash {
        Some(h) => h.clone(),
        None => {
            let h = quick_hash(&f.path)
                .map_err(|err| bad(err.to_string()))?
                .to_vec();
            // `other` 种类的 quick_hash 兼作 `.ts` “已判定不是视频”的标记，不能在这里写回
            if f.kind != "other"
                && let Err(err) = db(state)?.remember_quick_hash(f.id, f.size, f.mtime, &h)
            {
                log::warn!("无法记录 {} 的 quick_hash：{err}", f.path.display());
            }
            h
        }
    };
    Ok((f, dir, qh))
}

fn cached(path: &Path) -> Option<Reply> {
    path.is_file().then(|| Reply::Thumbnail {
        path: path.to_string_lossy().into_owned(),
    })
}

/// 先写到数据目录的 `tmp\` 再改名到 `path`
fn save(dir: &Path, path: &Path, bytes: &[u8]) -> Result<Reply, ApiError> {
    let tmp = dir.join("tmp").join(format!(
        "thumb-{}-{}.webp",
        std::process::id(),
        NEXT_TMP.fetch_add(1, Ordering::Relaxed)
    ));
    let saved = path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| fs::create_dir_all(dir.join("tmp")))
        .and_then(|()| fs::write(&tmp, bytes))
        .and_then(|()| fs::rename(&tmp, path));
    if let Err(err) = saved {
        let _ = fs::remove_file(&tmp);
        log::warn!("无法保存缩略图 {}：{err}", path.display());
        return Err(api_error(
            ApiError::UNAVAILABLE,
            "缩略图无法保存，请检查数据目录所在磁盘的空间。",
        ));
    }
    Ok(Reply::Thumbnail {
        path: path.to_string_lossy().into_owned(),
    })
}

pub(super) fn thumbnail(state: &State, id: i64) -> Result<Reply, ApiError> {
    let (f, dir, qh) = local_file(state, id)?;
    let path = thumb_path(dir, &qh);
    if let Some(reply) = cached(&path) {
        return Ok(reply);
    }
    let aspect = f
        .width
        .zip(f.height)
        .filter(|(w, h)| *w > 0 && *h > 0)
        .map(|(w, h)| f64::from(h) / f64::from(w));
    let bytes =
        rummage_media::thumbnail(&f.path, aspect, params::max_image_pixels(&state.params()))
            .map_err(|err| {
                log::warn!("无法生成 {} 的缩略图：{err}", f.path.display());
                api_error(ApiError::BAD_REQUEST, "无法生成这个文件的缩略图。")
            })?;
    save(dir, &path, &bytes)
}

pub(super) fn video_frame(state: &State, id: i64, ms: u64) -> Result<Reply, ApiError> {
    let ffmpeg = config(state)?.ffmpeg.clone();
    let (f, dir, qh) = local_file(state, id)?;
    if f.kind != "video" {
        return Err(api_error(ApiError::BAD_REQUEST, "只有视频才能取画面。"));
    }
    let path = derived_dir(dir, &qh).join(format!("t{ms}.webp"));
    if let Some(reply) = cached(&path) {
        return Ok(reply);
    }
    let _guard = lock(&DERIVED);
    if let Some(reply) = cached(&path) {
        return Ok(reply);
    }
    let bytes = rummage_media::frame_at(&ffmpeg, &f.path, ms, FRAME_TIMEOUT)
        .map_err(|err| err.to_string())
        .and_then(|img| rummage_media::shrink_webp(img, &f.path).map_err(|err| err.to_string()))
        .map_err(|err| {
            log::warn!("无法取 {} 在 {ms} 毫秒处的画面：{err}", f.path.display());
            api_error(ApiError::BAD_REQUEST, "无法取出这一刻的画面。")
        })?;
    save(dir, &path, &bytes)
}

/// 长图切片 locator 中的区域；不是切片（整图为 `{}`）时为空
fn region(locator_json: &str) -> Option<Region> {
    let v: serde_json::Value = serde_json::from_str(locator_json).ok()?;
    let r = v.get("region")?;
    let n = |k: &str| u32::try_from(r.get(k)?.as_u64()?).ok();
    Some(Region {
        x: n("x")?,
        y: n("y")?,
        width: n("width")?,
        height: n("height")?,
    })
}

pub(super) fn region_thumb(state: &State, segment_id: i64) -> Result<Reply, ApiError> {
    let bad = |msg: &str| api_error(ApiError::BAD_REQUEST, msg);
    let seg = db(state)?
        .segment(segment_id)
        .map_err(unavailable)?
        .ok_or_else(|| bad("找不到这个结果，它可能已从索引中移除。"))?;
    // 文档的内嵌图片或视频
    let loc: serde_json::Value = serde_json::from_str(&seg.locator_json).unwrap_or_default();
    if loc.get("embed").is_some() {
        return embed::embed_thumb(state, &seg, &loc);
    }
    let r = region(&seg.locator_json).ok_or_else(|| bad("这个结果不是长图的一段。"))?;
    let (f, dir, qh) = local_file(state, seg.file_id)?;
    let name = format!("r{}_{}_{}_{}.webp", r.x, r.y, r.width, r.height);
    let path = derived_dir(dir, &qh).join(name);
    if let Some(reply) = cached(&path) {
        return Ok(reply);
    }
    let _guard = lock(&DERIVED);
    if let Some(reply) = cached(&path) {
        return Ok(reply);
    }
    let img = rummage_media::decode(&f.path, params::max_image_pixels(&state.params())).map_err(
        |err| {
            log::warn!("无法解码长图 {}：{err}", f.path.display());
            bad("无法读取这张图片。")
        },
    )?;
    // 区域来自数据库，文件可能已被换成更小的图、还没重新索引
    let inside = r.width > 0
        && r.height > 0
        && u64::from(r.x) + u64::from(r.width) <= u64::from(img.width)
        && u64::from(r.y) + u64::from(r.height) <= u64::from(img.height);
    if !inside {
        return Err(bad("图片已经变化，请等重新索引后再试。"));
    }
    let bytes =
        rummage_media::shrink_webp(rummage_media::crop(&img, r), &f.path).map_err(|err| {
            log::warn!("无法生成 {} 的裁剪图：{err}", f.path.display());
            bad("无法生成这张图片的裁剪图。")
        })?;
    save(dir, &path, &bytes)
}

#[cfg(test)]
mod tests {
    use rummage_index::Db;
    use rummage_proto::Request;

    use super::*;
    use crate::server::respond;

    #[test]
    fn thumb_named_by_quick_hash() {
        let p = thumb_path(Path::new(r"D:\data"), &[0x01, 0xab, 0xff]);
        assert_eq!(p, Path::new(r"D:\data").join("thumbs").join("01abff.webp"));
        let d = derived_dir(Path::new(r"D:\data"), &[0x01, 0xab, 0xff]);
        assert_eq!(d, Path::new(r"D:\data").join("thumbs").join("01abff"));
    }

    fn seg(id: i64, start: usize, text: &str) -> SegmentRow {
        SegmentRow {
            id,
            file_id: 1,
            locator_json: format!(
                r#"{{"char_start":{start},"char_end":{},"heading":""}}"#,
                start + text.chars().count()
            ),
            text: Some(text.into()),
        }
    }

    #[test]
    fn preview_context_skips_overlap() {
        // 原文：“第一段。\n\n第二段甲。第二段乙。\n\n第三段。”，相邻片段重叠一句
        let all = [
            seg(1, 0, "第一段。\n\n第二段甲。"),
            seg(2, 6, "第二段甲。第二段乙。"),
            seg(3, 11, "第二段乙。\n\n第三段。"),
        ];
        let p = preview(all[1].clone(), &all);
        assert_eq!(p.text.as_deref(), Some("第二段甲。第二段乙。"));
        assert_eq!(p.before.as_deref(), Some("第一段。"));
        assert_eq!(p.after.as_deref(), Some("第三段。"));
        // 首尾片段没有前文或后文
        let first = preview(all[0].clone(), &all);
        assert_eq!(first.before, None);
        assert_eq!(first.after.as_deref(), Some("第二段乙。"));
        assert_eq!(preview(all[2].clone(), &all).after, None);
        // 过长的前后文截断
        let long = "字".repeat(CONTEXT_CHARS + 50);
        let all = [seg(1, 0, &long), seg(2, CONTEXT_CHARS + 50, "命中。")];
        let p = preview(all[1].clone(), &all);
        let before = p.before.unwrap();
        assert!(before.starts_with('…'));
        assert_eq!(before.chars().count(), CONTEXT_CHARS + 1);
        // 没有字符范围（图片、音视频）时只有正文
        let img = SegmentRow {
            id: 9,
            file_id: 1,
            locator_json: "{}".into(),
            text: None,
        };
        assert_eq!(preview(img, &all).before, None);
    }

    #[test]
    fn region_from_locator() {
        assert_eq!(
            region(r#"{"region":{"x":0,"y":2160,"width":1080,"height":1080}}"#),
            Some(Region {
                x: 0,
                y: 2160,
                width: 1080,
                height: 1080
            })
        );
        assert_eq!(region("{}"), None);
        assert_eq!(
            region(r#"{"region":{"x":-1,"y":0,"width":1,"height":1}}"#),
            None
        );
        assert_eq!(region("坏"), None);
    }

    fn conn(dir: &Path) -> rusqlite::Connection {
        rusqlite::Connection::open(dir.join("index.db")).unwrap()
    }

    /// 一个根目录下放一张黄金图片和一个文本文件，扫描入库，返回（状态, 图片 ID, 文本 ID）
    fn indexed(dir: &Path, docs: &Path) -> (std::sync::Arc<State>, i64, i64) {
        let corgi = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/golden/images/img02_photo_corgi.jpg");
        fs::copy(corgi, docs.join("狗.jpg")).unwrap();
        fs::write(docs.join("a.txt"), "正文").unwrap();
        let state = State::new("t".into(), Db::open(dir));
        let root = rummage_index::normalize_root(docs).unwrap();
        let root_id = db(&state).unwrap().add_root(&root).unwrap();
        let Ok(shared) = &state.db else {
            panic!("数据库应可用");
        };
        let limits = rummage_index::ScanLimits {
            max_doc_mb: 10,
            ..crate::TEST_LIMITS
        };
        let cancel = std::sync::atomic::AtomicBool::new(false);
        rummage_index::scan_root(&shared.db, root_id, limits, &cancel, &mut |_| {}).unwrap();
        let id = |name: &str| {
            conn(dir)
                .query_row("SELECT id FROM files WHERE name = ?1", [name], |r| r.get(0))
                .unwrap()
        };
        let (img, txt) = (id("狗.jpg"), id("a.txt"));
        (state, img, txt)
    }

    #[test]
    fn requests_by_file_id() {
        let dir = crate::test_dir();
        let docs = crate::test_dir();
        let (state, img, txt) = indexed(dir.path(), docs.path());

        let Ok(Reply::File(info)) = respond(Request::FileInfo { file_id: txt }, &state) else {
            panic!("应返回文件信息");
        };
        assert_eq!(
            (info.name.as_str(), info.kind.as_str()),
            ("a.txt", "document")
        );
        assert!(Path::new(&info.path).ends_with("a.txt"));
        assert!(!info.cloud && !info.offline);
        // 扫描后还没有建索引：有待处理的作业，没有片段
        assert_eq!(info.size, "正文".len() as i64);
        assert!(info.mtime > 0);
        assert_eq!(info.status, "pending");
        assert_eq!(info.job.as_ref().map(|j| j.stage.as_str()), Some("pending"));
        assert_eq!((info.segments, info.spaces.len()), (0, 0));
        assert_eq!(info.indexed_at, None);
        let Ok(Reply::File(img_info)) = respond(Request::FileInfo { file_id: img }, &state) else {
            panic!("应返回文件信息");
        };
        assert_eq!(img_info.kind, "image");
        let err = respond(Request::FileInfo { file_id: 999 }, &state).unwrap_err();
        assert_eq!(err.code, ApiError::BAD_REQUEST);
        assert!(err.message.ends_with('。'));

        // 尚未建索引、没有 quick_hash 时现算，按它命名
        let Ok(Reply::Thumbnail { path }) = respond(Request::Thumbnail { file_id: img }, &state)
        else {
            panic!("应返回缩略图");
        };
        let qh = quick_hash(&docs.path().join("狗.jpg")).unwrap();
        assert_eq!(Path::new(&path), thumb_path(dir.path(), &qh));
        let stored: Option<Vec<u8>> = conn(dir.path())
            .query_row("SELECT quick_hash FROM files WHERE id = ?1", [img], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(stored.as_deref(), Some(&qh[..]));
        let bytes = fs::read(&path).unwrap();
        assert_eq!(&bytes[8..12], b"WEBP");
        // 再次请求直接复用
        let Ok(Reply::Thumbnail { path: again }) =
            respond(Request::Thumbnail { file_id: img }, &state)
        else {
            panic!("应返回缩略图");
        };
        assert_eq!(again, path);
        // 不存在的 ID
        let err = respond(Request::Thumbnail { file_id: 999 }, &state).unwrap_err();
        assert_eq!(err.code, ApiError::BAD_REQUEST);

        // 设备未连接时不生成
        conn(dir.path())
            .execute("UPDATE roots SET offline = 1", [])
            .unwrap();
        let err = respond(Request::Thumbnail { file_id: img }, &state).unwrap_err();
        assert!(err.message.contains("未连接"), "{}", err.message);

        let Ok(Reply::Segment { segment }) =
            respond(Request::SegmentText { segment_id: 1 }, &state)
        else {
            panic!("应返回片段");
        };
        assert_eq!(segment, None);
    }
}
