//! 设置改了以后只重建受影响的文件：按当前设置应有的参数版本（`Wanted`，由核心按参数表算出）
//! 找出已完成、但向量的参数版本与之不符的文件。
//! - 文档看正文片段的文本版本（带分段参数）与内嵌媒体标记，只放回过时的部分（`jobs.parts`）；音频看音频版本。视频：只看画面的设置下须是纯画面版本；带音频的设置下须是
//!   带音频的版本（含片段长度上限；音轨读不出或超过上限而改为只看画面的，向量也记这个版本），或者是没有音轨
//!   （库中元数据 `has_audio` 为否）的纯画面版本。
//! - 图片：整图向量须是当前档位的版本；再按库中记下的宽高判断现在是不是长图：是长图却没有当前版本的切片、
//!   不是长图却有切片，或有其他版本的向量，都要重建。普通比例的图片只在档位变了时重建，调低判定比例后新成为长图的会补上切片。
//! - 只比较参数版本，不比较空间 ID：后端不同（CPU / Vulkan）的空间视为兼容，换了设备不重建。
//! - 没有向量的文件（全是静音的音频、没有正文的文档）无法判断，不重建。
//! - `requeue_stale` 把这些文件已完成的作业改回 `pending` 并标为重建作业（`jobs` 模块说明），片段与向量原样保留，
//!   新结果写入时才在同一事务中替换，重建期间旧向量照常可搜。占位文件不放回：领取时跳过它们，放回后会一直停在待处理。
use std::collections::BTreeMap;

use rusqlite::ToSql;
use rusqlite::{named_params, params};

use crate::db::{Db, DbError, now_ms};
use crate::jobs::{PART_MEDIA, PART_TEXT, REBUILD_BELOW, REBUILD_SHIFT};

/// 按当前设置各种类的向量应有的参数版本
#[derive(Debug, Clone, PartialEq)]
pub struct Wanted {
    /// 要检查的种类（`image`、`document`、`audio`、`video`）；关闭了内容索引或缺少模型的种类不放进来
    pub kinds: Vec<&'static str>,
    /// 文档
    pub text: String,
    /// 图片的整图向量
    pub image: String,
    /// 长图切片
    pub slice: String,
    /// 长边超过短边的这么多倍时为长图，与 `rummage_media::slices` 的判定相同
    pub long_ratio: f64,
    pub audio: String,
    /// 纯画面的视频窗口
    pub video: String,
    /// 带音频的视频窗口（含片段长度上限）；设置为只看画面时为空
    pub video_av: Option<String>,
    /// 文档的内嵌媒体标记应含的各项（JSON 数组）：过滤参数的版本与开启的各模态的参数版本
    pub doc_media: String,
}

/// 文档的正文片段有参数版本不是 `:text` 的向量
const TEXT_STALE: &str = "EXISTS (SELECT 1 FROM segments s JOIN vectors v ON v.segment_id = s.id
  WHERE s.file_id = f.id AND s.modality = 'text' AND v.params_version <> :text)";
/// 抽取内嵌媒体的文档格式（`rummage_extract::media` 同用这份清单）
pub const MEDIA_EXTS: &[&str] = &["pdf", "pptx", "docx", "xlsx", "odt", "ods"];

/// 该文件（`f`）有满足 `cond` 的向量（`v`）
fn has(cond: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM segments s JOIN vectors v ON v.segment_id = s.id
         WHERE s.file_id = f.id AND {cond})"
    )
}

impl Db {
    /// 把需要重建的已完成作业改回 `pending`、标为重建作业，返回各种类放回的数量（没有的种类不列出）
    pub fn requeue_stale(&self, w: &Wanted) -> Result<BTreeMap<String, usize>, DbError> {
        let now = now_ms();
        let mut out = BTreeMap::new();
        // 可能有内嵌媒体的文档（`MEDIA_EXTS`）的内嵌媒体标记（`meta_json` 的 `media`）缺少 `:media` 中的某一项。
        // 关闭某个模态或删除模型后它不在 `:media` 中，已有的内嵌片段保留，与单独的图片、音视频相同
        let exts = MEDIA_EXTS
            .iter()
            .map(|e| format!("'{e}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let media_stale = format!(
            "(f.ext IN ({exts}) AND EXISTS (SELECT 1 FROM json_each(:media) w
              WHERE w.value NOT IN (SELECT value FROM json_each(f.meta_json, '$.media'))))"
        );
        for kind in &w.kinds {
            let (cond, mut args): (String, Vec<(&str, &dyn ToSql)>) = match *kind {
                "image" => (
                    format!(
                        "NOT {} OR {}
                         OR (MIN(f.width, f.height) > 0 AND MAX(f.width, f.height) > :ratio * MIN(f.width, f.height)) <> {}",
                        has("v.params_version = :image"),
                        has("v.params_version NOT IN (:image, :slice)"),
                        has("v.params_version = :slice"),
                    ),
                    vec![
                        (":image", &w.image),
                        (":slice", &w.slice),
                        (":ratio", &w.long_ratio),
                    ],
                ),
                "audio" => (
                    has("v.params_version <> :audio"),
                    vec![(":audio", &w.audio)],
                ),
                "video" => (
                    format!(
                        "{} OR (:video_av IS NOT NULL AND IFNULL(json_extract(f.meta_json, '$.has_audio'), 1) AND {})",
                        has("v.params_version NOT IN (:video, IFNULL(:video_av, :video))"),
                        has("v.params_version = :video"),
                    ),
                    vec![(":video", &w.video), (":video_av", &w.video_av)],
                ),
                _ => (
                    format!("{TEXT_STALE} OR {media_stale}"),
                    vec![(":text", &w.text), (":media", &w.doc_media)],
                ),
            };
            // 文档只重算过时的部分（`jobs.parts`），其他种类整个重算
            let parts = if *kind == "document" {
                format!(
                    "(SELECT (CASE WHEN {TEXT_STALE} THEN {PART_TEXT} ELSE 0 END)
                       | (CASE WHEN {media_stale} THEN {PART_MEDIA} ELSE 0 END) FROM files f WHERE f.id = jobs.file_id)"
                )
            } else {
                "3".to_string()
            };
            args.extend([
                (":kind", kind as &dyn ToSql),
                (":now", &now),
                (":shift", &REBUILD_SHIFT),
                (":below", &REBUILD_BELOW),
            ]);
            let n = self
                .conn
                .execute(
                    &format!(
                        "UPDATE jobs SET stage = 'pending', attempts = 0, last_error = NULL, updated_at = :now,
                           priority = CASE WHEN priority < :below THEN priority ELSE priority - :shift END, parts = {parts}
                         WHERE stage = 'stored' AND file_id IN (
                           SELECT f.id FROM files f WHERE f.kind = :kind AND f.cloud = 0 AND ({cond}))"
                    ),
                    args.as_slice(),
                )
                .map_err(|err| self.err(err))?;
            if n > 0 {
                out.insert(kind.to_string(), n);
            }
            // 还在排队或处理中的文档补上过时的部分：处理中的结果按旧参数算出，会被放回再算（`Pipeline::requeue`）
            if *kind == "document" {
                self.conn
                    .execute(
                        &format!(
                            "UPDATE jobs SET parts = parts | {parts}
                             WHERE stage IN ('pending', 'preprocessing', 'embedding') AND file_id IN (
                               SELECT f.id FROM files f WHERE f.kind = 'document' AND ({cond}))"
                        ),
                        named_params! { ":text": &w.text, ":media": &w.doc_media },
                    )
                    .map_err(|err| self.err(err))?;
            }
        }
        Ok(out)
    }

    /// 还要重建的文件数（待处理与处理中的重建作业），键为种类。每次广播进度都要查，按阶段与优先级走 `jobs_stage` 索引。
    /// 条件与 `claimable_jobs` 相同：所在根目录在线、
    /// 文件不在云端、`images` / `audio` / `video` 为否时不含该类，免得永远领不到的作业让重建一直显示未完成
    pub fn rebuild_counts(
        &self,
        images: bool,
        audio: bool,
        video: bool,
    ) -> Result<BTreeMap<String, u64>, DbError> {
        self.conn
            .prepare(
                "SELECT f.kind, COUNT(*) FROM jobs j JOIN files f ON f.id = j.file_id JOIN roots r ON r.id = f.root_id
                 WHERE j.stage IN ('pending', 'preprocessing', 'embedding') AND j.priority < ?4 AND r.offline = 0 AND f.cloud = 0
                   AND (?1 OR f.kind <> 'image') AND (?2 OR f.kind <> 'audio') AND (?3 OR f.kind <> 'video')
                 GROUP BY f.kind",
            )
            .and_then(|mut stmt| {
                stmt.query_map(params![images, audio, video, REBUILD_BELOW], |row| {
                    Ok((row.get(0)?, row.get::<_, i64>(1)? as u64))
                })?
                .collect()
            })
            .map_err(|err| self.err(err))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::db::Space;
    use crate::jobs::{NewSegment, Outcome};
    use crate::scan::priority;
    use crate::search::tests::{Fixture, space, text_space, unit};
    use crate::search::{Filter, Query, SearchParams, search};
    use crate::vindex::VIndex;

    const SLICE: &str = "image-v1-t140-slice-v1-r2.5-n12";

    fn wanted() -> Wanted {
        Wanted {
            kinds: vec!["image", "document", "audio", "video"],
            text: "text-v1-c384-o15".into(),
            image: "image-v1-t140".into(),
            slice: SLICE.into(),
            long_ratio: 2.5,
            audio: "audio-v1-s10-o2-q-60".into(),
            video: "video-v1-w2-o0-f2-t70-v".into(),
            video_av: None,
            doc_media: "[]".into(),
        }
    }

    struct Env {
        fx: Fixture,
        root: i64,
        /// 写入的向量个数，各向量取不同的方向
        n: Mutex<usize>,
    }

    impl Env {
        fn new() -> Env {
            let fx = Fixture::new();
            let root = fx.root("D:\\资料");
            Env {
                fx,
                root,
                n: Mutex::new(0),
            }
        }

        /// 已完成的文件：宽高（图片）与各片段向量的参数版本
        fn file(&self, name: &str, kind: &str, size: Option<(u32, u32)>, versions: &[&str]) -> i64 {
            let id = self.fx.file(self.root, name, kind, 1_000);
            let db = self.fx.db.lock().unwrap();
            db.conn
                .execute(
                    "UPDATE files SET width = ?2, height = ?3 WHERE id = ?1",
                    params![id, size.map(|s| s.0), size.map(|s| s.1)],
                )
                .unwrap();
            db.conn
                .execute(
                    "INSERT INTO jobs (file_id, stage, priority, updated_at) VALUES (?1, 'stored', ?2, 0)",
                    params![id, priority(kind, 1_000)],
                )
                .unwrap();
            drop(db);
            for version in versions {
                let sp = Space {
                    id: format!("s-{version}"),
                    mmproj_sha256: (kind != "document").then(|| "mm".into()),
                    preprocess: version.to_string(),
                    ..text_space()
                };
                let mut n = self.n.lock().unwrap();
                let modality = if kind == "document" { "text" } else { "image" };
                self.fx.segment(id, modality, &unit(*n), &sp);
                *n += 1;
            }
            id
        }

        fn requeue(&self, w: &Wanted) -> BTreeMap<String, usize> {
            self.fx.db.lock().unwrap().requeue_stale(w).unwrap()
        }

        /// 作业为 `pending` 的文件名
        fn pending(&self) -> Vec<String> {
            let db = self.fx.db.lock().unwrap();
            let mut stmt = db
                .conn
                .prepare(
                    "SELECT f.name FROM jobs j JOIN files f ON f.id = j.file_id
                     WHERE j.stage = 'pending' ORDER BY f.name",
                )
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        }

        fn reset(&self) {
            self.fx
                .db
                .lock()
                .unwrap()
                .conn
                .execute("UPDATE jobs SET stage = 'stored'", [])
                .unwrap();
        }
    }

    /// 只改长图判定比例或分段上限：只有长图要重建，普通图片不动；没有切片的长图补上；不再是长图的删掉切片
    #[test]
    fn long_image_params_only_touch_long_images() {
        let e = Env::new();
        e.file("普通.png", "image", Some((640, 480)), &["image-v1-t140"]);
        e.file(
            "长图.png",
            "image",
            Some((100, 400)),
            &["image-v1-t140", SLICE, SLICE],
        );
        e.file("旧长图.png", "image", Some((100, 400)), &["image-v1-t140"]);
        e.file("说明.md", "document", None, &["text-v1-c384-o15"]);
        // 当前设置下只有没有切片的长图要重建
        assert_eq!(e.requeue(&wanted()), BTreeMap::from([("image".into(), 1)]));
        assert_eq!(e.pending(), ["旧长图.png"]);
        e.reset();

        // 分段上限改了：两张长图都要重建
        let w = Wanted {
            slice: "image-v1-t140-slice-v1-r2.5-n8".into(),
            ..wanted()
        };
        e.requeue(&w);
        assert_eq!(e.pending(), ["旧长图.png", "长图.png"]);
        e.reset();

        // 判定比例提高到 5：长宽比 4 的不再是长图，有切片的要删掉切片，没有切片的不动
        let w = Wanted {
            slice: "image-v1-t140-slice-v1-r5-n12".into(),
            long_ratio: 5.0,
            ..wanted()
        };
        e.requeue(&w);
        assert_eq!(e.pending(), ["长图.png"]);
    }

    #[test]
    fn image_tokens_rebuild_all_images() {
        let e = Env::new();
        e.file("普通.png", "image", Some((640, 480)), &["image-v1-t140"]);
        e.file(
            "长图.png",
            "image",
            Some((100, 400)),
            &["image-v1-t140", SLICE],
        );
        e.file("说明.md", "document", None, &["text-v1-c384-o15"]);
        assert!(e.requeue(&wanted()).is_empty());
        let w = Wanted {
            image: "image-v1-t280".into(),
            slice: "image-v1-t280-slice-v1-r2.5-n12".into(),
            ..wanted()
        };
        assert_eq!(e.requeue(&w), BTreeMap::from([("image".into(), 2)]));
        assert_eq!(e.pending(), ["普通.png", "长图.png"]);
    }

    #[test]
    fn video_params_only_touch_videos() {
        const AV: &str = "video-v1-w6-o0-f6-t70-av-m1024";
        const V: &str = "video-v1-w6-o0-f6-t70-v";
        let e = Env::new();
        let video = |name: &str, has_audio: Option<bool>, version: &str| {
            let id = e.file(name, "video", None, &[version]);
            let meta = has_audio.map_or("{}".to_string(), |a| format!(r#"{{"has_audio":{a}}}"#));
            e.fx.db
                .lock()
                .unwrap()
                .conn
                .execute(
                    "UPDATE files SET meta_json = ?2 WHERE id = ?1",
                    params![id, meta],
                )
                .unwrap();
        };
        video("有声.mp4", Some(true), AV);
        video("无声.mp4", Some(false), V);
        // 音轨读不出或超过片段长度上限，改为只看画面：向量记带音频的版本
        video("超长.mp4", Some(true), AV);
        // 在“找物体”的设置下建的、有音轨的视频，以及没有元数据的视频
        video("旧有声.mp4", Some(true), V);
        video("无元数据.mp4", None, V);
        e.file("普通.png", "image", Some((640, 480)), &["image-v1-t140"]);
        e.file("录音.mp3", "audio", None, &["audio-v1-s10-o2-q-60"]);
        e.file("说明.md", "document", None, &["text-v1-c384-o15"]);
        // 全是静音、没有片段的音频无法判断，不重建
        e.file("静音.wav", "audio", None, &[]);
        // “找说话内容”：有音轨却只有纯画面向量的要重建，其余都是当前的
        let speech = Wanted {
            video: V.into(),
            video_av: Some(AV.into()),
            ..wanted()
        };
        assert_eq!(e.requeue(&speech), BTreeMap::from([("video".into(), 2)]));
        assert_eq!(e.pending(), ["无元数据.mp4", "旧有声.mp4"]);
        e.reset();
        // 片段长度上限改了：带音频的版本都要重建，没有音轨的不动
        let longer = Wanted {
            video_av: Some("video-v1-w6-o0-f6-t70-av-m2048".into()),
            ..speech.clone()
        };
        e.requeue(&longer);
        assert_eq!(
            e.pending(),
            ["无元数据.mp4", "旧有声.mp4", "有声.mp4", "超长.mp4"]
        );
        e.reset();
        // 改为“找物体”：纯画面版本的都是当前的
        let objects = Wanted {
            video: V.into(),
            ..wanted()
        };
        assert_eq!(e.requeue(&objects), BTreeMap::from([("video".into(), 2)]));
        assert_eq!(e.pending(), ["有声.mp4", "超长.mp4"]);
        e.reset();
        // 分段长度改了：文档重建；不在 `kinds` 中的种类（关闭的类型）即使版本不符也不检查
        let w = Wanted {
            kinds: vec!["document"],
            text: "text-v1-c256-o15".into(),
            ..objects
        };
        assert_eq!(e.requeue(&w), BTreeMap::from([("document".into(), 1)]));
        assert_eq!(e.pending(), ["说明.md"]);
    }

    /// 文档只放回过时的部分：正文版本不符的只算正文，内嵌媒体标记缺项的只算内嵌媒体
    #[test]
    fn documents_requeue_by_part() {
        const MEDIA: &str = r#"["docmedia-v1-p128-n100-r3-mp100-mb200","image-v1-t140"]"#;
        let e = Env::new();
        let old_text = "text-v1-c256-o15";
        let new_text = "text-v1-c384-o15";
        let ids = [
            e.file("正文旧.docx", "document", None, &[old_text]),
            e.file("媒体旧.pdf", "document", None, &[new_text]),
            e.file("都旧.pdf", "document", None, &[old_text]),
            e.file("都新.pdf", "document", None, &[new_text]),
        ];
        // 内嵌图片的向量版本与正文不同，不算正文过时
        let sp = Space {
            id: "s-image".into(),
            mmproj_sha256: Some("mm".into()),
            preprocess: "image-v1-t280".into(),
            ..text_space()
        };
        e.fx.segment(ids[3], "image", &unit(9), &sp);
        let db = e.fx.db.lock().unwrap();
        for (id, media) in ids.iter().zip([
            MEDIA,
            r#"["docmedia-v1-p128-n100-r3-mp100-mb200"]"#,
            "[]",
            MEDIA,
        ]) {
            db.conn
                .execute(
                    "UPDATE files SET meta_json = json_object('media', json(?2)),
                       ext = substr(name, instr(name, '.') + 1) WHERE id = ?1",
                    params![id, media],
                )
                .unwrap();
        }
        drop(db);
        let w = Wanted {
            kinds: vec!["document"],
            doc_media: MEDIA.into(),
            ..wanted()
        };
        assert_eq!(e.requeue(&w), BTreeMap::from([("document".into(), 3)]));
        let parts = |name: &str| -> i64 {
            e.fx.db
                .lock()
                .unwrap()
                .conn
                .query_row(
                    "SELECT j.parts FROM jobs j JOIN files f ON f.id = j.file_id WHERE f.name = ?1",
                    [name],
                    |row| row.get(0),
                )
                .unwrap()
        };
        assert_eq!(
            [parts("正文旧.docx"), parts("媒体旧.pdf"), parts("都旧.pdf")],
            [1, 2, 3]
        );
        assert_eq!(e.pending(), ["媒体旧.pdf", "正文旧.docx", "都旧.pdf"]);
        // 关闭图片后标记少了一项，不放回（已有的内嵌片段保留）
        let off = Wanted {
            doc_media: r#"["docmedia-v1-p128-n100-r3-mp100-mb200"]"#.into(),
            ..w.clone()
        };
        assert!(e.requeue(&off).is_empty());
        // 打开音频：已完成的只算内嵌媒体，排队中的补上这一部分
        let audio = Wanted {
            doc_media: r#"["docmedia-v1-p128-n100-r3-mp100-mb200","audio-v1-s10-o2-q-60"]"#.into(),
            ..w
        };
        assert_eq!(e.requeue(&audio), BTreeMap::from([("document".into(), 1)]));
        assert_eq!(parts("都新.pdf"), 2);
        assert_eq!(parts("正文旧.docx"), 3);
    }

    /// 重建作业排在新文件之后（同类内先后不变）；统计时算作已完成
    #[test]
    fn rebuild_after_new_files() {
        let e = Env::new();
        e.file("旧.png", "image", Some((640, 480)), &["image-v1-t70"]);
        e.file("新.png", "image", Some((640, 480)), &["image-v1-t70"]);
        // 新加入的视频：档位最低的种类
        let video = e.fx.file(e.root, "新.mp4", "video", 0);
        {
            let db = e.fx.db.lock().unwrap();
            db.conn
                .execute(
                    "INSERT INTO jobs (file_id, stage, priority, updated_at) VALUES (?1, 'pending', ?2, 0)",
                    params![video, priority("video", 0)],
                )
                .unwrap();
            db.conn
                .execute(
                    "UPDATE jobs SET priority = ?1 WHERE file_id = (SELECT id FROM files WHERE name = '新.png')",
                    [priority("image", 2_000)],
                )
                .unwrap();
        }
        assert_eq!(e.requeue(&wanted()), BTreeMap::from([("image".into(), 2)]));
        // 再次放回不会把优先级再减一次
        e.fx.db
            .lock()
            .unwrap()
            .conn
            .execute(
                "UPDATE jobs SET stage = 'stored' WHERE file_id <> ?1",
                [video],
            )
            .unwrap();
        e.requeue(&wanted());
        let db = e.fx.db.lock().unwrap();
        let counts = db.job_counts().unwrap();
        assert_eq!((counts["image"].done, counts["image"].pending), (2, 0));
        assert_eq!(counts["video"].pending, 1);
        assert_eq!(
            db.rebuild_counts(true, true, true).unwrap(),
            BTreeMap::from([("image".into(), 2)])
        );
        assert!(db.rebuild_counts(false, true, true).unwrap().is_empty());
        assert!(db.has_unfinished_jobs().unwrap());
        assert_eq!(db.claimable_jobs(true, true, true).unwrap(), 3);
        // 新文件（即使是档位最低的视频）先领
        let order: Vec<String> = std::iter::from_fn(|| db.claim_job(true, true, true).unwrap())
            .map(|j| j.name)
            .collect();
        assert_eq!(order, ["新.mp4", "新.png", "旧.png"]);
    }

    /// 重建期间旧向量照常可搜（新旧档位分属不同空间，文本查询都能比较）；新结果写入时在一个事务里替换
    #[test]
    fn old_vectors_searchable_until_replaced() {
        let e = Env::new();
        let id = e.file("狗.png", "image", Some((640, 480)), &["image-v1-t140"]);
        let w = Wanted {
            image: "image-v1-t280".into(),
            ..wanted()
        };
        assert_eq!(e.requeue(&w), BTreeMap::from([("image".into(), 1)]));
        let params = SearchParams {
            candidates: 200,
            limit: 100,
            rrf_k: 60.0,
            weight_semantic: 1.0,
            weight_name: 1.0,
            weight_text: 1.0,
            video_windows: 5,
            video_gap_ms: 500,
            min_relevance: 2.0,
        };
        let query = text_space();
        let filter = Filter::default();
        let index = Mutex::new(VIndex::load(&e.fx.db.lock().unwrap()).unwrap());
        // (文件, 是否几乎同向)
        let find = |v: &[f32]| {
            let q = Query {
                vector: Some((v, &query)),
                coarse: v,
                kind: crate::QueryKind::Text,
                modality: None,
                text: None,
                filter: &filter,
                params,
                cancel: None,
            };
            search(&e.fx.db, &index, &q)
                .unwrap()
                .iter()
                .map(|h| (h.file_id, h.cosine.is_some_and(|c| c > 0.99)))
                .collect::<Vec<_>>()
        };
        assert_eq!(find(&unit(0)), [(id, true)]);

        let job =
            e.fx.db
                .lock()
                .unwrap()
                .claim_job(true, true, true)
                .unwrap()
                .unwrap();
        // 处理中仍可搜到
        assert_eq!(find(&unit(0)), [(id, true)]);
        let new = Space {
            id: "s-280".into(),
            preprocess: "image-v1-t280".into(),
            ..space("x", true)
        };
        let out = Outcome::Stored {
            kind: "image",
            quick_hash: vec![1],
            content_hash: Some(vec![1]),
            space: Box::new(new),
            segments: vec![NewSegment {
                modality: "image",
                locator_json: "{}".into(),
                text: None,
                vector: unit(5),
                coarse: unit(5),
                space: None,
            }],
            size: Some((640, 480)),
            meta_json: None,
            replace: None,
            more: false,
            media_failed: None,
        };
        e.fx.db.lock().unwrap().apply(&[(job, out)]).unwrap();
        // 语义检索总会返回最接近的，旧向量已不在：余弦不再接近 1
        assert_eq!(find(&unit(0)), [(id, false)]);
        assert_eq!(find(&unit(5)), [(id, true)]);
        let db = e.fx.db.lock().unwrap();
        let versions: Vec<String> = db
            .conn
            .prepare("SELECT params_version FROM vectors")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(versions, ["image-v1-t280"]);
        assert!(db.requeue_stale(&w).unwrap().is_empty());
        assert!(db.rebuild_counts(true, true, true).unwrap().is_empty());
    }
}
