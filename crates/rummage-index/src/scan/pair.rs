//! 改名与移动的配对：遍历（`super::walk`）之后，把消失的、有向量的记录与新出现的文件配对，
//! 配对成功的只改原记录的位置，不重算向量；然后写入配对不上的新文件、删除配对不上的消失者。
//! 打开候选文件取文件 ID 或算 `quick_hash` 都经过 `hash`，其中先检查占位文件。
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::params;

use super::{BATCH, Old, Rec, ScanError, Walk, flush, lock, root_exists};
use crate::db::{Db, now_ms};
use crate::hash;

/// 消失、有向量、等待配对的记录
struct Gone {
    id: i64,
    size: i64,
    mtime: i64,
    ext: String,
    volume_file_id: Option<String>,
    quick_hash: Option<Vec<u8>>,
}

/// 配对改名与移动，然后插入配对不上的新文件、删除配对不上的消失者。
/// 候选新文件须与消失者大小相同、扩展名相同（扩展名变了，提取方式可能不同，按删除 + 新增重算）；消失者有文件 ID 时先比 ID（修改时间也要相同，否则可能是
/// 移动后又改了内容），对不上或没有 ID 时比 `quick_hash`。取 ID、算哈希都要打开候选文件，`hash` 中先检查占位文件。
/// 配对成功时更新原记录的位置、大小、修改时间与文件 ID，片段、向量、作业、缩略图都保留；正在处理的作业放回 `pending`，
/// 按旧路径算出的结果写入时被丢弃（`jobs` 模块说明）
pub(super) fn settle(
    db: &Mutex<Db>,
    walks: &mut [Walk],
    cancel: &AtomicBool,
) -> Result<(), ScanError> {
    let ids: Vec<i64> = walks
        .iter()
        .flat_map(|w| w.gone.iter().filter(|g| g.1).map(|g| g.0))
        .collect();
    // (消失者 id, 新文件所在的 Walk, 新文件的下标, 新文件的 ID)
    let mut moves: Vec<(i64, usize, usize, Option<String>)> = Vec::new();
    if !ids.is_empty() && walks.iter().any(|w| !w.deferred.is_empty()) {
        let gone: Vec<Gone> = {
            let db = lock(db);
            // 遍历之后根目录可能已被移除（记录随之删除）或索引已被清空
            if cancel.load(Ordering::Relaxed) {
                return Err(ScanError::Cancelled);
            }
            for w in walks.iter() {
                if !root_exists(&db, w.root_id, &w.root)? {
                    return Err(ScanError::Cancelled);
                }
            }
            let res: rusqlite::Result<Vec<Gone>> = ids
                .iter()
                .map(|id| {
                    db.conn.query_row(
                        "SELECT size, mtime, ext, volume_file_id, quick_hash FROM files WHERE id = ?1",
                        [id],
                        |row| {
                            Ok(Gone {
                                id: *id,
                                size: row.get(0)?,
                                mtime: row.get(1)?,
                                ext: row.get(2)?,
                                volume_file_id: row.get(3)?,
                                quick_hash: row.get(4)?,
                            })
                        },
                    )
                })
                .collect();
            res.map_err(|err| db.err(err))?
        };
        let path = |w: &Walk, i: usize| Path::new(&w.root).join(&w.deferred[i].rel);
        let mut file_ids: HashMap<(usize, usize), Option<String>> = HashMap::new();
        let mut hashes: HashMap<(usize, usize), Option<[u8; 32]>> = HashMap::new();
        let mut taken: HashSet<(usize, usize)> = HashSet::new();
        for g in gone {
            if cancel.load(Ordering::Relaxed) {
                return Err(ScanError::Cancelled);
            }
            let cands: Vec<(usize, usize)> = walks
                .iter()
                .enumerate()
                .flat_map(|(wi, w)| {
                    w.deferred
                        .iter()
                        .enumerate()
                        .filter(|(_, r)| r.size == g.size && r.ext == g.ext)
                        .map(move |(i, _)| (wi, i))
                })
                .filter(|c| !taken.contains(c))
                .collect();
            let mut hit = None;
            if g.volume_file_id.is_some() {
                for &c in &cands {
                    let id = file_ids.entry(c).or_insert_with(|| {
                        hash::file_id(&path(&walks[c.0], c.1)).unwrap_or_else(|err| {
                            log::warn!("无法读取文件 ID：{err}");
                            None
                        })
                    });
                    if *id == g.volume_file_id && walks[c.0].deferred[c.1].mtime == g.mtime {
                        hit = Some(c);
                        break;
                    }
                }
            }
            if hit.is_none()
                && let Some(qh) = &g.quick_hash
            {
                for &c in &cands {
                    let h = hashes.entry(c).or_insert_with(|| {
                        let res = hash::quick_hash_with_id(&path(&walks[c.0], c.1));
                        match res {
                            Ok((h, id)) => {
                                file_ids.insert(c, id);
                                Some(h)
                            }
                            Err(err) => {
                                log::warn!("无法计算 quick_hash：{err}");
                                None
                            }
                        }
                    });
                    if h.is_some_and(|h| h[..] == qh[..]) {
                        hit = Some(c);
                        break;
                    }
                }
            }
            if let Some(c) = hit {
                taken.insert(c);
                moves.push((g.id, c.0, c.1, file_ids.get(&c).cloned().flatten()));
            }
        }
    }

    if !moves.is_empty() {
        let db = lock(db);
        if cancel.load(Ordering::Relaxed) {
            return Err(ScanError::Cancelled);
        }
        for w in walks.iter() {
            if !root_exists(&db, w.root_id, &w.root)? {
                return Err(ScanError::Cancelled);
            }
        }
        let now = now_ms();
        let res = db.conn.unchecked_transaction().and_then(|tx| {
            for (id, wi, i, file_id) in &moves {
                let w = &walks[*wi];
                let rec = &w.deferred[*i];
                tx.execute(
                    "UPDATE files SET root_id = ?2, rel_path = ?3, name = ?4, ext = ?5, size = ?6, mtime = ?7,
                       volume_file_id = ?8, updated_at = ?9
                     WHERE id = ?1",
                    params![
                        id, w.root_id, rec.rel, rec.name, rec.ext, rec.size, rec.mtime, file_id, now
                    ],
                )?;
                tx.execute(
                    "UPDATE jobs SET stage = 'pending', updated_at = ?2
                     WHERE file_id = ?1 AND stage IN ('preprocessing', 'embedding')",
                    params![id, now],
                )?;
            }
            tx.commit()
        });
        res.map_err(|err| ScanError::Db(db.err(err)))?;
    }
    for (id, wi, _, _) in &moves {
        walks[*wi].stats.added -= 1;
        walks[*wi].stats.updated += 1;
        for w in walks.iter_mut() {
            w.gone.retain(|g| g.0 != *id);
        }
    }

    for (wi, w) in walks.iter_mut().enumerate() {
        let mut batch: Vec<(Option<Old>, Rec)> = Vec::new();
        for (i, rec) in std::mem::take(&mut w.deferred).into_iter().enumerate() {
            if moves.iter().any(|m| m.1 == wi && m.2 == i) {
                continue;
            }
            batch.push((None, rec));
            if batch.len() >= BATCH {
                flush(db, w.root_id, &w.root, &mut batch, cancel)?;
            }
        }
        flush(db, w.root_id, &w.root, &mut batch, cancel)?;

        for chunk in w.gone.chunks(BATCH) {
            let db = lock(db);
            if cancel.load(Ordering::Relaxed) || !root_exists(&db, w.root_id, &w.root)? {
                return Err(ScanError::Cancelled);
            }
            let res = db.conn.unchecked_transaction().and_then(|tx| {
                for (id, _) in chunk {
                    tx.execute("DELETE FROM files WHERE id = ?1", [id])?;
                }
                tx.commit()
            });
            res.map_err(|err| ScanError::Db(db.err(err)))?;
            w.stats.removed += chunk.len() as u64;
        }
    }
    Ok(())
}
