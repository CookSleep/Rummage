//! 对账结果的写入：新增或变化的文件按批写入，同时建立、重置或删除作业；
//! 变成占位文件、超过大小上限、关闭该类的内容索引时如何处理向量与已完成的作业，见上级模块说明。
//! 数据库锁只在每批写入时持有（每批一个事务）；写入前在锁内确认未取消、根目录仍在（id 与路径都相同），否则按取消处理。
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{Connection, params};

use super::{Old, Rec, ScanError, lock, priority};
use crate::db::{Db, now_ms};

fn upsert_job(conn: &Connection, id: i64, rec: &Rec, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO jobs (file_id, stage, attempts, last_error, priority, updated_at)
         VALUES (?1, 'pending', 0, NULL, ?2, ?3)
         ON CONFLICT (file_id) DO UPDATE SET stage = 'pending', attempts = 0, last_error = NULL,
           priority = excluded.priority, updated_at = excluded.updated_at, parts = 3, media_failed = 0",
        params![id, priority(rec.kind, rec.mtime), now],
    )?;
    Ok(())
}

/// 写入一个新增或有变化的文件
fn write(
    conn: &Connection,
    root_id: i64,
    old: Option<Old>,
    rec: &Rec,
    now: i64,
) -> rusqlite::Result<()> {
    let Some(old) = old else {
        let id: i64 = conn.query_row(
            "INSERT INTO files (root_id, rel_path, name, ext, kind, size, mtime, cloud, status, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (root_id, rel_path) DO UPDATE SET size = excluded.size, mtime = excluded.mtime,
               quick_hash = NULL, content_hash = NULL, width = NULL, height = NULL, meta_json = '{}',
               cloud = excluded.cloud, status = excluded.status,
               updated_at = excluded.updated_at
             RETURNING id",
            params![
                root_id,
                rec.rel,
                rec.name,
                rec.ext,
                rec.kind,
                rec.size,
                rec.mtime,
                rec.cloud,
                rec.status(),
                now
            ],
            |row| row.get(0),
        )?;
        if rec.wants_job {
            upsert_job(conn, id, rec, now)?;
        }
        return Ok(());
    };
    if old.size != rec.size || old.mtime != rec.mtime {
        conn.execute(
            "UPDATE files SET size = ?2, mtime = ?3, quick_hash = NULL, content_hash = NULL, width = NULL,
               height = NULL, meta_json = '{}', cloud = ?4, status = ?5, updated_at = ?6
             WHERE id = ?1",
            params![old.id, rec.size, rec.mtime, rec.cloud, rec.status(), now],
        )?;
        if rec.wants_job {
            return upsert_job(conn, old.id, rec, now);
        }
        // 内容变了又不再索引：旧作业（含已完成的）和旧向量都不再代表文件内容，片段删除时级联删除向量
        conn.execute("DELETE FROM jobs WHERE file_id = ?1", [old.id])?;
        conn.execute("DELETE FROM segments WHERE file_id = ?1", [old.id])?;
        return Ok(());
    }
    if rec.cloud {
        // 被“释放空间”变回占位文件：保留向量和已完成的作业，只改 cloud 与 status
        conn.execute(
            "UPDATE files SET cloud = 1, status = 'cloud', updated_at = ?2 WHERE id = ?1",
            params![old.id, now],
        )?;
        conn.execute(
            "DELETE FROM jobs WHERE file_id = ?1 AND stage <> 'stored'",
            [old.id],
        )?;
        return Ok(());
    }
    // 内容未变：重新下载回本机，或修改大小上限后重新判定。已有向量的直接恢复，否则按当前上限处理
    let stored: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM jobs WHERE file_id = ?1 AND stage = 'stored')",
        [old.id],
        |row| row.get(0),
    )?;
    let status = if stored { "stored" } else { rec.status() };
    conn.execute(
        "UPDATE files SET cloud = 0, status = ?2, updated_at = ?3 WHERE id = ?1",
        params![old.id, status, now],
    )?;
    if stored {
        return Ok(());
    }
    if rec.wants_job {
        return upsert_job(conn, old.id, rec, now);
    }
    conn.execute(
        "DELETE FROM jobs WHERE file_id = ?1 AND stage <> 'stored'",
        [old.id],
    )?;
    Ok(())
}

/// 根目录是否仍是扫描开始时的那个（id 与路径都相同）：`RemoveRoot` 之后不能再写入，id 被重新使用时也不能写错
pub(super) fn root_exists(db: &Db, root_id: i64, root: &str) -> Result<bool, ScanError> {
    db.conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM roots WHERE id = ?1 AND path = ?2)",
            params![root_id, root],
            |row| row.get(0),
        )
        .map_err(|err| ScanError::Db(db.err(err)))
}

/// 在一个事务中写入一批；先在锁内检查取消和根目录是否仍在，移除根目录后不会再写入
pub(super) fn flush(
    db: &Mutex<Db>,
    root_id: i64,
    root: &str,
    batch: &mut Vec<(Option<Old>, Rec)>,
    cancel: &AtomicBool,
) -> Result<(), ScanError> {
    if batch.is_empty() {
        return Ok(());
    }
    let db = lock(db);
    if cancel.load(Ordering::Relaxed) || !root_exists(&db, root_id, root)? {
        return Err(ScanError::Cancelled);
    }
    let now = now_ms();
    let res = db.conn.unchecked_transaction().and_then(|tx| {
        for (old, rec) in batch.drain(..) {
            write(&tx, root_id, old, &rec, now)?;
        }
        tx.commit()
    });
    res.map_err(|err| ScanError::Db(db.err(err)))
}
