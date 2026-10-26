//! 搜索页的文件夹筛选：`folder_filter` 表存各项的勾选状态（`rel` 为空是根目录本身，否则是用户添加的子文件夹），
//! “自定义”开关存在 `meta` 的 `folder_filter.custom`（`"1"` / `"0"`，缺省为否）。
//! 保存时整体替换：丢弃已不存在的根目录，`rel` 先规范化（`normalize_rel`），同一项重复时只留第一项。
//! 移除根目录时外键级联删除它的各项。
use std::collections::HashSet;

use rusqlite::params;
use rusqlite::types::Value;

use crate::db::{Db, DbError};
use crate::roots::Root;

const CUSTOM_KEY: &str = "folder_filter.custom";

/// 筛选中保存的一项，对应 `rummage_proto::FolderFilterEntry`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderEntry {
    pub root_id: i64,
    pub rel: String,
    pub checked: bool,
}

/// 相对根目录的路径规范化为本机写法：Windows 上 `/` 换成 `\`，去掉首尾分隔符
pub fn normalize_rel(rel: &str) -> String {
    let rel = if cfg!(windows) {
        rel.replace('/', "\\")
    } else {
        rel.to_string()
    };
    rel.trim_matches(std::path::MAIN_SEPARATOR).to_string()
}

/// 按绝对路径找它所在的根目录与相对根目录的路径（为空表示就是根目录）：不区分大小写，忽略末尾分隔符与 `/`、`\` 的区别；
/// 根目录的路径之后必须紧跟分隔符（`D:\照片2` 不属于 `D:\照片`），相对路径保留输入的大小写并规范化（`normalize_rel`）。
/// 根目录不会互相嵌套，最多一个匹配
pub fn find_scope<'a>(roots: &'a [Root], path: &str) -> Option<(&'a Root, String)> {
    let is_sep = |c: char| c == '/' || c == '\\';
    roots.iter().find_map(|r| {
        let mut rest = path;
        for rc in r.path.trim_end_matches(is_sep).chars() {
            let mut it = rest.chars();
            let pc = it.next()?;
            let same = if is_sep(rc) {
                is_sep(pc)
            } else {
                rc.to_lowercase().eq(pc.to_lowercase())
            };
            if !same {
                return None;
            }
            rest = it.as_str();
        }
        if !rest.is_empty() && !rest.starts_with(is_sep) {
            return None;
        }
        Some((r, normalize_rel(rest)))
    })
}

impl Db {
    /// (是否自定义, 各项)，各项按根目录与路径排序
    pub fn folder_filter(&self) -> Result<(bool, Vec<FolderEntry>), DbError> {
        let custom = self.meta_get(CUSTOM_KEY)?.as_deref() == Some("1");
        let entries = self
            .conn
            .prepare("SELECT root_id, rel, checked FROM folder_filter ORDER BY root_id, rel")
            .and_then(|mut stmt| {
                stmt.query_map([], |row| {
                    Ok(FolderEntry {
                        root_id: row.get(0)?,
                        rel: row.get(1)?,
                        checked: row.get(2)?,
                    })
                })?
                .collect()
            })
            .map_err(|err| self.err(err))?;
        Ok((custom, entries))
    }

    /// 整体替换保存的筛选
    pub fn set_folder_filter(&self, custom: bool, entries: &[FolderEntry]) -> Result<(), DbError> {
        let res = self.conn.unchecked_transaction().and_then(|tx| {
            let roots: HashSet<i64> = tx
                .prepare("SELECT id FROM roots")?
                .query_map([], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            tx.execute("DELETE FROM folder_filter", [])?;
            let mut seen = HashSet::new();
            for e in entries.iter().filter(|e| roots.contains(&e.root_id)) {
                let rel = normalize_rel(&e.rel);
                if !seen.insert((e.root_id, rel.clone())) {
                    continue;
                }
                tx.execute(
                    "INSERT INTO folder_filter (root_id, rel, checked) VALUES (?1, ?2, ?3)",
                    params![e.root_id, rel, e.checked],
                )?;
            }
            tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                params![CUSTOM_KEY, if custom { "1" } else { "0" }],
            )?;
            tx.commit()
        });
        res.map_err(|err| self.err(err))
    }

    /// 库中位于根目录 `root_id` 的 `rel`（含子文件夹）下的文件数，`rel` 为空时为整个根目录；匹配规则同搜索的文件夹范围
    pub fn folder_files(&self, root_id: i64, rel: &str) -> Result<i64, DbError> {
        let mut args: Vec<Value> = Vec::new();
        let sql = format!(
            "SELECT COUNT(*) FROM files f WHERE {}",
            crate::search::folders_sql(&[(root_id, rel.to_string())], &mut args)
        );
        self.conn
            .query_row(&sql, rusqlite::params_from_iter(args), |row| row.get(0))
            .map_err(|err| self.err(err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(root_id: i64, rel: &str, checked: bool) -> FolderEntry {
        FolderEntry {
            root_id,
            rel: rel.into(),
            checked,
        }
    }

    #[test]
    fn replaces_and_normalizes() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        assert_eq!(db.folder_filter().unwrap(), (false, Vec::new()));
        let a = db.add_root(r"D:\照片库").unwrap();
        let sep = std::path::MAIN_SEPARATOR;
        db.set_folder_filter(
            true,
            &[
                entry(a, "", false),
                entry(a, "/照片/2024/", true),
                entry(a, &format!("照片{sep}2024"), false),
                entry(a + 100, "", true),
            ],
        )
        .unwrap();
        assert_eq!(
            db.folder_filter().unwrap(),
            (
                true,
                vec![
                    entry(a, "", false),
                    entry(a, &format!("照片{sep}2024"), true)
                ]
            )
        );
        db.set_folder_filter(false, &[entry(a, "", true)]).unwrap();
        assert_eq!(
            db.folder_filter().unwrap(),
            (false, vec![entry(a, "", true)])
        );
        // 移除根目录时一并删除
        db.remove_root(a).unwrap();
        assert!(db.folder_filter().unwrap().1.is_empty());
    }

    #[test]
    fn finds_scope() {
        let root = |id, path: &str| Root {
            id,
            path: path.into(),
            offline: false,
            missing: false,
            files: 0,
            excludes: Vec::new(),
            skip_nomedia: true,
        };
        let roots = [
            root(1, r"D:\照片"),
            root(2, r"E:\"),
            root(3, "/storage/emulated/0/DCIM"),
        ];
        let find = |path: &str| find_scope(&roots, path).map(|(r, rel)| (r.id, rel));
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(find(r"D:\照片"), Some((1, String::new())));
        assert_eq!(find(r"d:/照片/"), Some((1, String::new())));
        assert_eq!(
            find(r"d:\照片\Trip\2024\"),
            Some((1, format!("Trip{sep}2024")))
        );
        assert_eq!(find(r"D:\照片2"), None);
        assert_eq!(find(r"D:\照"), None);
        assert_eq!(find(r"C:\照片"), None);
        assert_eq!(find(r"E:\"), Some((2, String::new())));
        assert_eq!(find(r"e:\Music"), Some((2, "Music".into())));
        assert_eq!(
            find("/storage/emulated/0/DCIM/Camera"),
            Some((3, "Camera".into()))
        );
    }

    #[test]
    fn counts_folder_files() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        let a = db.add_root(r"D:\照片库").unwrap();
        let sep = std::path::MAIN_SEPARATOR;
        for rel in [
            format!("照片{sep}a.jpg"),
            format!("照片{sep}深{sep}b.jpg"),
            format!("照片2{sep}c.jpg"),
        ] {
            db.conn
                .execute(
                    "INSERT INTO files (root_id, rel_path, name, ext, kind, size, mtime, status, updated_at)
                     VALUES (?1, ?2, ?2, '', 'image', 1, 0, 'stored', 0)",
                    params![a, rel],
                )
                .unwrap();
        }
        assert_eq!(db.folder_files(a, "").unwrap(), 3);
        assert_eq!(db.folder_files(a, "照片").unwrap(), 2);
        assert_eq!(db.folder_files(a, "没有").unwrap(), 0);
    }
}
