//! 文档内嵌媒体命中时的图：`/crop/<片段 ID>` 的片段属于文档的内嵌图片或视频时，
//! 按 locator 的 `embed` 从文档中重新取出那张图（视频取命中时间段中点的一帧），缩成卡片用的 WebP，与长图裁剪图一样缓存在
//! `thumbs\<十六进制>\` 下，按位置命名：PDF 为 `e-p<页>-<绘制顺序>`，zip 中的条目为 `e-<条目名 BLAKE3 的前 16 位>`，
//! 视频再加 `_t<毫秒>`。
//! 读取前做占位文件检查（`local_file`；`read_bytes` 与交给 ffmpeg 之前各再检查一次）。压缩存放的视频先解压到
//! `Config::embed_tmp`，取完即删除。
use std::fs;

use rummage_extract::media::{At, Embedded, Loaded, MediaOpts, Source};
use rummage_extract::read_bytes;
use rummage_index::{SegmentRow, ensure_local};
use rummage_proto::{ApiError, Reply, params};

use super::super::setup::config;
use super::{
    DERIVED, FRAME_TIMEOUT, State, api_error, cached, derived_dir, local_file, lock, save,
};

pub(super) fn embed_thumb(
    state: &State,
    seg: &SegmentRow,
    loc: &serde_json::Value,
) -> Result<Reply, ApiError> {
    let bad = |msg: &str| api_error(ApiError::BAD_REQUEST, msg);
    let embed = &loc["embed"];
    let at = Embedded::at_of(embed).ok_or_else(|| bad("这个结果不是文档中的图片或视频。"))?;
    let kind = embed["kind"].as_str().unwrap_or_default();
    let n = embed["n"].as_u64().unwrap_or(0);
    let mid = match (loc["start_ms"].as_u64(), loc["end_ms"].as_u64()) {
        (Some(a), Some(b)) => Some((a + b) / 2),
        _ => None,
    };
    // 按位置命名：设置改了、序号变了，同一位置的图仍是同一张
    let key = match &at {
        At::Pdf { page, pos } => format!("e-p{page}-{pos}"),
        At::Part(part) => format!("e-{}", &blake3::hash(part.as_bytes()).to_hex()[..16]),
    };
    let name = match (kind, mid) {
        ("image", _) => format!("{key}.webp"),
        ("video", Some(ms)) => format!("{key}_t{ms}.webp"),
        _ => return Err(bad("这个结果没有可以显示的画面。")),
    };
    let (f, dir, qh) = local_file(state, seg.file_id)?;
    let path = derived_dir(dir, &qh).join(name);
    if let Some(reply) = cached(&path) {
        return Ok(reply);
    }
    let _guard = lock(&DERIVED);
    if let Some(reply) = cached(&path) {
        return Ok(reply);
    }
    let max_doc = params::max_doc_bytes(&state.params());
    let opts = MediaOpts {
        images: true,
        audio: true,
        video: true,
        min_px: 1,
        max_count: usize::MAX,
        repeat_pages: usize::MAX,
        max_pixels: params::max_image_pixels(&state.params()),
        max_entry_bytes: max_doc,
    };
    let fail = |err: String| {
        log::warn!("无法取出 {} 中的第 {n} 个内嵌媒体：{err}", f.path.display());
        bad("无法取出文档中的这张图片。")
    };
    let src = read_bytes(&f.path, max_doc)
        .map_err(|err| err.to_string())
        .and_then(|b| Source::open(&f.ext, b).map_err(|err| err.to_string()))
        .map_err(fail)?
        .ok_or_else(|| bad("这个文件中没有可以取出的图片。"))?;
    let loaded = src.load(&at, &opts).map_err(|err| fail(err.to_string()))?;
    let img = match (loaded, mid) {
        (
            Loaded::Rgb {
                width,
                height,
                data,
            },
            _,
        ) => rummage_media::Rgb {
            width,
            height,
            data,
        },
        (Loaded::File(bytes), None) => {
            rummage_media::decode_bytes(&bytes, &f.path, opts.max_pixels)
                .map_err(|err| fail(err.to_string()))?
        }
        (Loaded::File(bytes), Some(ms)) => {
            let cfg = config(state)?;
            let (ffmpeg, tmp_dir) = (cfg.ffmpeg.clone(), cfg.embed_tmp.clone());
            // 压缩存放的视频：解压到临时文件，取完一帧即删除
            let tmp = tmp_dir.join(format!(
                "crop-{}-{n}.{}",
                seg.file_id,
                crate::pipeline::part_ext(embed["part"].as_str())
            ));
            let res = fs::create_dir_all(&tmp_dir)
                .and_then(|()| fs::write(&tmp, bytes))
                .map_err(|err| err.to_string())
                .and_then(|()| {
                    rummage_media::frame_at(&ffmpeg, &tmp, ms, FRAME_TIMEOUT)
                        .map_err(|err| err.to_string())
                });
            let _ = fs::remove_file(&tmp);
            res.map_err(fail)?
        }
        (Loaded::Range(start, end), Some(ms)) => {
            let ffmpeg = config(state)?.ffmpeg.clone();
            ensure_local(&f.path).map_err(|err| bad(&err.to_string()))?;
            let input = rummage_media::ffmpeg::subfile(&f.path, start, end);
            rummage_media::frame_at(&ffmpeg, &input, ms, FRAME_TIMEOUT)
                .map_err(|err| fail(err.to_string()))?
        }
        (Loaded::Range(..), None) => return Err(fail("图片不应按字节范围读取".to_string())),
    };
    let bytes = rummage_media::shrink_webp(img, &f.path).map_err(|err| fail(err.to_string()))?;
    save(dir, &path, &bytes)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use rummage_index::Db;
    use rummage_proto::Request;

    use super::*;
    use crate::server::{db, respond};

    #[test]
    fn crop_embedded_image() {
        let dir = crate::test_dir();
        let docs = crate::test_dir();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../rummage-extract/fixtures/media/images.docx");
        fs::copy(fixture, docs.path().join("图文.docx")).unwrap();
        let state = State::new("t".into(), Db::open(dir.path()));
        let root = rummage_index::normalize_root(docs.path()).unwrap();
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
        let conn = rusqlite::Connection::open(dir.path().join("index.db")).unwrap();
        let insert = |locator: &str| -> i64 {
            conn.execute(
                "INSERT INTO segments (file_id, modality, locator_json, created_at)
                 SELECT id, 'image', ?1, 0 FROM files",
                [locator],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        let image = insert(
            r#"{"embed":{"kind":"image","n":1,"part":"word/media/image2.jpg","width":200,"height":160}}"#,
        );
        let Ok(Reply::Thumbnail { path }) =
            respond(Request::RegionThumb { segment_id: image }, &state)
        else {
            panic!("应返回内嵌图片");
        };
        let key = &blake3::hash(b"word/media/image2.jpg").to_hex()[..16];
        assert!(path.ends_with(&format!("e-{key}.webp")), "{path}");
        assert!(fs::metadata(&path).unwrap().len() > 0);
        // 第二次直接用缓存
        let Ok(Reply::Thumbnail { path: again }) =
            respond(Request::RegionThumb { segment_id: image }, &state)
        else {
            panic!("应返回缓存的图");
        };
        assert_eq!(again, path);
        // 找不到的条目、音频都不给图
        let missing = insert(r#"{"embed":{"kind":"image","n":2,"part":"word/media/无.png"}}"#);
        let err = respond(
            Request::RegionThumb {
                segment_id: missing,
            },
            &state,
        )
        .unwrap_err();
        assert_eq!(err.code, ApiError::BAD_REQUEST);
        let audio = insert(
            r#"{"embed":{"kind":"audio","n":3,"part":"word/media/a.m4a"},"start_ms":0,"end_ms":10}"#,
        );
        let err = respond(Request::RegionThumb { segment_id: audio }, &state).unwrap_err();
        assert!(err.message.ends_with('。'), "{}", err.message);
    }
}
