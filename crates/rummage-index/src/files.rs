//! 按 ID 读取单个已索引文件与片段：打开、在资源管理器中显示、缩略图与预览都只接受
//! 索引中的文件 ID，由这里查出路径，不向界面开放任意路径。`index_info` 给详情栏提供该文件的索引状态。
use std::path::PathBuf;

use rusqlite::{OptionalExtension, params};

use crate::db::{Db, DbError};

#[derive(Debug, Clone, PartialEq)]
pub struct FileRow {
    pub id: i64,
    /// 根目录路径与相对路径拼成的完整路径
    pub path: PathBuf,
    pub name: String,
    /// 小写、不含点
    pub ext: String,
    pub kind: String,
    pub cloud: bool,
    /// 所在根目录不可访问
    pub offline: bool,
    /// 所在根目录不存在，它的磁盘或存储已连接
    pub missing: bool,
    pub quick_hash: Option<Vec<u8>>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub size: i64,
    /// Unix 毫秒
    pub mtime: i64,
    /// `files.status`：`pending`、`stored`、`cloud`、`too_large`
    pub status: String,
}

/// 一个片段：`locator_json` 的写法见 `rummage_proto::SegmentHit`，没有正文（图片、音视频）时 `text` 为空
#[derive(Debug, Clone, PartialEq)]
pub struct SegmentRow {
    pub id: i64,
    pub file_id: i64,
    pub locator_json: String,
    pub text: Option<String>,
}

/// 一个文件的作业
#[derive(Debug, Clone, PartialEq)]
pub struct JobRow {
    /// `pending`、`preprocessing`、`embedding`、`stored`、`failed`
    pub stage: String,
    pub attempts: i64,
    pub last_error: Option<String>,
}

/// 一个文件的索引情况：作业（没有作业表示不做语义索引）、片段数，以及向量所在空间的预处理版本与后端
#[derive(Debug, Clone, PartialEq)]
pub struct IndexInfo {
    pub job: Option<JobRow>,
    pub segments: i64,
    /// 最近一次写入片段的时间（Unix 毫秒），即建好索引的时间；没有片段时为空
    pub indexed_at: Option<i64>,
    /// (预处理版本, 后端)，按预处理版本排序
    pub spaces: Vec<(String, String)>,
}

impl Db {
    /// 生成缩略图时现算的 `quick_hash` 写回：只在还没有、且大小与修改时间仍是读取时的值时写入，
    /// 不建内容索引的文件（视频、音频等）的缩略图因此也有记录可查，清理时不会被当作旧图删除
    pub fn remember_quick_hash(
        &self,
        id: i64,
        size: i64,
        mtime: i64,
        quick_hash: &[u8],
    ) -> Result<(), DbError> {
        self.conn
            .execute(
                "UPDATE files SET quick_hash = ?4
                 WHERE id = ?1 AND size = ?2 AND mtime = ?3 AND quick_hash IS NULL",
                params![id, size, mtime, quick_hash],
            )
            .map_err(|err| self.err(err))?;
        Ok(())
    }

    /// 文件不存在（已删除、根目录已移除）时为空
    pub fn file(&self, id: i64) -> Result<Option<FileRow>, DbError> {
        self.conn
            .query_row(
                "SELECT r.path, f.rel_path, f.name, f.ext, f.kind, f.cloud, r.offline, f.quick_hash,
                   f.width, f.height, f.size, f.mtime, f.status, r.offline = 2
                 FROM files f JOIN roots r ON r.id = f.root_id WHERE f.id = ?1",
                [id],
                |row| {
                    let root: String = row.get(0)?;
                    let rel: String = row.get(1)?;
                    // 库中的宽高不合法时当作未记录
                    let dim = |i| {
                        row.get::<_, Option<i64>>(i)
                            .map(|v| v.and_then(|v| u32::try_from(v).ok()))
                    };
                    Ok(FileRow {
                        id,
                        path: PathBuf::from(root).join(rel),
                        name: row.get(2)?,
                        ext: row.get(3)?,
                        kind: row.get(4)?,
                        cloud: row.get(5)?,
                        offline: row.get(6)?,
                        missing: row.get(13)?,
                        quick_hash: row.get(7)?,
                        width: dim(8)?,
                        height: dim(9)?,
                        size: row.get(10)?,
                        mtime: row.get(11)?,
                        status: row.get(12)?,
                    })
                },
            )
            .optional()
            .map_err(|err| self.err(err))
    }

    /// 文件的索引情况；文件不存在时作业为空、片段数为 0
    pub fn index_info(&self, id: i64) -> Result<IndexInfo, DbError> {
        let conn = &self.conn;
        let job = conn
            .query_row(
                "SELECT stage, attempts, last_error FROM jobs WHERE file_id = ?1",
                [id],
                |row| {
                    Ok(JobRow {
                        stage: row.get(0)?,
                        attempts: row.get(1)?,
                        last_error: row.get(2)?,
                    })
                },
            )
            .optional();
        let segments = conn.query_row(
            "SELECT COUNT(*), MAX(created_at) FROM segments WHERE file_id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        );
        let spaces = conn
            .prepare(
                "SELECT DISTINCT v.params_version, sp.backend FROM segments s
                 JOIN vectors v ON v.segment_id = s.id JOIN spaces sp ON sp.id = v.space_id
                 WHERE s.file_id = ?1 ORDER BY v.params_version, sp.backend",
            )
            .and_then(|mut stmt| {
                stmt.query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect()
            });
        let map = |err| self.err(err);
        let (segments, indexed_at) = segments.map_err(map)?;
        Ok(IndexInfo {
            job: job.map_err(map)?,
            segments,
            indexed_at,
            spaces: spaces.map_err(map)?,
        })
    }

    /// 片段不存在时为空
    pub fn segment(&self, id: i64) -> Result<Option<SegmentRow>, DbError> {
        self.conn
            .query_row(
                "SELECT id, file_id, locator_json, text FROM segments WHERE id = ?1",
                [id],
                segment_row,
            )
            .optional()
            .map_err(|err| self.err(err))
    }

    /// 文档预览的前后文：字符范围为 `[start, end)` 的片段之前、之后各一片——前一片是起点在它之前的最后一片，
    /// 后一片是起点与终点都在它之后的第一片。只查这两片，大文档不必把全部正文读出来
    pub fn text_neighbors(
        &self,
        file_id: i64,
        start: usize,
        end: usize,
    ) -> Result<Vec<SegmentRow>, DbError> {
        let to_i64 = |v: usize| i64::try_from(v).unwrap_or(i64::MAX);
        self.conn
            .prepare(
                "SELECT * FROM (SELECT id, file_id, locator_json, text FROM segments
                   WHERE file_id = ?1 AND text IS NOT NULL
                     AND json_extract(locator_json, '$.char_start') < ?2
                   ORDER BY json_extract(locator_json, '$.char_start') DESC LIMIT 1)
                 UNION ALL
                 SELECT * FROM (SELECT id, file_id, locator_json, text FROM segments
                   WHERE file_id = ?1 AND text IS NOT NULL
                     AND json_extract(locator_json, '$.char_start') > ?2
                     AND json_extract(locator_json, '$.char_end') > ?3
                   ORDER BY json_extract(locator_json, '$.char_start') LIMIT 1)",
            )
            .and_then(|mut stmt| {
                stmt.query_map(
                    rusqlite::params![file_id, to_i64(start), to_i64(end)],
                    segment_row,
                )?
                .collect()
            })
            .map_err(|err| self.err(err))
    }
}

fn segment_row(row: &rusqlite::Row) -> rusqlite::Result<SegmentRow> {
    Ok(SegmentRow {
        id: row.get(0)?,
        file_id: row.get(1)?,
        locator_json: row.get(2)?,
        text: row.get(3)?,
    })
}

#[cfg(test)]
mod tests {
    use super::JobRow;
    use crate::search::tests::{Fixture, text_space, unit};

    #[test]
    fn file_and_segment_lookup() {
        let f = Fixture::new();
        let r = f.root("D:\\资料");
        let id = f.file(r, "猫.jpg", "image", 0);
        let seg = f.segment(id, "text", &unit(0), &text_space());
        let db = f.db.lock().unwrap();
        db.conn
            .execute(
                "UPDATE files SET width = 640, height = 480, quick_hash = x'0102' WHERE id = ?1",
                [id],
            )
            .unwrap();
        let row = db.file(id).unwrap().unwrap();
        assert_eq!(row.path, std::path::Path::new("D:\\资料").join("猫.jpg"));
        assert_eq!((row.width, row.height), (Some(640), Some(480)));
        assert_eq!(row.quick_hash.as_deref(), Some(&[1u8, 2][..]));
        assert_eq!(row.kind, "image");
        assert!(!row.cloud && !row.offline);
        assert_eq!(db.file(id + 100).unwrap(), None);

        let row = db.segment(seg).unwrap().unwrap();
        assert_eq!((row.file_id, row.text), (id, None));
        assert!(db.text_neighbors(id, 0, 1).unwrap().is_empty());
        db.conn
            .execute(
                r#"UPDATE segments SET text = '正文', locator_json = '{"char_start":2,"char_end":4}' WHERE id = ?1"#,
                [seg],
            )
            .unwrap();
        assert_eq!(
            db.segment(seg).unwrap().unwrap().text.as_deref(),
            Some("正文")
        );
        // 这一片在 [0, 1) 之后、在 [5, 9) 之前，不算 [2, 4) 自己的前后文
        assert_eq!(db.text_neighbors(id, 0, 1).unwrap()[0].id, seg);
        assert_eq!(db.text_neighbors(id, 5, 9).unwrap()[0].id, seg);
        assert!(db.text_neighbors(id, 2, 4).unwrap().is_empty());
        assert_eq!(db.segment(seg + 100).unwrap(), None);
    }

    #[test]
    fn index_info_and_details() {
        let f = Fixture::new();
        let r = f.root("D:\\资料");
        let id = f.file(r, "a.md", "document", 1_700_000_000_000);
        f.segment(id, "text", &unit(0), &text_space());
        f.segment(id, "text", &unit(1), &text_space());
        let db = f.db.lock().unwrap();
        let row = db.file(id).unwrap().unwrap();
        assert_eq!((row.size, row.mtime), (1, 1_700_000_000_000));
        assert_eq!(row.status, "stored");

        // 还没有作业
        let info = db.index_info(id).unwrap();
        assert_eq!(info.job, None);
        assert_eq!(info.segments, 2);
        // 测试夹具写入的片段时间为 0
        assert_eq!(info.indexed_at, Some(0));
        let t = text_space();
        assert_eq!(info.spaces, [(t.preprocess.clone(), t.backend.clone())]);

        db.conn
            .execute(
                "INSERT INTO jobs (file_id, stage, attempts, last_error, priority, updated_at)
                 VALUES (?1, 'failed', 3, '坏了。', 0, 0)",
                [id],
            )
            .unwrap();
        let info = db.index_info(id).unwrap();
        assert_eq!(
            info.job,
            Some(JobRow {
                stage: "failed".into(),
                attempts: 3,
                last_error: Some("坏了。".into()),
            })
        );
        let none = db.index_info(id + 100).unwrap();
        assert_eq!((none.job, none.segments, none.spaces.len()), (None, 0, 0));
        assert_eq!(none.indexed_at, None);
    }
}
