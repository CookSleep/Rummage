//! 失败文件：界面“失败文件”列表的查询与重试。
//! - 列表为作业 `failed` 的文件，以及有内嵌媒体无法识别的已完成文档（`jobs.media_failed`；正文与其他媒体照常可搜），
//!   与 `job_counts` 的 `failed` 一致（同用 `jobs::FAILED`）；最近失败的在前。按游标分页：下一页从上一页最后一行的
//!   (失败时间, 文件 ID) 之后取，翻页期间有文件被重试或新失败，也不会跳过或重复已有的行。
//! - 加密文档（`ENCRYPTED`）也在列表中，但不能重试：重试也只会得到同样的结果，去掉密码、文件变化后扫描会自动重新排队。
//! - 重试：作业放回 `pending`、失败次数清零、清除原因，并把文件当前 `quick_hash` 移出黑名单，否则预处理会直接再判为失败。
//!   优先级不变，重建作业仍排在新文件之后。有内嵌媒体无法识别的文档只重算内嵌媒体（`parts`），失败个数清零，
//!   同样把 `quick_hash` 移出黑名单（这类文档本不会进黑名单，与失败的文件一致处理）。唤醒流水线由调用方负责。
//!   文件修改后扫描重新排队、失败个数清零，删除后作业级联删除，都会从列表中消失。
//! - 忽略：在文件所属根目录的排除规则中加入只匹配它的规则（`scan::exact_rule`），并删除文件记录（级联删除片段、向量
//!   与作业），之后扫描与监听都不再处理它。加密文档也能忽略。规则与删除在同一个事务中。
use std::collections::BTreeMap;
use std::path::{MAIN_SEPARATOR, PathBuf};

use rusqlite::params;

use crate::db::{Db, DbError, now_ms};
use crate::jobs::{ENCRYPTED, FAILED, PART_MEDIA};
use crate::scan::exact_rule;

/// 失败文件列表的一行
#[derive(Debug, Clone, PartialEq)]
pub struct FailedFile {
    pub file_id: i64,
    pub name: String,
    pub path: PathBuf,
    pub kind: String,
    /// 给用户看的中文整句
    pub reason: Option<String>,
    /// 失败的时间（Unix 毫秒），与 `file_id` 一起作为分页游标
    pub failed_at: i64,
    /// 能否重试：加密文档不能
    pub retryable: bool,
}

/// 失败文件的一页
#[derive(Debug, Clone, PartialEq)]
pub struct FailedPage {
    pub files: Vec<FailedFile>,
    /// 失败文件的总数
    pub total: u64,
    /// 其中能重试的
    pub retryable: u64,
}

impl Db {
    /// 失败的文件，最近失败的在前；`after` 为上一页最后一行的 (`failed_at`, `file_id`)，为空时从头取
    pub fn failed_files(
        &self,
        after: Option<(i64, i64)>,
        limit: u64,
    ) -> Result<FailedPage, DbError> {
        let (total, retryable): (i64, i64) = self
            .conn
            .query_row(
                &format!(
                    "SELECT COUNT(*), COUNT(*) FILTER (WHERE j.last_error IS NOT ?1) FROM jobs j WHERE {FAILED}"
                ),
                [ENCRYPTED],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|err| self.err(err))?;
        let files = self
            .conn
            .prepare(&format!(
                "SELECT f.id, f.name, r.path, f.rel_path, f.kind, j.last_error, j.updated_at, j.stage, j.media_failed
                 FROM jobs j JOIN files f ON f.id = j.file_id JOIN roots r ON r.id = f.root_id
                 WHERE {FAILED} AND (?1 IS NULL OR (j.updated_at, f.id) < (?1, ?2))
                 ORDER BY j.updated_at DESC, f.id DESC LIMIT ?3"
            ))
            .and_then(|mut stmt| {
                stmt.query_map(
                    params![
                        after.map(|a| a.0),
                        after.map(|a| a.1),
                        i64::try_from(limit).unwrap_or(i64::MAX)
                    ],
                    |row| {
                        let root: String = row.get(2)?;
                        let rel: String = row.get(3)?;
                        let stage: String = row.get(7)?;
                        let media_failed: i64 = row.get(8)?;
                        // 已完成、只有内嵌媒体无法识别的文档
                        let reason: Option<String> = if stage == "failed" {
                            row.get(5)?
                        } else {
                            Some(format!(
                                "文档中有 {media_failed} 个图片或音视频无法识别，正文可以正常搜索。"
                            ))
                        };
                        Ok(FailedFile {
                            file_id: row.get(0)?,
                            name: row.get(1)?,
                            path: PathBuf::from(root).join(rel),
                            kind: row.get(4)?,
                            retryable: reason.as_deref() != Some(ENCRYPTED),
                            reason,
                            failed_at: row.get(6)?,
                        })
                    },
                )?
                .collect()
            })
            .map_err(|err| self.err(err))?;
        Ok(FailedPage {
            files,
            total: total.max(0) as u64,
            retryable: retryable.max(0) as u64,
        })
    }

    /// 重试失败的文件：`file_id` 为空时重试全部；加密文档不重试。返回放回待处理的文件数
    pub fn retry_failed(&self, file_id: Option<i64>) -> Result<u64, DbError> {
        let res = self.conn.unchecked_transaction().and_then(|tx| {
            tx.execute(
                &format!(
                    "DELETE FROM blacklist WHERE quick_hash IN (
                       SELECT f.quick_hash FROM files f JOIN jobs j ON j.file_id = f.id
                       WHERE {FAILED} AND j.last_error IS NOT ?2 AND f.quick_hash IS NOT NULL
                         AND (?1 IS NULL OR f.id = ?1))"
                ),
                params![file_id, ENCRYPTED],
            )?;
            // 先放回只有内嵌媒体无法识别的文档：只重算内嵌媒体
            let partial = tx.execute(
                "UPDATE jobs SET stage = 'pending', attempts = 0, last_error = NULL, updated_at = ?2, parts = ?3,
                   media_failed = 0
                 WHERE stage = 'stored' AND media_failed > 0 AND (?1 IS NULL OR file_id = ?1)",
                params![file_id, now_ms(), PART_MEDIA],
            )?;
            let n = tx.execute(
                "UPDATE jobs SET stage = 'pending', attempts = 0, last_error = NULL, updated_at = ?3, parts = 3,
                   media_failed = 0
                 WHERE stage = 'failed' AND last_error IS NOT ?2 AND (?1 IS NULL OR file_id = ?1)",
                params![file_id, ENCRYPTED, now_ms()],
            )? + partial;
            tx.commit()?;
            Ok(n as u64)
        });
        res.map_err(|err| self.err(err))
    }

    /// 忽略失败的文件（见模块文档）；`file_id` 为空时忽略全部。返回忽略的文件数
    pub fn ignore_failed(&self, file_id: Option<i64>) -> Result<u64, DbError> {
        let res = self.conn.unchecked_transaction().and_then(|tx| {
            let rows: Vec<(i64, i64, String)> = tx
                .prepare(&format!(
                    "SELECT f.id, f.root_id, f.rel_path FROM jobs j JOIN files f ON f.id = j.file_id
                     WHERE {FAILED} AND (?1 IS NULL OR f.id = ?1)"
                ))?
                .query_map([file_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<Result<_, _>>()?;
            let mut rules: BTreeMap<i64, Vec<String>> = BTreeMap::new();
            for (id, root, rel) in &rows {
                let rel = rel.replace(MAIN_SEPARATOR, "/");
                rules.entry(*root).or_default().push(exact_rule(&rel));
                tx.execute("DELETE FROM files WHERE id = ?1", [id])?;
            }
            for (root, new) in rules {
                let settings: String = tx.query_row(
                    "SELECT settings_json FROM roots WHERE id = ?1",
                    [root],
                    |row| row.get(0),
                )?;
                // 设置损坏时从空对象开始，与 `set_root_excludes` 相同
                let mut value: serde_json::Value = serde_json::from_str(&settings)
                    .ok()
                    .filter(serde_json::Value::is_object)
                    .unwrap_or_else(|| serde_json::json!({}));
                let mut list: Vec<String> = value["exclude"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
                for rule in new {
                    if !list.contains(&rule) {
                        list.push(rule);
                    }
                }
                value["exclude"] = serde_json::json!(list);
                tx.execute(
                    "UPDATE roots SET settings_json = ?2 WHERE id = ?1",
                    params![root, value.to_string()],
                )?;
            }
            tx.commit()?;
            Ok(rows.len() as u64)
        });
        res.map_err(|err| self.err(err))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::jobs::MAX_ATTEMPTS;
    use crate::{Outcome, normalize_root, scan_root};

    use crate::scan::TEST_LIMITS as LIMITS;

    struct Env {
        db: Mutex<Db>,
        _root: tempfile::TempDir,
        _data: tempfile::TempDir,
    }

    fn env(files: &[&str]) -> Env {
        let data = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        for name in files {
            std::fs::write(root.path().join(name), name.as_bytes()).unwrap();
        }
        let db = Db::open(data.path()).unwrap();
        let id = db.add_root(&normalize_root(root.path()).unwrap()).unwrap();
        let db = Mutex::new(db);
        scan_root(&db, id, LIMITS, &AtomicBool::new(false), &mut |_| {}).unwrap();
        Env {
            db,
            _root: root,
            _data: data,
        }
    }

    impl Env {
        /// 把作业领取并失败 `MAX_ATTEMPTS` 次，进入黑名单；`hash` 为 quick_hash
        fn fail(&self, hash: u8, error: &str) {
            for _ in 0..MAX_ATTEMPTS {
                let db = self.db.lock().unwrap();
                let job = db.claim_job(true, true, true).unwrap().unwrap();
                let out = Outcome::Failed {
                    quick_hash: Some(vec![hash]),
                    error: error.into(),
                };
                db.apply(&[(job, out)]).unwrap();
            }
        }

        fn count(&self, sql: &str) -> i64 {
            let db = self.db.lock().unwrap();
            db.conn.query_row(sql, [], |row| row.get(0)).unwrap()
        }
    }

    #[test]
    fn list_and_retry() {
        let e = env(&["a.txt", "b.txt", "c.txt"]);
        // 同类文件按修改时间从新到旧领取，顺序不重要：按 quick_hash 区分
        e.fail(1, "无法读取此文件。");
        e.fail(2, "没有读取此文件的权限。");
        assert_eq!(e.count("SELECT COUNT(*) FROM blacklist"), 2);
        // 第三个是加密文档：在列表中，但不能重试
        let db = e.db.lock().unwrap();
        let job = db.claim_job(true, true, true).unwrap().unwrap();
        let enc = job.file_id;
        db.apply(&[(
            job,
            Outcome::Encrypted {
                quick_hash: vec![3],
            },
        )])
        .unwrap();

        let page = db.failed_files(None, 10).unwrap();
        let files = page.files.clone();
        assert_eq!((page.total, page.retryable, files.len()), (3, 2, 3));
        assert!(files.iter().all(|f| f.kind == "document"));
        assert!(
            files
                .iter()
                .all(|f| f.path.ends_with(&f.name) && f.path.is_file())
        );
        assert!(files.iter().all(|f| f.retryable == (f.file_id != enc)));
        let mut reasons: Vec<_> = files.iter().filter_map(|f| f.reason.clone()).collect();
        reasons.sort();
        assert_eq!(
            reasons,
            ["无法读取此文件。", ENCRYPTED, "没有读取此文件的权限。"]
        );
        // 游标分页：按 (失败时间, 文件 ID) 从新到旧接着取
        let cursor = |f: &FailedFile| Some((f.failed_at, f.file_id));
        let first = db.failed_files(None, 1).unwrap().files;
        assert_eq!(first, files[..1]);
        assert_eq!(
            db.failed_files(cursor(&files[0]), 1).unwrap().files,
            files[1..2]
        );
        assert_eq!(
            db.failed_files(cursor(&files[1]), 5).unwrap().files,
            files[2..]
        );
        assert!(
            db.failed_files(cursor(&files[2]), 5)
                .unwrap()
                .files
                .is_empty()
        );

        // 加密文档：单个与全部重试都不处理
        assert_eq!(db.retry_failed(Some(enc)).unwrap(), 0);

        // 重试一个：移出黑名单、清零、放回待处理；翻页期间重试，下一页照常从游标之后取，不漏行
        let one = files.iter().find(|f| f.retryable).unwrap().clone();
        let at = files.iter().position(|f| f.file_id == one.file_id).unwrap();
        assert_eq!(db.retry_failed(Some(one.file_id)).unwrap(), 1);
        assert_eq!(
            db.failed_files(cursor(&one), 10).unwrap().files,
            files[at + 1..]
        );
        let page = db.failed_files(None, 10).unwrap();
        assert_eq!((page.total, page.retryable), (2, 1));
        assert!(page.files.iter().all(|f| f.file_id != one.file_id));
        let job = |id: i64| -> (String, i64, Option<String>) {
            db.conn
                .query_row(
                    "SELECT stage, attempts, last_error FROM jobs WHERE file_id = ?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap()
        };
        assert_eq!(job(one.file_id), ("pending".into(), 0, None));
        let hash: Vec<u8> = db
            .conn
            .query_row(
                "SELECT quick_hash FROM files WHERE id = ?1",
                [one.file_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!db.is_blacklisted(&hash).unwrap());
        assert_eq!(db.job_counts().unwrap()["document"].failed, 2);
        // 重新排队后能再次领取
        let again = db.claim_job(true, true, true).unwrap().unwrap();
        assert_eq!(again.file_id, one.file_id);
        // 不是失败的文件：不处理
        assert_eq!(db.retry_failed(Some(one.file_id)).unwrap(), 0);

        // 全部重试：加密文档留在列表中
        assert_eq!(db.retry_failed(None).unwrap(), 1);
        let page = db.failed_files(None, 10).unwrap();
        assert_eq!((page.total, page.retryable), (1, 0));
        assert_eq!(page.files[0].file_id, enc);
        assert_eq!(job(enc), ("failed".into(), 0, Some(ENCRYPTED.into())));
        drop(db);
        assert_eq!(e.count("SELECT COUNT(*) FROM blacklist"), 0);
    }

    #[test]
    fn ignore() {
        let e = env(&["a.txt", "b[1].txt", "c.txt"]);
        e.fail(1, "无法读取此文件。");
        e.fail(2, "无法读取此文件。");
        let db = e.db.lock().unwrap();
        let files = db.failed_files(None, 10).unwrap().files;
        assert_eq!(files.len(), 2);
        // 不是失败的文件：不处理
        let ok: i64 = db
            .conn
            .query_row(
                "SELECT id FROM files WHERE id NOT IN (SELECT file_id FROM jobs WHERE stage = 'failed')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(db.ignore_failed(Some(ok)).unwrap(), 0);

        assert_eq!(db.ignore_failed(Some(files[0].file_id)).unwrap(), 1);
        assert_eq!(db.failed_files(None, 10).unwrap().total, 1);
        assert_eq!(db.ignore_failed(None).unwrap(), 1);
        assert_eq!(db.failed_files(None, 10).unwrap().total, 0);
        let mut rules = db.list_roots().unwrap()[0].excludes.clone();
        rules.sort();
        let mut want: Vec<String> = files.iter().map(|f| exact_rule(&f.name)).collect();
        want.sort();
        assert_eq!(rules, want);
        drop(db);
        assert_eq!(e.count("SELECT COUNT(*) FROM files"), 1);
        // 重新扫描也不再加入
        let id = e.db.lock().unwrap().list_roots().unwrap()[0].id;
        scan_root(&e.db, id, LIMITS, &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert_eq!(e.count("SELECT COUNT(*) FROM files"), 1);
    }
}
