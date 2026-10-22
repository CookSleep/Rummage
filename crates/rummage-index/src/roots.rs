//! 被索引的根目录。
//! - 添加分两步：`normalize_root` 读属性（必须是文件夹、不能是按需下载的网盘文件夹）、规范化路径（存库时去掉
//!   `\\?\` 前缀）并做卷检查，会访问磁盘，调用方在数据库锁之外执行；`Db::add_root` 再检查不在数据目录之内、
//!   不与已有根目录互相包含，然后写入。每一步的拒绝原因都是给用户看的中文整句。
//! - 根目录包含数据目录是允许的，扫描时跳过数据目录。
//! - 根目录设置 `settings_json` 当前为 `{"exclude": [glob, …], "skip_nomedia": bool}`（用户排除规则；是否跳过含 `.nomedia`
//!   的子文件夹，没有这一项时为真），`settings_version` 为 1。
//!   `set_root_excludes` 保存前逐条检查写法（与扫描时的编译方式相同），去掉空行与重复项，作路径分隔符的 `\` 换成 `/`
//!   （Windows 用户习惯这样写路径；`\` 后跟通配符等特殊字符时是转义，原样保留，见 `scan::exclude`）；完整路径（盘符或 `\\`
//!   开头）中的 `\` 一律是分隔符，本文件夹之下的去掉前缀后按相对规则处理，其他位置的报错；最后去掉末尾的 `/`，开头的 `/` 只在规则不再含 `/` 时保留（表示只匹配根目录下的路径）。
//!   只改 `exclude` 与给出的 `skip_nomedia`，保留设置中的其他键。
//! - 移除时级联删除其下全部文件、片段、向量和作业；`clear_index_batch` 分批对全部根目录做同样的删除，但保留根目录本身。
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};

use crate::db::{Db, DbError, now_ms};
use crate::fsx::{self, FsError};
use crate::platform;
use crate::scan::{exclude_glob, slashes, wants_job};

#[derive(Debug, thiserror::Error)]
pub enum RootError {
    #[error(transparent)]
    Fs(#[from] FsError),
    #[error("这个路径不是文件夹。")]
    NotDir,
    #[error("该文件夹是网盘的按需下载文件夹，不支持索引。")]
    CloudFolder,
    #[error("该文件夹位于 Rummage 的数据目录之内，不能添加。")]
    InDataDir,
    #[error("该文件夹已包含在已添加的 \"{0}\" 中。")]
    Inside(String),
    #[error("该文件夹包含已添加的 \"{0}\"，请先移除它。")]
    Contains(String),
    #[error("找不到该文件夹，它可能已被移除。")]
    NotFound,
    #[error("排除规则 \"{0}\" 的写法不正确，请检查方括号、花括号是否成对。")]
    BadExclude(String),
    #[error("排除规则 \"{0}\" 是这个文件夹以外的路径，请写成这个文件夹中的相对路径。")]
    AbsoluteExclude(String),
    #[error(transparent)]
    Db(#[from] DbError),
}

/// `roots.offline`：所在的磁盘或存储未连接
pub const OFFLINE_DEVICE: i64 = 1;
/// `roots.offline`：磁盘或存储已连接，文件夹不存在（被删除、改名或移走）
pub const OFFLINE_MISSING: i64 = 2;

#[derive(Debug, Clone, PartialEq)]
pub struct Root {
    pub id: i64,
    pub path: String,
    /// 不可访问（`missing` 或设备未连接）
    pub offline: bool,
    /// 文件夹不存在，所在的磁盘或存储已连接
    pub missing: bool,
    /// 已记录的文件数
    pub files: i64,
    /// 用户排除规则；设置不是合法 JSON 时为空（扫描同样忽略）
    pub excludes: Vec<String>,
    /// 跳过含 `.nomedia` 的子文件夹（只在 Android 上起作用）
    pub skip_nomedia: bool,
}

/// 设置中的排除规则；格式不对时为空
fn excludes_of(settings: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(settings)
        .ok()
        .and_then(|v| {
            v["exclude"].as_array().map(|a| {
                a.iter()
                    .filter_map(|r| r.as_str().map(str::to_string))
                    .collect()
            })
        })
        .unwrap_or_default()
}

/// 设置中的 `skip_nomedia`；没有这一项或格式不对时为真
pub(crate) fn skip_nomedia_of(settings: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(settings)
        .ok()
        .and_then(|v| v["skip_nomedia"].as_bool())
        .unwrap_or(true)
}

/// 忽略大小写比较用的路径
fn folded(path: &str) -> PathBuf {
    PathBuf::from(path.to_lowercase())
}

fn check_attrs(attrs: u32) -> Result<(), RootError> {
    if attrs & fsx::FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Err(RootError::NotDir);
    }
    // 扫描不进入这种目录，添加了也是空的
    if attrs & fsx::FILE_ATTRIBUTE_RECALL_ON_OPEN != 0 {
        return Err(RootError::CloudFolder);
    }
    Ok(())
}

/// 添加根目录的第一步，不需要数据库；返回存库用的路径，交给 `Db::add_root`
pub fn normalize_root(path: &Path) -> Result<String, RootError> {
    // 先读属性确认是文件夹：规范化要打开路径，选中的若是占位文件会触发下载
    let attrs = platform::file_attributes(path).map_err(|err| {
        log::warn!("无法读取 {} 的属性：{err}", path.display());
        FsError::Inaccessible(fsx::shown(path))
    })?;
    check_attrs(attrs)?;
    let real = path.canonicalize().map_err(|err| {
        log::warn!("无法规范化 {}：{err}", path.display());
        FsError::Inaccessible(fsx::shown(path))
    })?;
    let shown = fsx::shown(&real);
    fsx::check_volume(Path::new(&shown))?;
    Ok(shown)
}

impl Db {
    /// `root` 为 `normalize_root` 的结果；返回新根目录的 id
    pub fn add_root(&self, root: &str) -> Result<i64, RootError> {
        let new = folded(root);
        let data = self
            .dir
            .canonicalize()
            .map_or_else(|_| fsx::shown(&self.dir), |d| fsx::shown(&d));
        if new.starts_with(folded(&data)) {
            return Err(RootError::InDataDir);
        }
        for root in self.list_roots()? {
            let old = folded(&root.path);
            if new.starts_with(&old) {
                return Err(RootError::Inside(root.path));
            }
            if old.starts_with(&new) {
                return Err(RootError::Contains(root.path));
            }
        }
        self.conn
            .execute(
                "INSERT INTO roots (path, settings_json, settings_version, offline, added_at)
                 VALUES (?1, '{\"exclude\": []}', 1, 0, ?2)",
                params![root, now_ms()],
            )
            .map_err(|err| self.err(err))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 保存根目录的用户排除规则（见模块文档），`skip_nomedia` 为空时不改；返回整理后保存的规则。调用方随后重新扫描该根目录
    pub fn set_root_excludes(
        &self,
        id: i64,
        rules: &[String],
        skip_nomedia: Option<bool>,
    ) -> Result<Vec<String>, RootError> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT path, settings_json FROM roots WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|err| self.err(err))?;
        let Some((root, settings)) = row else {
            return Err(RootError::NotFound);
        };
        let root = root.replace('\\', "/").trim_end_matches('/').to_lowercase();
        let mut clean: Vec<String> = Vec::new();
        for line in rules {
            let line = line.trim();
            // 盘符或 `\\` 开头的完整路径中 `\` 都是路径分隔符，不当转义；去掉本文件夹的前缀后按相对规则处理
            let full = line.get(1..2) == Some(":") || line.starts_with(r"\\");
            let mut rule = if full {
                line.replace('\\', "/")
            } else {
                slashes(line)
            };
            let lower = rule.to_lowercase();
            if let Some(rest) = lower.strip_prefix(&root)
                && (rest.is_empty() || rest.starts_with('/'))
            {
                // 小写不改变这些路径的字节长度时才能按长度切；否则保守地当作其他位置
                if lower.len() == rule.len()
                    && let Some(r) = rule.get(root.len()..)
                {
                    rule = r.to_string();
                }
            }
            if rule.get(1..2) == Some(":") || rule.starts_with("//") {
                return Err(RootError::AbsoluteExclude(line.to_string()));
            }
            // 末尾的 / 去掉；开头的 / 表示只匹配根目录下的路径（`scan::exclude`），本来含 / 的规则已经如此，去掉
            let rest = rule.trim_matches('/');
            let rule = if rule.starts_with('/') && !rest.is_empty() && !rest.contains('/') {
                format!("/{rest}")
            } else {
                rest.to_string()
            };
            if rule.is_empty() || clean.contains(&rule) {
                continue;
            }
            if exclude_glob(&rule).is_err() {
                return Err(RootError::BadExclude(line.to_string()));
            }
            clean.push(rule);
        }
        // 旧设置不是对象（损坏）时从空对象开始
        let mut value: serde_json::Value = serde_json::from_str(&settings)
            .ok()
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        value["exclude"] = serde_json::json!(clean);
        if let Some(skip) = skip_nomedia {
            value["skip_nomedia"] = serde_json::json!(skip);
        }
        self.conn
            .execute(
                "UPDATE roots SET settings_json = ?2 WHERE id = ?1",
                params![id, value.to_string()],
            )
            .map_err(|err| self.err(err))?;
        Ok(clean)
    }

    pub fn remove_root(&self, id: i64) -> Result<(), RootError> {
        let n = self
            .conn
            .execute("DELETE FROM roots WHERE id = ?1", [id])
            .map_err(|err| self.err(err))?;
        if n == 0 {
            return Err(RootError::NotFound);
        }
        Ok(())
    }

    /// 清空索引的一步：删除最多 `limit` 个文件（级联删除片段、向量和作业），根目录、排除规则和设置保留；
    /// 已没有文件可删时清除失败黑名单。返回删除的文件数，为 0 表示清空完成。调用方在两步之间释放数据库锁，
    /// 不让清空大索引时长时间挡住搜索。
    /// 不做 VACUUM：它要在锁内重写整个数据库并临时占用同样大小的磁盘空间，而空出的页面在随后重建时会被重新使用
    pub fn clear_index_batch(&self, limit: usize) -> Result<usize, DbError> {
        let res = self.conn.unchecked_transaction().and_then(|tx| {
            let before: i64 = tx.query_row(
                "SELECT COALESCE(MAX(rowid), 0) FROM thumb_orphans",
                [],
                |row| row.get(0),
            )?;
            let n = tx.execute(
                "DELETE FROM files WHERE id IN (SELECT id FROM files LIMIT ?1)",
                [limit as i64],
            )?;
            // 缩略图留给重建复用，不当作失去引用（`thumbs`）
            tx.execute("DELETE FROM thumb_orphans WHERE rowid > ?1", [before])?;
            if n == 0 {
                tx.execute("DELETE FROM blacklist", [])?;
            }
            tx.commit().map(|()| n)
        });
        res.map_err(|err| self.err(err))
    }

    /// 每个根目录随机取最多 `limit` 张图片（条件同 `kind_totals`），返回 `(根目录 id, 路径)`，供引导抽样估计长图的切片数
    pub fn sample_images(&self, limit: u32) -> Result<Vec<(i64, PathBuf)>, DbError> {
        self.conn
            .prepare(
                "SELECT root_id, path, rel_path, ext FROM (
                   SELECT f.root_id, r.path, f.rel_path, f.ext,
                     ROW_NUMBER() OVER (PARTITION BY f.root_id ORDER BY RANDOM()) AS n
                   FROM files f JOIN roots r ON r.id = f.root_id
                   WHERE f.kind = 'image' AND f.cloud = 0 AND f.status <> 'too_large')
                 WHERE n <= ?1",
            )
            .and_then(|mut stmt| {
                stmt.query_map([limit], |row| {
                    let root: String = row.get(1)?;
                    let rel: String = row.get(2)?;
                    Ok((row.get(0)?, PathBuf::from(root).join(rel), row.get(3)?))
                })?
                .collect::<Result<Vec<(i64, PathBuf, String)>, _>>()
            })
            .map(|rows| {
                rows.into_iter()
                    .filter(|r| wants_job("image", &r.2))
                    .map(|r| (r.0, r.1))
                    .collect()
            })
            .map_err(|err| self.err(err))
    }

    /// 各根目录中能识别内容的文件（`wants_job`；不含占位文件与超过大小上限的）按种类的数量与总大小，
    /// 不看引导中开启了哪些类型。返回 `(根目录 id, 种类, 文件数, 字节数)`，供引导预估索引时间与空间
    pub fn kind_totals(&self) -> Result<Vec<(i64, String, u64, u64)>, DbError> {
        let rows: Vec<(i64, String, String, i64, i64)> = self
            .conn
            .prepare(
                "SELECT root_id, kind, ext, COUNT(*), SUM(size) FROM files
                 WHERE cloud = 0 AND status <> 'too_large' GROUP BY root_id, kind, ext",
            )
            .and_then(|mut stmt| {
                stmt.query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })?
                .collect()
            })
            .map_err(|err| self.err(err))?;
        let mut out: Vec<(i64, String, u64, u64)> = Vec::new();
        for (root, kind, ext, n, bytes) in rows {
            if !wants_job(&kind, &ext) {
                continue;
            }
            match out.iter_mut().find(|r| r.0 == root && r.1 == kind) {
                Some(r) => {
                    r.2 += n as u64;
                    r.3 += bytes.max(0) as u64;
                }
                None => out.push((root, kind, n as u64, bytes.max(0) as u64)),
            }
        }
        Ok(out)
    }

    /// 各根目录的 (id, 路径, `roots.offline`)：监听与离线检查用，不统计文件数
    pub fn root_states(&self) -> Result<Vec<(i64, String, i64)>, DbError> {
        self.conn
            .prepare("SELECT id, path, offline FROM roots ORDER BY id")
            .and_then(|mut stmt| {
                stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                    .collect()
            })
            .map_err(|err| self.err(err))
    }

    /// 记下根目录不可访问的原因（`OFFLINE_DEVICE`、`OFFLINE_MISSING`），可以访问时为 0
    pub fn set_root_offline(&self, id: i64, state: i64) -> Result<(), DbError> {
        self.conn
            .execute(
                "UPDATE roots SET offline = ?2 WHERE id = ?1",
                params![id, state],
            )
            .map(drop)
            .map_err(|err| self.err(err))
    }

    pub fn list_roots(&self) -> Result<Vec<Root>, DbError> {
        self.conn
            .prepare(
                "SELECT r.id, r.path, r.offline, (SELECT COUNT(*) FROM files f WHERE f.root_id = r.id),
                   r.settings_json, r.offline = 2
                 FROM roots r ORDER BY r.id",
            )
            .and_then(|mut stmt| {
                stmt.query_map([], |row| {
                    Ok(Root {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        offline: row.get(2)?,
                        missing: row.get(5)?,
                        files: row.get(3)?,
                        excludes: excludes_of(&row.get::<_, String>(4)?),
                        skip_nomedia: skip_nomedia_of(&row.get::<_, String>(4)?),
                    })
                })?
                .collect()
            })
            .map_err(|err| self.err(err))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn add(db: &Db, path: &Path) -> Result<i64, RootError> {
        db.add_root(&normalize_root(path)?)
    }

    #[test]
    fn cloud_folder_rejected() {
        assert!(matches!(check_attrs(0x20), Err(RootError::NotDir)));
        check_attrs(fsx::FILE_ATTRIBUTE_DIRECTORY | 0x400).unwrap();
        let err = check_attrs(fsx::FILE_ATTRIBUTE_DIRECTORY | fsx::FILE_ATTRIBUTE_RECALL_ON_OPEN)
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "该文件夹是网盘的按需下载文件夹，不支持索引。"
        );
    }

    #[test]
    fn add_list_remove() {
        let data = tempfile::tempdir().unwrap();
        let docs = tempfile::tempdir().unwrap();
        let db = Db::open(data.path()).unwrap();
        let id = add(&db, docs.path()).unwrap();
        let roots = db.list_roots().unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].id, id);
        assert!(!roots[0].path.starts_with(r"\\?\"));
        assert!(!roots[0].offline);
        assert_eq!(roots[0].files, 0);
        let (settings, version): (String, i64) = db
            .conn
            .query_row(
                "SELECT settings_json, settings_version FROM roots WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&settings).unwrap(),
            serde_json::json!({"exclude": []})
        );
        assert_eq!(version, 1);

        db.conn
            .execute(
                "INSERT INTO files (root_id, rel_path, name, ext, kind, size, mtime, status, updated_at)
                 VALUES (?1, 'a.txt', 'a.txt', 'txt', 'document', 1, 0, 'pending', 0)",
                [id],
            )
            .unwrap();
        assert_eq!(db.list_roots().unwrap()[0].files, 1);
        // 预估只算能识别内容的文件：占位文件、过大的不算
        for (name, ext, kind, size, cloud, status) in [
            ("b.md", "md", "document", 10, 0, "pending"),
            ("c.mp3", "mp3", "audio", 100, 0, "pending"),
            ("d.png", "png", "image", 20, 0, "pending"),
            ("e.png", "png", "image", 30, 1, "cloud"),
            ("f.png", "png", "image", 40, 0, "too_large"),
        ] {
            db.conn
                .execute(
                    "INSERT INTO files (root_id, rel_path, name, ext, kind, size, mtime, cloud, status, updated_at)
                     VALUES (?1, ?2, ?2, ?3, ?4, ?5, 0, ?6, ?7, 0)",
                    rusqlite::params![id, name, ext, kind, size, cloud, status],
                )
                .unwrap();
        }
        let mut totals = db.kind_totals().unwrap();
        totals.sort();
        assert_eq!(
            totals,
            [
                (id, "audio".to_string(), 1, 100),
                (id, "document".to_string(), 2, 11),
                (id, "image".to_string(), 1, 20)
            ]
        );
        db.remove_root(id).unwrap();
        assert!(db.list_roots().unwrap().is_empty());
        let n: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 0);
        assert!(matches!(db.remove_root(id), Err(RootError::NotFound)));
    }

    #[test]
    fn clear_index_keeps_roots_and_settings() {
        let data = tempfile::tempdir().unwrap();
        let docs = tempfile::tempdir().unwrap();
        let db = Db::open(data.path()).unwrap();
        let id = add(&db, docs.path()).unwrap();
        db.set_root_excludes(id, &["*.tmp".into()], None).unwrap();
        db.set_param("result_limit", &serde_json::json!(50))
            .unwrap();
        db.add_history("柯基", 10).unwrap();
        db.conn
            .execute_batch(&format!(
                "INSERT INTO files (id, root_id, rel_path, name, ext, kind, size, mtime, status, updated_at)
                 VALUES (1, {id}, 'a.txt', 'a.txt', 'txt', 'document', 1, 0, 'stored', 0);
                 INSERT INTO segments (id, file_id, modality, locator_json, text, created_at) VALUES (1, 1, 'text', '{{}}', '季度总结', 0);
                 INSERT INTO jobs (file_id, stage, priority, updated_at) VALUES (1, 'stored', 0, 0);
                 INSERT INTO blacklist (quick_hash, reason, failures, updated_at) VALUES (x'01', '坏', 3, 0);"
            ))
            .unwrap();
        while db.clear_index_batch(1).unwrap() > 0 {}
        let count = |table: &str| -> i64 {
            db.conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap()
        };
        for table in ["files", "segments", "jobs", "blacklist", "fts_names"] {
            assert_eq!(count(table), 0, "{table}");
        }
        // 外部内容表的 COUNT 读的是内容表，直接查索引
        let fts: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM fts_text WHERE fts_text MATCH '\"季度总\"'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fts, 0);
        let roots = db.list_roots().unwrap();
        assert_eq!(
            (roots[0].id, roots[0].excludes.clone()),
            (id, vec!["*.tmp".to_string()])
        );
        assert_eq!(db.params().unwrap()["result_limit"], serde_json::json!(50));
        assert_eq!(db.history(10).unwrap(), ["柯基"]);
    }

    #[test]
    fn excludes_saved_and_listed() {
        let data = tempfile::tempdir().unwrap();
        let docs = tempfile::tempdir().unwrap();
        let db = Db::open(data.path()).unwrap();
        let id = add(&db, docs.path()).unwrap();
        assert!(db.list_roots().unwrap()[0].excludes.is_empty());
        // 其他键保留
        db.conn
            .execute(
                "UPDATE roots SET settings_json = '{\"exclude\": [], \"x\": 1}' WHERE id = ?1",
                [id],
            )
            .unwrap();
        let rules = [" node_modules ", "", "照片\\raw", "*.tmp", "*.tmp"].map(String::from);
        db.set_root_excludes(id, &rules, None).unwrap();
        assert_eq!(
            db.list_roots().unwrap()[0].excludes,
            ["node_modules", "照片/raw", "*.tmp"]
        );
        let settings: String = db
            .conn
            .query_row(
                "SELECT settings_json FROM roots WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&settings).unwrap()["x"],
            1
        );
        // 跳过 .nomedia 默认为真；为空时不改
        assert!(db.list_roots().unwrap()[0].skip_nomedia);
        db.set_root_excludes(id, &rules, Some(false)).unwrap();
        assert!(!db.list_roots().unwrap()[0].skip_nomedia);
        db.set_root_excludes(id, &rules, None).unwrap();
        assert!(!db.list_roots().unwrap()[0].skip_nomedia);

        // 本文件夹之下的完整路径转成相对路径（大小写不同也算），末尾的 / 去掉，开头的 / 只在规则不再含 / 时保留，
        // 文件夹本身转成空规则后去掉；
        // 其他位置的完整路径报错
        let root = db.list_roots().unwrap()[0].path.to_uppercase();
        let rules = [
            format!(r"{root}\照片\RAW\"),
            "/node_modules/".into(),
            root.clone(),
            // 完整路径中 `\` 后跟 `[`、`{` 也是分隔符，不当转义
            format!(r"{root}\[草稿]\{{a,b}}.txt"),
            format!(r"{root}\*.BAK"),
        ];
        db.set_root_excludes(id, &rules, None).unwrap();
        assert_eq!(
            db.list_roots().unwrap()[0].excludes,
            ["照片/RAW", "/node_modules", "[草稿]/{a,b}.txt", "/*.BAK"]
        );
        for bad in [r"C:\其他", "//server/share/a", r"\\server\share\a"] {
            let err = db.set_root_excludes(id, &[bad.into()], None).unwrap_err();
            assert!(matches!(err, RootError::AbsoluteExclude(_)), "{err}");
            assert!(
                err.to_string().starts_with(&format!("排除规则 \"{bad}\"")),
                "{err}"
            );
        }
        // `\` 后跟特殊字符时是转义，原样保留（所以 `c\*` 是字面的 `c*`，此处要分隔时写 `/`）；其他 `\` 是分隔符。再保存一次结果不变，与转义后相同的规则去重
        let rules = [
            r"/报告\[1\].txt",
            r"子目录\年度 汇总/\{a,b\}.txt",
            r"a\\b\c\*",
            r"/报告\[1\].txt",
        ]
        .map(String::from);
        db.set_root_excludes(id, &rules, None).unwrap();
        let saved = db.list_roots().unwrap()[0].excludes.clone();
        assert_eq!(
            saved,
            [
                r"/报告\[1\].txt",
                r"子目录/年度 汇总/\{a,b\}.txt",
                r"a\\b/c\*"
            ]
        );
        db.set_root_excludes(id, &saved, None).unwrap();
        assert_eq!(db.list_roots().unwrap()[0].excludes, saved);
        let rules = [" node_modules ", r"照片\raw", "*.tmp"].map(String::from);
        db.set_root_excludes(id, &rules, None).unwrap();

        let err = db
            .set_root_excludes(id, &["ok".into(), "[".into()], None)
            .unwrap_err();
        assert!(
            matches!(err, RootError::BadExclude(ref r) if r == "["),
            "{err}"
        );
        // 写法不对时整组不保存
        assert_eq!(db.list_roots().unwrap()[0].excludes.len(), 3);
        assert!(matches!(
            db.set_root_excludes(id + 1, &[], None),
            Err(RootError::NotFound)
        ));
        // 设置损坏时列表为空，保存时从空对象开始
        db.conn
            .execute(
                "UPDATE roots SET settings_json = 'oops' WHERE id = ?1",
                [id],
            )
            .unwrap();
        assert!(db.list_roots().unwrap()[0].excludes.is_empty());
        db.set_root_excludes(id, &["a".into()], None).unwrap();
        assert_eq!(db.list_roots().unwrap()[0].excludes, ["a"]);
    }

    #[test]
    fn nested_roots_rejected() {
        let data = tempfile::tempdir().unwrap();
        let top = tempfile::tempdir().unwrap();
        let inner = top.path().join("子目录");
        fs::create_dir(&inner).unwrap();
        let db = Db::open(data.path()).unwrap();
        add(&db, &inner).unwrap();

        let err = add(&db, top.path()).unwrap_err();
        assert!(matches!(err, RootError::Contains(_)), "{err}");
        assert!(err.to_string().ends_with("\"，请先移除它。"), "{err}");

        let deeper = inner.join("更深");
        fs::create_dir(&deeper).unwrap();
        let err = add(&db, &deeper).unwrap_err();
        assert!(matches!(err, RootError::Inside(_)), "{err}");
        assert!(
            err.to_string().starts_with("该文件夹已包含在已添加的 \""),
            "{err}"
        );
        // 同一个文件夹换种写法也算包含
        let upper = PathBuf::from(inner.to_string_lossy().to_uppercase());
        if upper.exists() {
            assert!(matches!(add(&db, &upper), Err(RootError::Inside(_))));
        }
        // 名称前缀相同但不是上下级的文件夹可以添加
        let sibling = top.path().join("子目录2");
        fs::create_dir(&sibling).unwrap();
        add(&db, &sibling).unwrap();
    }

    #[test]
    fn data_dir_and_files_rejected() {
        let data = tempfile::tempdir().unwrap();
        let db = Db::open(data.path()).unwrap();
        assert!(matches!(add(&db, data.path()), Err(RootError::InDataDir)));
        let inside = data.path().join("thumbs");
        fs::create_dir(&inside).unwrap();
        assert!(matches!(add(&db, &inside), Err(RootError::InDataDir)));

        // 包含数据目录的文件夹可以添加，扫描时跳过数据目录
        let parent = tempfile::tempdir().unwrap();
        let nested_data = parent.path().join("data");
        let db2 = Db::open(&nested_data).unwrap();
        add(&db2, parent.path()).unwrap();

        let file = parent.path().join("a.txt");
        fs::write(&file, b"x").unwrap();
        let docs = tempfile::tempdir().unwrap();
        let db3 = Db::open(docs.path()).unwrap();
        assert!(matches!(add(&db3, &file), Err(RootError::NotDir)));
        let err = add(&db3, &parent.path().join("不存在")).unwrap_err();
        assert!(
            matches!(err, RootError::Fs(FsError::Inaccessible(_))),
            "{err}"
        );
    }
}
