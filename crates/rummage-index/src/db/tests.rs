//! 数据库的测试：迁移与升级、打开失败的各种情形、备份与恢复、外键、全文索引、半精度编码
use super::*;

fn user_version(db: &Db) -> i64 {
    db.conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

fn names(db: &Db, kind: &str) -> Vec<String> {
    let mut stmt = db
        .conn
        .prepare("SELECT name FROM sqlite_master WHERE type = ?1 ORDER BY name")
        .unwrap();
    stmt.query_map([kind], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn pragma(db: &Db, name: &str) -> i64 {
    db.conn
        .pragma_query_value(None, name, |row| row.get(0))
        .unwrap()
}

#[test]
fn deleting_rows_shrinks_file() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path()).unwrap();
    assert_eq!(pragma(&db, "auto_vacuum"), 1);
    let base = pragma(&db, "page_count");
    for i in 0..200 {
        db.meta_set(&format!("k{i}"), &"x".repeat(4000)).unwrap();
    }
    assert!(pragma(&db, "page_count") > base + 100);
    db.conn.execute("DELETE FROM meta", []).unwrap();
    assert_eq!(pragma(&db, "freelist_count"), 0);
    assert!(pragma(&db, "page_count") <= base + 1);
}

#[test]
fn existing_db_switches_to_auto_vacuum() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path()).unwrap();
    db.conn
        .execute_batch("PRAGMA auto_vacuum = NONE; VACUUM;")
        .unwrap();
    assert_eq!(pragma(&db, "auto_vacuum"), 0);
    drop(db);
    let db = Db::open(dir.path()).unwrap();
    assert_eq!(pragma(&db, "auto_vacuum"), 1);
}

#[test]
fn migrates_empty_db_to_latest() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path()).unwrap();
    assert_eq!(user_version(&db), MIGRATIONS.len() as i64);
    let tables = names(&db, "table");
    for t in [
        "meta",
        "params",
        "spaces",
        "roots",
        "files",
        "segments",
        "vectors",
        "jobs",
        "blacklist",
        "fts_names",
        "search_history",
        "fts_text",
        "calib",
        "calib_queries",
    ] {
        assert!(tables.iter().any(|n| n == t), "缺少表 {t}");
    }
    assert_eq!(
        names(&db, "trigger"),
        [
            "files_fts_delete",
            "files_fts_insert",
            "files_fts_update",
            "segments_fts_delete",
            "segments_fts_insert",
            "segments_fts_update"
        ]
    );
    let mode: String = db
        .conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let fk: i64 = db
        .conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    assert_eq!(fk, 1);
    let recursive: i64 = db
        .conn
        .query_row("PRAGMA recursive_triggers", [], |row| row.get(0))
        .unwrap();
    assert_eq!(recursive, 1);
    // trigram 分词器要求 SQLite ≥ 3.34
    assert!(rusqlite::version_number() >= 3_034_000);
    drop(db);
    // 再次打开不重复迁移
    let db = Db::open(dir.path()).unwrap();
    assert_eq!(user_version(&db), MIGRATIONS.len() as i64);
}

#[test]
fn clears_stale_tmp_files() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("tmp")).unwrap();
    fs::write(dir.path().join("tmp").join("x.tmp"), b"x").unwrap();
    Db::open(dir.path()).unwrap();
    assert_eq!(fs::read_dir(dir.path().join("tmp")).unwrap().count(), 0);
}

#[test]
fn unwritable_dir_rejected() {
    let dir = tempfile::tempdir().unwrap();
    // 用普通文件占住数据目录的位置，无法创建 tmp
    let blocked = dir.path().join("data");
    fs::write(&blocked, b"").unwrap();
    assert!(matches!(Db::open(&blocked), Err(DbError::Unwritable(_))));
}

#[test]
fn readonly_db_rejected() {
    let dir = tempfile::tempdir().unwrap();
    drop(Db::open(dir.path()).unwrap());
    let path = dir.path().join(DB_FILE);
    let mut perm = fs::metadata(&path).unwrap().permissions();
    perm.set_readonly(true);
    fs::set_permissions(&path, perm.clone()).unwrap();
    let res = Db::open(dir.path());
    // 恢复可写，临时目录才能删掉
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    fs::set_permissions(&path, perm).unwrap();
    assert!(
        matches!(res, Err(DbError::Unwritable(_))),
        "{:?}",
        res.err()
    );
}

#[test]
fn corrupt_db_reported() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(DB_FILE), vec![0x5a; 8192]).unwrap();
    let err = Db::open(dir.path()).err().unwrap();
    assert!(matches!(err, DbError::Corrupt), "{err}");
}

#[cfg(windows)]
#[test]
fn missing_drive_reported() {
    // 找一个不存在的盘符
    let Some(letter) = ('D'..='Z')
        .rev()
        .find(|l| !Path::new(&format!(r"{l}:\")).exists())
    else {
        return;
    };
    let err = Db::open(Path::new(&format!(r"{letter}:\Rummage\data")))
        .err()
        .unwrap();
    assert!(matches!(err, DbError::DriveMissing(_)), "{err}");
    assert!(err.to_string().contains("未连接"));
}

#[test]
fn rejects_newer_db() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = Db::open(dir.path()).unwrap();
        db.conn
            .pragma_update(None, "user_version", MIGRATIONS.len() as i64 + 1)
            .unwrap();
    }
    let err = Db::open(dir.path()).err().unwrap();
    assert!(matches!(err, DbError::TooNew));
    assert_eq!(
        err.to_string(),
        "索引数据库来自更新版本的 Rummage，请升级应用后再打开。"
    );
}

fn backups(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".bak"))
        .collect()
}

#[test]
fn backup_removed_after_migration() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = Db::open(dir.path()).unwrap();
        db.meta_set("k", "v").unwrap();
    }
    // 新库直接建表，不备份
    assert!(backups(dir.path()).is_empty());

    let mut migrations = MIGRATIONS.to_vec();
    migrations.push("CREATE TABLE extra (a TEXT);");
    // 备份确实生成并可用，由 failed_migration_restores_backup 验证；这里验证成功后删除
    let bak = dir
        .path()
        .join(format!("{DB_FILE}.v{}.bak", MIGRATIONS.len()));
    let db = Db::open_with(dir.path(), &migrations).unwrap();
    assert_eq!(user_version(&db), migrations.len() as i64);
    assert_eq!(db.meta_get("k").unwrap().as_deref(), Some("v"));
    assert!(!bak.exists());
    assert!(backups(dir.path()).is_empty());
}

#[test]
fn failed_migration_restores_backup() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = Db::open(dir.path()).unwrap();
        db.meta_set("k", "v").unwrap();
    }
    let mut migrations = MIGRATIONS.to_vec();
    // 第一项成功提交，第二项失败；恢复后第一项的改动也要消失
    migrations.push("CREATE TABLE extra (a TEXT); INSERT INTO extra VALUES ('x');");
    migrations.push("INSERT INTO no_such_table VALUES (1);");
    let err = Db::open_with(dir.path(), &migrations).err().unwrap();
    assert!(
        matches!(err, DbError::Migrate(from) if from == MIGRATIONS.len() + 1),
        "{err}"
    );
    assert!(backups(dir.path()).is_empty());

    let db = Db::open(dir.path()).unwrap();
    assert_eq!(user_version(&db), MIGRATIONS.len() as i64);
    assert_eq!(db.meta_get("k").unwrap().as_deref(), Some("v"));
    assert!(!names(&db, "table").iter().any(|n| n == "extra"));
}

/// 建一个带数据的当前版本库，再复制一份作为“上次迁移留下的备份”，返回备份路径
fn with_leftover_backup(dir: &Path) -> PathBuf {
    {
        let db = Db::open(dir).unwrap();
        db.meta_set("k", "v").unwrap();
        db.conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .unwrap();
    }
    let bak = backup_path(dir, MIGRATIONS.len());
    fs::copy(dir.join(DB_FILE), &bak).unwrap();
    bak
}

fn one_more() -> Vec<&'static str> {
    let mut migrations = MIGRATIONS.to_vec();
    migrations.push("CREATE TABLE extra (a TEXT);");
    migrations
}

#[test]
fn leftover_backup_overwritten_when_db_intact() {
    let dir = tempfile::tempdir().unwrap();
    let bak = with_leftover_backup(dir.path());
    fs::write(&bak, b"old").unwrap();
    let migrations = one_more();
    let db = Db::open_with(dir.path(), &migrations).unwrap();
    assert_eq!(user_version(&db), migrations.len() as i64);
    assert_eq!(db.meta_get("k").unwrap().as_deref(), Some("v"));
    assert_eq!(db.meta_get(NOTICE_DB_RESTORED).unwrap(), None);
    assert!(!bak.exists());
}

#[test]
fn corrupt_db_restored_from_leftover_backup() {
    let dir = tempfile::tempdir().unwrap();
    let bak = with_leftover_backup(dir.path());
    fs::write(dir.path().join(DB_FILE), vec![0x5a; 8192]).unwrap();
    let migrations = one_more();
    let db = Db::open_with(dir.path(), &migrations).unwrap();
    assert_eq!(user_version(&db), migrations.len() as i64);
    assert_eq!(db.meta_get("k").unwrap().as_deref(), Some("v"));
    assert!(db.meta_get(NOTICE_DB_RESTORED).unwrap().is_some());
    assert!(!bak.exists());
}

#[test]
fn corrupt_backup_fails_restore() {
    let dir = tempfile::tempdir().unwrap();
    let bak = with_leftover_backup(dir.path());
    fs::write(dir.path().join(DB_FILE), vec![0x5a; 8192]).unwrap();
    fs::write(&bak, vec![0x5a; 8192]).unwrap();
    let err = Db::open_with(dir.path(), &one_more()).err().unwrap();
    assert!(
        matches!(&err, DbError::RestoreFailed { backup } if *backup == bak),
        "{err}"
    );
    assert!(err.to_string().contains(&format!(
        "用数据目录中的 \"{DB_FILE}.v{}.bak\" 替换",
        MIGRATIONS.len()
    )));
    assert!(bak.exists());
}

#[test]
fn restore_keeps_user_fixable_error() {
    let dir = tempfile::tempdir().unwrap();
    let bak = with_leftover_backup(dir.path());
    fs::write(dir.path().join(DB_FILE), vec![0x5a; 8192]).unwrap();
    // 恢复后升级前的备份要先写到 tmp\index.db：放一个同名目录让它失败
    fs::create_dir_all(dir.path().join("tmp").join(DB_FILE)).unwrap();
    let err = Db::open_with(dir.path(), &one_more()).err().unwrap();
    assert!(matches!(err, DbError::Backup), "{err}");
    assert!(bak.exists());
}

#[test]
fn foreign_key_violation_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    drop(Db::open(dir.path()).unwrap());
    let mut migrations = MIGRATIONS.to_vec();
    // 迁移期间外键关闭，这条插入本身能成功，提交前的检查要拦下它
    migrations.push(
        "INSERT INTO files (root_id, rel_path, name, ext, kind, size, mtime, status, updated_at)
         VALUES (999, 'a', 'a', '', 'other', 0, 0, 'pending', 0);",
    );
    let err = Db::open_with(dir.path(), &migrations).err().unwrap();
    assert!(
        matches!(err, DbError::Migrate(from) if from == MIGRATIONS.len()),
        "{err}"
    );
    let db = Db::open(dir.path()).unwrap();
    assert_eq!(user_version(&db), MIGRATIONS.len() as i64);
    let n: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(n, 0);
    assert!(backups(dir.path()).is_empty());
}

fn fts(db: &Db, query: &str) -> Vec<i64> {
    let mut stmt = db
        .conn
        .prepare("SELECT rowid FROM fts_names WHERE fts_names MATCH ?1 ORDER BY rowid")
        .unwrap();
    stmt.query_map([format!("\"{query}\"")], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn fts_names_trigram_sync() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path()).unwrap();
    db.conn
        .execute(
            "INSERT INTO roots (id, path, settings_json, settings_version, added_at)
             VALUES (1, 'D:\\资料', '{}', 1, 0)",
            [],
        )
        .unwrap();
    let insert = |id: i64, rel: &str, name: &str| {
        db.conn
            .execute(
                "INSERT INTO files (id, root_id, rel_path, name, ext, kind, size, mtime, status, updated_at)
                 VALUES (?1, 1, ?2, ?3, 'pdf', 'document', 1, 0, 'pending', 0)",
                params![id, rel, name],
            )
            .unwrap();
    };
    insert(1, "工作\\2025年度总结报告.pdf", "2025年度总结报告.pdf");
    insert(2, "旅行\\京都游记.pdf", "京都游记.pdf");
    assert_eq!(fts(&db, "总结报"), [1]);
    assert_eq!(fts(&db, "京都游"), [2]);
    // 路径列也可搜
    assert_eq!(fts(&db, "旅行\\"), [2]);
    assert!(fts(&db, "不存在").is_empty());

    // 改名
    db.conn
        .execute(
            "UPDATE files SET name = '年终汇报.pdf', rel_path = '工作\\年终汇报.pdf' WHERE id = 1",
            [],
        )
        .unwrap();
    assert!(fts(&db, "总结报").is_empty());
    assert_eq!(fts(&db, "年终汇"), [1]);
    // 只改其他列不影响索引
    db.conn
        .execute("UPDATE files SET status = 'stored' WHERE id = 1", [])
        .unwrap();
    assert_eq!(fts(&db, "年终汇"), [1]);

    // 删除
    db.conn
        .execute("DELETE FROM files WHERE id = 2", [])
        .unwrap();
    assert!(fts(&db, "京都游").is_empty());
    // 删除根目录时级联删除文件，索引同步
    db.conn
        .execute("DELETE FROM roots WHERE id = 1", [])
        .unwrap();
    assert!(fts(&db, "年终汇").is_empty());
    // 外部内容表的完整性检查
    db.conn
        .execute(
            "INSERT INTO fts_names (fts_names) VALUES ('integrity-check')",
            [],
        )
        .unwrap();
}

fn fts_text(db: &Db, query: &str) -> Vec<i64> {
    let mut stmt = db
        .conn
        .prepare("SELECT rowid FROM fts_text WHERE fts_text MATCH ?1 ORDER BY rowid")
        .unwrap();
    stmt.query_map([format!("\"{query}\"")], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn fts_text_check(db: &Db) {
    db.conn
        .execute(
            "INSERT INTO fts_text (fts_text) VALUES ('integrity-check')",
            [],
        )
        .unwrap();
}

const TWO_FILES: &str = "
    INSERT INTO roots (id, path, settings_json, settings_version, added_at) VALUES (1, 'D:\\资料', '{}', 1, 0);
    INSERT INTO files (id, root_id, rel_path, name, ext, kind, size, mtime, status, updated_at)
    VALUES (1, 1, 'a.md', 'a.md', 'md', 'document', 1, 0, 'stored', 0),
           (2, 1, 'b.jpg', 'b.jpg', 'jpg', 'image', 1, 0, 'stored', 0);";

#[test]
fn fts_text_trigram_sync() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path()).unwrap();
    db.conn.execute_batch(TWO_FILES).unwrap();
    let insert = |id: i64, file: i64, text: Option<&str>| {
        db.conn
            .execute(
                "INSERT INTO segments (id, file_id, modality, locator_json, text, created_at)
                 VALUES (?1, ?2, 'text', '{}', ?3, 0)",
                params![id, file, text],
            )
            .unwrap();
    };
    insert(1, 1, Some("京都的红叶在十一月最好看"));
    insert(2, 1, Some("Quarterly Report"));
    insert(3, 2, None);
    assert_eq!(fts_text(&db, "红叶在"), [1]);
    // trigram 默认不区分大小写
    assert_eq!(fts_text(&db, "quarterly"), [2]);
    // 改正文
    db.conn
        .execute("UPDATE segments SET text = '奈良的鹿' WHERE id = 1", [])
        .unwrap();
    assert!(fts_text(&db, "红叶在").is_empty());
    assert_eq!(fts_text(&db, "奈良的"), [1]);
    // 删除片段、删除文件、删除根目录
    db.conn
        .execute("DELETE FROM segments WHERE id = 2", [])
        .unwrap();
    assert!(fts_text(&db, "quarterly").is_empty());
    fts_text_check(&db);
    db.conn
        .execute("DELETE FROM roots WHERE id = 1", [])
        .unwrap();
    assert!(fts_text(&db, "奈良的").is_empty());
    fts_text_check(&db);
}

#[test]
fn params_meta_spaces() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path()).unwrap();
    assert!(db.params().unwrap().is_empty());
    db.set_param("a", &serde_json::json!(1.5)).unwrap();
    db.set_param("b", &serde_json::json!(true)).unwrap();
    db.set_param("a", &serde_json::json!(2)).unwrap();
    db.conn
        .execute("INSERT INTO params VALUES ('bad', '{oops')", [])
        .unwrap();
    let all = db.params().unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all["a"], serde_json::json!(2));
    assert_eq!(all["b"], serde_json::json!(true));

    assert_eq!(db.meta_get("x").unwrap(), None);
    db.meta_set("x", "1").unwrap();
    db.meta_set("x", "2").unwrap();
    assert_eq!(db.meta_get("x").unwrap().as_deref(), Some("2"));
    db.meta_delete("x").unwrap();
    db.meta_delete("x").unwrap();
    assert_eq!(db.meta_get("x").unwrap(), None);

    let space = Space {
        id: "0123456789abcdef".into(),
        family: "embeddinggemma-2".into(),
        model_sha256: "aa".into(),
        mmproj_sha256: None,
        runtime: "llama.cpp b11461".into(),
        backend: "cpu".into(),
        precision: "q8_0".into(),
        prefix_scheme: "A".into(),
        preprocess: "text-v1".into(),
    };
    assert!(db.insert_space(&space).unwrap());
    let changed = Space {
        backend: "vulkan".into(),
        ..space.clone()
    };
    assert!(!db.insert_space(&changed).unwrap());
    let backend: String = db
        .conn
        .query_row(
            "SELECT backend FROM spaces WHERE id = ?1",
            [&space.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(backend, "cpu");
}

#[test]
fn f16_round_trip() {
    let max = f16::MAX.to_f32();
    let min_sub = f16::from_bits(1).to_f32();
    let v = [
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.5,
        max,
        -max,
        f16::MIN_POSITIVE.to_f32(),
        min_sub,
        f32::INFINITY,
        f32::NEG_INFINITY,
    ];
    let blob = vec_to_blob(&v);
    assert_eq!(blob.len(), v.len() * 2);
    let back = blob_to_vec(&blob).unwrap();
    for (a, b) in v.iter().zip(&back) {
        assert_eq!(a.to_bits(), b.to_bits(), "{a} 往返后为 {b}");
    }
    // 小端：1.0 = 0x3C00
    assert_eq!(&vec_to_blob(&[1.0]), &[0x00, 0x3C]);
    // 超出范围变为无穷，小于最小次正规数一半的变为 0
    assert_eq!(blob_to_vec(&vec_to_blob(&[1e6])).unwrap(), [f32::INFINITY]);
    assert_eq!(blob_to_vec(&vec_to_blob(&[1e-9])).unwrap(), [0.0]);
    // 舍入到最近偶数：1 + 2^-11 恰在 1 与 1 + 2^-10 中间，取尾数为偶的 1；1 + 3·2^-11 取 1 + 2^-9
    let ulp = 2f32.powi(-10);
    assert_eq!(
        blob_to_vec(&vec_to_blob(&[1.0 + ulp / 2.0, 1.0 + 3.0 * ulp / 2.0])).unwrap(),
        [1.0, 1.0 + 2.0 * ulp]
    );
    // 768 维向量的误差在 f16 精度内
    let v: Vec<f32> = (0..768).map(|i| ((i as f32) * 0.37).sin() * 0.1).collect();
    let back = blob_to_vec(&vec_to_blob(&v)).unwrap();
    assert_eq!(vec_to_blob(&v).len(), 1536);
    for (a, b) in v.iter().zip(&back) {
        assert!((a - b).abs() <= a.abs() * 2f32.powi(-11) + 1e-7);
    }
    assert_eq!(blob_to_vec(&[1, 2, 3]), None);
}
