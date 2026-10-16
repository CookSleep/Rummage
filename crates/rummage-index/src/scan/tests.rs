//! 扫描与对账的测试：种类与优先级、对账、排除规则、占位文件与大小上限、改名与移动、局部对账
use std::fs;

use super::*;
use crate::Space;

mod exclude;
mod limits;
mod pair;

use crate::scan::TEST_LIMITS as LIMITS;

struct Env {
    /// 字段按声明顺序丢弃：数据库连接先关闭，临时目录才能删掉
    db: Mutex<Db>,
    root: tempfile::TempDir,
    _data: tempfile::TempDir,
    id: i64,
}

fn env() -> Env {
    let data = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let db = Db::open(data.path()).unwrap();
    let id = db
        .add_root(&crate::normalize_root(root.path()).unwrap())
        .unwrap();
    Env {
        db: Mutex::new(db),
        root,
        _data: data,
        id,
    }
}

impl Env {
    fn put(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }

    fn scan_attrs(&self, extra: &dyn Fn(&str) -> u32) -> ScanStats {
        scan_with(
            &self.db,
            self.id,
            LIMITS,
            &AtomicBool::new(false),
            &mut |_| {},
            extra,
        )
        .unwrap()
    }

    fn scan(&self) -> ScanStats {
        self.scan_attrs(&|_| 0)
    }

    /// (rel_path, kind, status, cloud)，按路径排序；路径分隔符统一为 `/`
    fn files(&self) -> Vec<(String, String, String, bool)> {
        let db = lock(&self.db);
        let mut stmt = db
            .conn
            .prepare("SELECT rel_path, kind, status, cloud FROM files ORDER BY rel_path")
            .unwrap();
        stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?.replace('\\', "/"),
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    }

    fn names(&self) -> Vec<String> {
        self.files().into_iter().map(|f| f.0).collect()
    }

    /// (rel_path, stage, attempts)
    fn jobs(&self) -> Vec<(String, String, i64)> {
        let db = lock(&self.db);
        let mut stmt = db
            .conn
            .prepare(
                "SELECT f.rel_path, j.stage, j.attempts FROM jobs j JOIN files f ON f.id = j.file_id
                 ORDER BY f.rel_path",
            )
            .unwrap();
        stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?.replace('\\', "/"),
                row.get(1)?,
                row.get(2)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    }

    fn exec(&self, sql: &str) {
        lock(&self.db).conn.execute_batch(sql).unwrap();
    }

    fn count(&self, sql: &str) -> i64 {
        lock(&self.db)
            .conn
            .query_row(sql, [], |row| row.get(0))
            .unwrap()
    }

    fn offline(&self) -> bool {
        self.count(&format!("SELECT offline FROM roots WHERE id = {}", self.id)) == 1
    }

    fn scan_limits(&self, limits: ScanLimits) -> ScanStats {
        scan_root(
            &self.db,
            self.id,
            limits,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap()
    }

    fn exclude(&self, rules: &[&str]) {
        let json = serde_json::json!({ "exclude": rules }).to_string();
        lock(&self.db)
            .conn
            .execute(
                "UPDATE roots SET settings_json = ?2 WHERE id = ?1",
                params![self.id, json],
            )
            .unwrap();
    }

    /// 模拟索引流水线已为该文件写好一条向量
    fn store(&self, name: &str) {
        let db = lock(&self.db);
        db.insert_space(&Space {
            id: "s".into(),
            family: "f".into(),
            model_sha256: "m".into(),
            mmproj_sha256: None,
            runtime: "r".into(),
            backend: "cpu".into(),
            precision: "q8_0".into(),
            prefix_scheme: "A".into(),
            preprocess: "text-v1".into(),
        })
        .unwrap();
        let id: i64 = db
            .conn
            .query_row("SELECT id FROM files WHERE name = ?1", [name], |row| {
                row.get(0)
            })
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO segments (file_id, modality, locator_json, created_at)
                 VALUES (?1, 'text', '{}', 0)",
                [id],
            )
            .unwrap();
        let seg = db.conn.last_insert_rowid();
        db.conn
            .execute(
                "INSERT INTO vectors VALUES (?1, 's', 2, x'0000', 'v1', x'')",
                [seg],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO jobs (file_id, stage, priority, updated_at) VALUES (?1, 'stored', 0, 0)
                 ON CONFLICT (file_id) DO UPDATE SET stage = 'stored'",
                [id],
            )
            .unwrap();
        db.conn
            .execute("UPDATE files SET status = 'stored' WHERE id = ?1", [id])
            .unwrap();
    }

    fn vectors(&self) -> i64 {
        self.count("SELECT COUNT(*) FROM vectors") + self.count("SELECT COUNT(*) FROM segments")
    }

    fn status(&self, name: &str) -> String {
        self.files().into_iter().find(|f| f.0 == name).unwrap().2
    }
}

#[test]
fn kinds_table() {
    for (ext, kind) in [
        ("pdf", "document"),
        ("md", "document"),
        ("txt", "document"),
        ("json", "document"),
        ("rs", "other"),
        ("ts", "other"),
        ("mts", "other"),
        ("m2ts", "video"),
        ("jpg", "image"),
        ("heic", "image"),
        ("flac", "audio"),
        ("opus", "audio"),
        ("mkv", "video"),
        ("3gp", "video"),
        ("zip", "other"),
        ("svg", "other"),
        ("", "other"),
    ] {
        assert_eq!(kind_of(ext), kind, "{ext}");
    }
    let all: Vec<&str> = KINDS
        .iter()
        .flat_map(|(_, exts)| exts.iter().copied())
        .collect();
    for (i, e) in all.iter().enumerate() {
        assert!(!all[i + 1..].contains(e), "{e} 重复");
        assert_eq!(*e, e.to_lowercase());
    }
}

#[test]
fn indexed_formats_queued() {
    let e = env();
    for name in [
        "a.pdf", "b.docx", "c.html", "d.json", "e.txt", "f.MD", "g.py", "h.png", "i.mp3", "j.zip",
    ] {
        e.put(name, b"x");
    }
    e.scan();
    assert_eq!(e.names().len(), 10);
    let jobs: Vec<String> = e.jobs().into_iter().map(|j| j.0).collect();
    assert_eq!(
        jobs,
        [
            "a.pdf", "b.docx", "c.html", "d.json", "e.txt", "f.MD", "h.png", "i.mp3"
        ]
    );
    assert!(wants_job("other", "ts"));
    assert!(wants_job("other", "mts"));
    assert!(!wants_job("other", "py"));
    assert!(wants_job("audio", "wav"));
    assert!(wants_job("document", "pdf"));
    assert!(wants_job("video", "mp4"));
    assert!(!wants_job("other", "txt"));
}

#[test]
fn priority_order() {
    let old = 1_000_000_000_000;
    let new = 1_700_000_000_000;
    assert!(priority("image", old) > priority("document", new));
    assert!(priority("image", new) > priority("image", old));
    // 早于 1970 年的按 0 计，不越过档位
    assert_eq!(priority("image", -5), priority("image", 0));
    assert!(priority("image", 0) > priority("document", i64::MAX));
    // 音频排在文档之后
    assert!(priority("document", old) > priority("audio", new));
    assert!(priority("audio", new) > priority("audio", old));
    // 视频最后
    assert!(priority("audio", 0) > priority("video", i64::MAX));
    assert!(priority("video", new) > priority("video", old));
}

#[test]
fn reconcile() {
    let e = env();
    e.put("a.txt", b"hello");
    e.put("sub/b.png", b"png");
    e.put("sub/c.mp3", b"mp3");
    e.put("d.zip", b"zip");
    let s = e.scan();
    assert_eq!((s.added, s.updated, s.removed), (4, 0, 0));
    assert_eq!(
        e.files(),
        [
            ("a.txt".into(), "document".into(), "pending".into(), false),
            ("d.zip".into(), "other".into(), "pending".into(), false),
            ("sub/b.png".into(), "image".into(), "pending".into(), false),
            ("sub/c.mp3".into(), "audio".into(), "pending".into(), false),
        ]
    );
    // 只有文档、图片、音频、视频建作业
    assert_eq!(
        e.jobs(),
        [
            ("a.txt".into(), "pending".into(), 0),
            ("sub/b.png".into(), "pending".into(), 0),
            ("sub/c.mp3".into(), "pending".into(), 0)
        ]
    );
    // 图片排在前面
    let first: String = lock(&e.db)
        .conn
        .query_row(
            "SELECT f.name FROM jobs j JOIN files f ON f.id = j.file_id ORDER BY j.priority DESC",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(first, "b.png");

    // 未变化
    assert_eq!(e.scan(), ScanStats::default());

    // 修改：清空哈希，作业重置
    e.exec(
        "UPDATE files SET quick_hash = x'01', content_hash = x'02';
         UPDATE jobs SET stage = 'failed', attempts = 3, last_error = 'x';",
    );
    e.put("a.txt", b"hello world");
    let s = e.scan();
    assert_eq!((s.added, s.updated, s.removed), (0, 1, 0));
    assert_eq!(
        e.count("SELECT COUNT(*) FROM files WHERE quick_hash IS NULL AND content_hash IS NULL"),
        1
    );
    assert_eq!(
        e.jobs(),
        [
            ("a.txt".into(), "pending".into(), 0),
            ("sub/b.png".into(), "failed".into(), 3),
            ("sub/c.mp3".into(), "failed".into(), 3)
        ]
    );
    assert_eq!(e.count("SELECT size FROM files WHERE name = 'a.txt'"), 11);

    // 删除
    fs::remove_file(e.root.path().join("sub").join("b.png")).unwrap();
    let s = e.scan();
    assert_eq!((s.added, s.updated, s.removed), (0, 0, 1));
    assert_eq!(e.names(), ["a.txt", "d.zip", "sub/c.mp3"]);
    assert_eq!(e.jobs().len(), 2);
    assert_eq!(
        e.count("SELECT COUNT(*) FROM fts_names WHERE fts_names MATCH '\"b.png\"'"),
        0
    );
}

#[test]
fn data_dir_skipped() {
    let root = tempfile::tempdir().unwrap();
    let db = Db::open(&root.path().join("data")).unwrap();
    let id = db
        .add_root(&crate::normalize_root(root.path()).unwrap())
        .unwrap();
    fs::write(root.path().join("a.txt"), b"x").unwrap();
    let db = Mutex::new(db);
    let s = scan_root(&db, id, LIMITS, &AtomicBool::new(false), &mut |_| {}).unwrap();
    assert_eq!(s.added, 1);
    assert_eq!(s.excluded, 1);
}

#[test]
fn offline_root_keeps_records() {
    let e = env();
    e.put("a.txt", b"x");
    e.put("b.png", b"x");
    e.scan();
    let moved = e.root.path().with_extension("moved");
    fs::rename(e.root.path(), &moved).unwrap();
    let s = e.scan();
    assert_eq!(s, ScanStats::default());
    assert!(e.offline());
    assert_eq!(e.names().len(), 2);
    assert_eq!(e.jobs().len(), 2);

    fs::rename(&moved, e.root.path()).unwrap();
    e.put("c.md", b"x");
    let s = e.scan();
    assert!(!e.offline());
    assert_eq!((s.added, s.updated, s.removed), (1, 0, 0));
}

#[test]
fn removed_root_stops_writes() {
    let e = env();
    e.put("a.txt", b"x");
    let done = std::cell::Cell::new(false);
    // 遍历期间根目录被移除，同一 id 又被另一个根目录用上
    let res = scan_with(
        &e.db,
        e.id,
        LIMITS,
        &AtomicBool::new(false),
        &mut |_| {},
        &|_| {
            if !done.replace(true) {
                e.exec(&format!(
                    r"DELETE FROM roots WHERE id = {0};
                      INSERT INTO roots (id, path, settings_json, settings_version, added_at)
                      VALUES ({0}, 'X:\其他', '{{}}', 1, 0);",
                    e.id
                ));
            }
            0
        },
    );
    assert!(matches!(res, Err(ScanError::Cancelled)), "{res:?}");
    assert_eq!(e.count("SELECT COUNT(*) FROM files"), 0);
}

#[cfg(windows)]
#[test]
fn links_not_followed() {
    let e = env();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), b"x").unwrap();
    e.put("ok.txt", b"x");
    // 目录联接不需要特殊权限
    let junction = e.root.path().join("junction");
    let out = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(&junction)
        .arg(outside.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let mut links = 1;
    // 符号链接需要管理员或开发者模式
    match std::os::windows::fs::symlink_dir(outside.path(), e.root.path().join("dirlink")) {
        Ok(()) => {
            std::os::windows::fs::symlink_file(
                outside.path().join("secret.txt"),
                e.root.path().join("filelink.txt"),
            )
            .unwrap();
            links += 2;
        }
        Err(err) => println!("跳过符号链接部分：没有创建符号链接的权限（{err}）"),
    }
    let s = e.scan();
    assert_eq!(e.names(), ["ok.txt"]);
    assert_eq!(s.excluded, links);
    // 删除联接本身，不删目标
    fs::remove_dir(&junction).unwrap();
    assert!(outside.path().join("secret.txt").exists());
}

#[test]
fn cancel_keeps_records() {
    let e = env();
    e.put("a.txt", b"x");
    e.scan();
    fs::remove_file(e.root.path().join("a.txt")).unwrap();
    let res = scan_root(&e.db, e.id, LIMITS, &AtomicBool::new(true), &mut |_| {});
    assert!(matches!(res, Err(ScanError::Cancelled)));
    assert_eq!(e.names(), ["a.txt"]);
    let res = scan_root(&e.db, 999, LIMITS, &AtomicBool::new(false), &mut |_| {});
    assert!(matches!(res, Err(ScanError::NoRoot)));
}

#[test]
fn batches_and_progress() {
    let e = env();
    for i in 0..2500 {
        e.put(&format!("d{}/{i}.txt", i % 7), b"x");
    }
    let mut calls = Vec::new();
    let s = scan_root(&e.db, e.id, LIMITS, &AtomicBool::new(false), &mut |s| {
        calls.push(s.added)
    })
    .unwrap();
    assert_eq!(s.added, 2500);
    assert_eq!(calls, [1000, 2000]);
    assert_eq!(e.count("SELECT COUNT(*) FROM files"), 2500);
    assert_eq!(e.count("SELECT COUNT(*) FROM jobs"), 2500);
}

#[test]
fn under_prefix() {
    let sep = MAIN_SEPARATOR_STR;
    assert!(under("a", ""));
    assert!(under("a", "a"));
    assert!(under(&format!("a{sep}b"), "a"));
    assert!(!under("ab", "a"));
    assert!(!under("b", "a"));
}
