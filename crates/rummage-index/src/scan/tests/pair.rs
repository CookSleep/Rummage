//! 改名与移动：配对后保留向量、没有文件 ID 时按 `quick_hash` 配对、扩展名变化按删除 + 新增处理、
//! 跨根目录的移动；局部对账（监听到变化的目录）的范围与 `dirty_of`。
use super::*;

/// 本机路径分隔符的相对路径
fn rel(p: &str) -> String {
    p.replace('/', MAIN_SEPARATOR_STR)
}

impl Env {
    /// 模拟索引流水线建好向量：写入片段、向量、已完成的作业，以及 quick_hash 和文件 ID
    fn index(&self, rel_path: &str) {
        let name = Path::new(rel_path).file_name().unwrap().to_str().unwrap();
        self.store(name);
        let (qh, id) = crate::hash::quick_hash_with_id(&self.root.path().join(rel_path)).unwrap();
        lock(&self.db)
            .conn
            .execute(
                "UPDATE files SET quick_hash = ?2, volume_file_id = ?3 WHERE name = ?1",
                params![name, &qh[..], id],
            )
            .unwrap();
    }

    /// (rel_path, 文件 id, 片段 id)，路径分隔符统一为 `/`
    fn segs(&self) -> Vec<(String, i64, i64)> {
        let db = lock(&self.db);
        let mut stmt = db
            .conn
            .prepare(
                "SELECT f.rel_path, f.id, s.id FROM segments s JOIN files f ON f.id = s.file_id
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

    fn mv(&self, from: &str, to: &str) {
        let to = self.root.path().join(to);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::rename(self.root.path().join(from), to).unwrap();
    }

    /// 按监听事件的路径算出局部对账范围后对账（`dirty_of`）
    fn scan_paths(&self, paths: &[&str]) -> ScanStats {
        let dirs: Vec<Dirty> = paths
            .iter()
            .filter_map(|p| dirty_of(&self.root.path().join(p), &rel(p), false))
            .collect();
        let map = BTreeMap::from([(self.id, dirs)]);
        let res = scan_dirs(&self.db, &map, LIMITS, &AtomicBool::new(false)).unwrap();
        res.first().map_or(ScanStats::default(), |r| r.1)
    }
}

#[test]
fn rename_and_move_keep_vectors() {
    let e = env();
    e.put("a.txt", b"aaa");
    e.put("b.png", b"png");
    e.put("other.txt", b"zzz");
    e.scan();
    e.index("a.txt");
    e.index("b.png");
    let before = e.segs();

    // 同一根目录内改名、移动到子目录：整个根目录对账
    e.mv("a.txt", "a2.txt");
    e.mv("b.png", "sub/b.png");
    let s = e.scan();
    assert_eq!((s.added, s.updated, s.removed), (0, 2, 0));
    let after = e.segs();
    assert_eq!(
        after.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(),
        ["a2.txt", "sub/b.png"]
    );
    // 文件与片段 id 都不变（向量没有重算）
    assert_eq!(
        after.iter().map(|s| (s.1, s.2)).collect::<Vec<_>>(),
        before.iter().map(|s| (s.1, s.2)).collect::<Vec<_>>()
    );
    assert_eq!(e.vectors(), 4);
    assert_eq!(
        e.jobs(),
        [
            ("a2.txt".into(), "stored".into(), 0),
            ("other.txt".into(), "pending".into(), 0),
            ("sub/b.png".into(), "stored".into(), 0),
        ]
    );
    assert_eq!(e.status("a2.txt"), "stored");
    assert_eq!(
        e.count("SELECT COUNT(*) FROM fts_names WHERE fts_names MATCH '\"a2.txt\"'"),
        1
    );

    // 局部对账（监听事件的路径）：旧路径与新路径
    e.mv("a2.txt", "sub/deep/a3.txt");
    let s = e.scan_paths(&["a2.txt", "sub/deep"]);
    assert_eq!((s.added, s.updated, s.removed), (0, 1, 0));
    assert_eq!(e.segs()[0].0, "sub/b.png");
    assert_eq!(e.segs()[1].0, "sub/deep/a3.txt");
    assert_eq!(e.vectors(), 4);
}

#[test]
fn pair_by_quick_hash_without_file_id() {
    let e = env();
    e.put("a.txt", b"same size 1");
    e.put("b.txt", b"same size 2");
    e.scan();
    e.index("a.txt");
    e.index("b.txt");
    // 跨盘、FAT32 等：没有文件 ID，只能比 quick_hash
    e.exec("UPDATE files SET volume_file_id = NULL");
    e.mv("a.txt", "x/a.txt");
    // b.txt 删除后出现一个大小相同、内容不同的文件：不配对
    fs::remove_file(e.root.path().join("b.txt")).unwrap();
    e.put("c.txt", b"same size 3");
    let s = e.scan();
    assert_eq!((s.added, s.updated, s.removed), (1, 1, 1));
    assert_eq!(
        e.segs().iter().map(|s| s.0.as_str()).collect::<Vec<_>>(),
        ["x/a.txt"]
    );
    assert_eq!(
        e.jobs(),
        [
            ("c.txt".into(), "pending".into(), 0),
            ("x/a.txt".into(), "stored".into(), 0),
        ]
    );
}

#[test]
fn modify_and_delete_in_local_scope() {
    let e = env();
    e.put("sub/a.txt", b"aaa");
    e.put("sub/b.txt", b"bbb");
    e.put("c.txt", b"ccc");
    e.scan();
    e.index("sub/a.txt");
    // 修改内容：作业重置为待处理，旧向量保留到新结果写入
    e.put("sub/a.txt", b"changed");
    fs::remove_file(e.root.path().join("sub/b.txt")).unwrap();
    // 范围外的删除不处理
    fs::remove_file(e.root.path().join("c.txt")).unwrap();
    let s = e.scan_paths(&["sub/a.txt", "sub/b.txt"]);
    assert_eq!((s.added, s.updated, s.removed), (0, 1, 1));
    assert_eq!(e.names(), ["c.txt", "sub/a.txt"]);
    assert_eq!(e.jobs()[1], ("sub/a.txt".into(), "pending".into(), 0));
    // 整个目录被删除：其下的记录都消失
    e.put("sub/d/e.txt", b"e");
    e.scan();
    fs::remove_dir_all(e.root.path().join("sub")).unwrap();
    let s = e.scan_paths(&["sub"]);
    assert_eq!(s.removed, 2);
    assert!(e.names().is_empty());
}

#[test]
fn local_scope_direct_children_only() {
    let e = env();
    e.put("a.txt", b"a");
    e.put("sub/b.txt", b"b");
    e.put("sub/deep/c.txt", b"c");
    e.scan();
    // 只看根目录与 sub 的直接子项：更深处的删除不处理
    fs::remove_file(e.root.path().join("sub/deep/c.txt")).unwrap();
    fs::remove_file(e.root.path().join("sub/b.txt")).unwrap();
    e.put("sub/d.txt", b"d");
    e.put("e.txt", b"e");
    let s = e.scan_paths(&["e.txt", "sub/d.txt"]);
    assert_eq!((s.added, s.updated, s.removed), (2, 0, 1));
    assert_eq!(e.names(), ["a.txt", "e.txt", "sub/d.txt", "sub/deep/c.txt"]);
}

#[test]
fn ext_change_is_delete_and_add() {
    for to in ["a.png", "a.md"] {
        let e = env();
        e.put("a.txt", b"aaa");
        e.scan();
        e.index("a.txt");
        e.mv("a.txt", to);
        let s = e.scan();
        assert_eq!((s.added, s.updated, s.removed), (1, 0, 1));
        assert_eq!(e.vectors(), 0);
        assert_eq!(e.jobs(), [(to.into(), "pending".into(), 0)]);
    }
}

#[test]
fn move_across_roots_in_one_batch() {
    let e = env();
    let other = tempfile::tempdir().unwrap();
    let id2 = lock(&e.db)
        .add_root(&crate::normalize_root(other.path()).unwrap())
        .unwrap();
    e.put("a.txt", b"aaa");
    e.scan();
    e.index("a.txt");
    let before = e.segs();
    fs::create_dir(other.path().join("in")).unwrap();
    // 跨根目录可能跨盘，用复制加删除模拟
    fs::copy(e.root.path().join("a.txt"), other.path().join("in/a.txt")).unwrap();
    fs::remove_file(e.root.path().join("a.txt")).unwrap();
    let map = BTreeMap::from([
        (
            e.id,
            vec![dirty_of(&e.root.path().join("a.txt"), "a.txt", false).unwrap()],
        ),
        (
            id2,
            vec![dirty_of(&other.path().join("in"), "in", false).unwrap()],
        ),
    ]);
    let res = scan_dirs(&e.db, &map, LIMITS, &AtomicBool::new(false)).unwrap();
    assert_eq!(res[0].1, ScanStats::default());
    assert_eq!((res[1].1.added, res[1].1.updated), (0, 1));
    let after = e.segs();
    assert_eq!(after[0].0, "in/a.txt");
    assert_eq!((after[0].1, after[0].2), (before[0].1, before[0].2));
    assert_eq!(
        e.count(&format!(
            "SELECT root_id FROM files WHERE id = {}",
            after[0].1
        )),
        id2
    );
}

#[test]
fn dirty_paths() {
    let e = env();
    e.put("d/f.txt", b"x");
    let sep = MAIN_SEPARATOR_STR;
    let dirty = |p: &str, modified| dirty_of(&e.root.path().join(p), &rel(p), modified);
    // 文件：所在目录的直接子项
    assert_eq!(
        dirty("d/f.txt", true),
        Some(Dirty {
            rel: "d".into(),
            recursive: false
        })
    );
    // 新建或移进来的目录：整个子树；目录本身的修改不处理
    assert_eq!(
        dirty("d", false),
        Some(Dirty {
            rel: "d".into(),
            recursive: true
        })
    );
    assert_eq!(dirty("d", true), None);
    // 已不存在的路径：按子树处理
    assert_eq!(
        dirty("gone/x", true),
        Some(Dirty {
            rel: format!("gone{sep}x"),
            recursive: true
        })
    );
    let scope = [Dirty {
        rel: "d".into(),
        recursive: false,
    }];
    assert!(file_in(&scope, &rel("d/f.txt")));
    assert!(!file_in(&scope, &rel("d/e/f.txt")));
    assert!(dir_in(&scope, "d"));
    assert!(!dir_in(&scope, &rel("d/e")));
    assert!(!dir_in(&scope, "x"));
}
