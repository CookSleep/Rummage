//! 缩略图的引用：缩略图按 `quick_hash` 命名，文件移出索引（删除、移除文件夹、被排除）或内容变化
//! （扫描把 `quick_hash` 清空）后，旧的哈希若不再被任何文件使用，它的缩略图就该删除。
//! 打开数据库时在这个连接上建临时触发器，把失去的哈希记进临时表 `thumb_orphans`；调用方用 `take_orphan_thumbs`
//! 取走其中已无文件引用的哈希，自行删除图片。清空索引（`clear_index_batch`）在同一事务中丢弃本批记下的哈希，
//! 缩略图留给重建复用。
use std::collections::HashSet;

use crate::db::{Db, DbError};

pub(crate) const TRACK: &str = "
CREATE TEMP TABLE IF NOT EXISTS thumb_orphans (hash BLOB NOT NULL);
CREATE TEMP TRIGGER IF NOT EXISTS thumb_files_delete AFTER DELETE ON files
WHEN old.quick_hash IS NOT NULL BEGIN
  INSERT INTO thumb_orphans VALUES (old.quick_hash);
END;
CREATE TEMP TRIGGER IF NOT EXISTS thumb_files_rehash AFTER UPDATE OF quick_hash ON files
WHEN old.quick_hash IS NOT NULL AND (new.quick_hash IS NULL OR new.quick_hash <> old.quick_hash) BEGIN
  INSERT INTO thumb_orphans VALUES (old.quick_hash);
END;
";

impl Db {
    /// 全部文件的 `quick_hash`（去重），全量清扫缩略图时据此判断哪些仍被引用
    pub fn quick_hashes(&self) -> Result<HashSet<Vec<u8>>, DbError> {
        self.conn
            .prepare("SELECT DISTINCT quick_hash FROM files WHERE quick_hash IS NOT NULL")
            .and_then(|mut stmt| stmt.query_map([], |row| row.get(0))?.collect())
            .map_err(|err| self.err(err))
    }

    /// 取走上次调用以来失去、且已没有任何文件引用的 `quick_hash`
    pub fn take_orphan_thumbs(&self) -> Result<Vec<Vec<u8>>, DbError> {
        let res = self.conn.unchecked_transaction().and_then(|tx| {
            let hashes = tx
                .prepare(
                    "SELECT DISTINCT hash FROM thumb_orphans WHERE hash NOT IN
                       (SELECT quick_hash FROM files WHERE quick_hash IS NOT NULL)",
                )?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            tx.execute("DELETE FROM thumb_orphans", [])?;
            tx.commit().map(|()| hashes)
        });
        res.map_err(|err| self.err(err))
    }

    /// 全量清扫前的检查：有未完成的作业、或有建好内容索引的文件还没算出 `quick_hash` 时为假，
    /// 这时它们现有的缩略图无法与文件对应，清扫会误删。失败的文件可能读不出内容、永远没有 `quick_hash`，不算在内
    pub fn thumbs_settled(&self) -> Result<bool, DbError> {
        self.conn
            .query_row(
                "SELECT NOT EXISTS (SELECT 1 FROM jobs j JOIN files f ON f.id = j.file_id
                   WHERE j.stage NOT IN ('stored', 'failed') OR (j.stage = 'stored' AND f.quick_hash IS NULL))",
                [],
                |row| row.get(0),
            )
            .map_err(|err| self.err(err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_unreferenced_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        db.conn
            .execute_batch(
                "INSERT INTO roots (id, path, settings_json, settings_version, added_at)
                 VALUES (1, 'D:\\资料', '{}', 1, 0);
                 INSERT INTO files (id, root_id, rel_path, name, ext, kind, size, mtime, quick_hash, status, updated_at)
                 VALUES (1, 1, 'a.jpg', 'a.jpg', 'jpg', 'image', 1, 0, x'01', 'stored', 0),
                        (2, 1, 'b.jpg', 'b.jpg', 'jpg', 'image', 1, 0, x'02', 'stored', 0),
                        (3, 1, 'c.jpg', 'c.jpg', 'jpg', 'image', 1, 0, x'02', 'stored', 0),
                        (4, 1, 'd.mp4', 'd.mp4', 'mp4', 'video', 1, 0, x'04', 'stored', 0);",
            )
            .unwrap();
        assert!(db.take_orphan_thumbs().unwrap().is_empty());
        // 独占的哈希随文件删除而失去；共用的还有 c.jpg 引用
        db.conn
            .execute("DELETE FROM files WHERE id IN (1, 2)", [])
            .unwrap();
        // 内容变化：扫描清空 quick_hash
        db.conn
            .execute("UPDATE files SET quick_hash = NULL WHERE id = 4", [])
            .unwrap();
        let mut got = db.take_orphan_thumbs().unwrap();
        got.sort();
        assert_eq!(got, [vec![0x01], vec![0x04]]);
        assert!(db.take_orphan_thumbs().unwrap().is_empty());
        assert_eq!(db.quick_hashes().unwrap(), HashSet::from([vec![0x02]]));

        // 清空索引时不记下，缩略图留给重建
        while db.clear_index_batch(1).unwrap() > 0 {}
        assert!(db.take_orphan_thumbs().unwrap().is_empty());
    }

    #[test]
    fn settled_only_without_pending_work() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        db.conn
            .execute_batch(
                "INSERT INTO roots (id, path, settings_json, settings_version, added_at)
                 VALUES (1, 'D:\\资料', '{}', 1, 0);
                 INSERT INTO files (id, root_id, rel_path, name, ext, kind, size, mtime, status, updated_at)
                 VALUES (1, 1, 'a.txt', 'a.txt', 'txt', 'document', 1, 0, 'pending', 0),
                        (2, 1, 'v.mp4', 'v.mp4', 'mp4', 'video', 1, 0, 'stored', 0);
                 INSERT INTO jobs (file_id, stage, priority, updated_at) VALUES (1, 'pending', 0, 0);",
            )
            .unwrap();
        assert!(!db.thumbs_settled().unwrap());
        db.conn
            .execute_batch("UPDATE jobs SET stage = 'stored'; UPDATE files SET quick_hash = x'01' WHERE id = 1;")
            .unwrap();
        // 不建内容索引的视频没有 quick_hash 也不影响
        assert!(db.thumbs_settled().unwrap());
        // 失败且没有 quick_hash 的文件不阻止清扫
        db.conn
            .execute_batch(
                "INSERT INTO files (id, root_id, rel_path, name, ext, kind, size, mtime, status, updated_at)
                 VALUES (3, 1, 'b.pdf', 'b.pdf', 'pdf', 'document', 1, 0, 'failed', 0);
                 INSERT INTO jobs (file_id, stage, priority, updated_at) VALUES (3, 'failed', 0, 0);",
            )
            .unwrap();
        assert!(db.thumbs_settled().unwrap());
    }
}
