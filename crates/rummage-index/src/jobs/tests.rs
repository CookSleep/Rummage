//! 作业的测试：领取顺序与写回、模型未就绪时跳过、失败与黑名单、过期结果、复用相同内容的向量、恢复与释放
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use super::*;
use crate::{normalize_root, scan_root};

struct Env {
    db: Mutex<Db>,
    _root: tempfile::TempDir,
    _data: tempfile::TempDir,
    id: i64,
}

fn env(files: &[(&str, &[u8])]) -> Env {
    let data = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    for (name, bytes) in files {
        std::fs::write(root.path().join(name), bytes).unwrap();
    }
    let db = Db::open(data.path()).unwrap();
    let id = db.add_root(&normalize_root(root.path()).unwrap()).unwrap();
    let db = Mutex::new(db);
    let limits = crate::scan::TEST_LIMITS;
    scan_root(&db, id, limits, &AtomicBool::new(false), &mut |_| {}).unwrap();
    Env {
        db,
        _root: root,
        _data: data,
        id,
    }
}

impl Env {
    fn db(&self) -> std::sync::MutexGuard<'_, Db> {
        self.db.lock().unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        self.db().conn.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    fn strings(&self, sql: &str) -> Vec<String> {
        let db = self.db();
        let mut stmt = db.conn.prepare(sql).unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// (stage, attempts, last_error)
    fn job(&self, name: &str) -> (String, i64, Option<String>) {
        self.db()
            .conn
            .query_row(
                "SELECT j.stage, j.attempts, j.last_error FROM jobs j JOIN files f ON f.id = j.file_id
                 WHERE f.name = ?1",
                [name],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    }
}

fn space() -> Space {
    Space {
        id: "t".into(),
        family: "f".into(),
        model_sha256: "m".into(),
        mmproj_sha256: None,
        runtime: "r".into(),
        backend: "cpu".into(),
        precision: "q8_0".into(),
        prefix_scheme: "A".into(),
        preprocess: "text-v1".into(),
    }
}

fn stored(hash: &[u8], n: usize) -> Outcome {
    Outcome::Stored {
        kind: "document",
        quick_hash: hash.to_vec(),
        content_hash: Some(hash.to_vec()),
        space: Box::new(space()),
        segments: (0..n)
            .map(|i| NewSegment {
                modality: "text",
                locator_json: "{}".into(),
                text: Some(format!("片段{i}")),
                vector: vec![1.0, 0.0],
                coarse: vec![1.0, 0.0],
                space: None,
            })
            .collect(),
        size: None,
        meta_json: None,
        replace: None,
        more: false,
        media_failed: None,
    }
}

#[test]
fn claim_order_and_store() {
    let e = env(&[("a.txt", b"a"), ("b.png", b"b")]);
    let first = e.db().claim_job(true, true, true).unwrap().unwrap();
    // 图片优先
    assert_eq!(first.name, "b.png");
    assert!(first.path.is_file() && first.path.ends_with("b.png"));
    let second = e.db().claim_job(true, true, true).unwrap().unwrap();
    assert_eq!(second.name, "a.txt");
    assert!(e.db().claim_job(true, true, true).unwrap().is_none());
    assert_eq!(e.job("a.txt").0, "preprocessing");

    e.db()
        .apply(&[(
            second.clone(),
            Outcome::Embedding {
                quick_hash: vec![1],
            },
        )])
        .unwrap();
    assert_eq!(e.job("a.txt").0, "embedding");
    e.db().apply(&[(second.clone(), stored(&[1], 2))]).unwrap();
    assert_eq!(e.job("a.txt"), ("stored".into(), 0, None));
    assert_eq!(e.count("SELECT COUNT(*) FROM segments"), 2);
    assert_eq!(
        e.count("SELECT COUNT(*) FROM vectors WHERE dim = 2 AND params_version = 'text-v1'"),
        2
    );
    assert_eq!(e.count("SELECT COUNT(*) FROM spaces"), 1);
    assert_eq!(
        e.count("SELECT COUNT(*) FROM files WHERE status = 'stored' AND quick_hash = x'01'"),
        1
    );
    // 同一结果再写一次：作业已不在处理中，丢弃
    e.db().apply(&[(second, stored(&[1], 5))]).unwrap();
    assert_eq!(e.count("SELECT COUNT(*) FROM segments"), 2);

    let counts = e.db().job_counts().unwrap();
    assert_eq!(
        counts["document"],
        JobCounts {
            total: 1,
            done: 1,
            failed: 0,
            pending: 0
        }
    );
    assert_eq!(counts["image"].pending, 1);
}

#[test]
fn images_skipped_without_mmproj() {
    let e = env(&[("a.txt", b"a"), ("b.png", b"b")]);
    assert_eq!(
        e.db().claim_job(false, false, false).unwrap().unwrap().name,
        "a.txt"
    );
    assert!(e.db().claim_job(false, false, false).unwrap().is_none());
}

#[test]
fn claimable_matches_claim_job() {
    let e = env(&[
        ("a.txt", b"a"),
        ("b.txt", b"b"),
        ("c.png", b"c"),
        ("d.txt", b"d"),
    ]);
    assert_eq!(e.db().claimable_jobs(true, true, true).unwrap(), 4);
    // 没有 mmproj 时不算图片
    assert_eq!(e.db().claimable_jobs(false, false, false).unwrap(), 3);
    // 云端文件不算
    e.db()
        .conn
        .execute("UPDATE files SET cloud = 1 WHERE name = 'd.txt'", [])
        .unwrap();
    assert_eq!(e.db().claimable_jobs(false, false, false).unwrap(), 2);
    // 处理中的算，不论种类
    let job = e.db().claim_job(true, true, true).unwrap().unwrap();
    assert_eq!(job.name, "c.png");
    assert_eq!(e.db().claimable_jobs(false, false, false).unwrap(), 3);
    // 根目录离线时只剩处理中的
    e.db()
        .conn
        .execute("UPDATE roots SET offline = 1 WHERE id = ?1", [e.id])
        .unwrap();
    assert_eq!(e.db().claimable_jobs(true, true, true).unwrap(), 1);
    assert!(e.db().claim_job(true, true, true).unwrap().is_none());
}

#[test]
fn audio_after_documents_and_needs_mmproj() {
    let e = env(&[("a.mp3", b"a"), ("b.txt", b"b"), ("c.png", b"c")]);
    // 关闭了音频识别时不算音频；没有 mmproj 时音频与图片都不领取、不计入剩余
    assert_eq!(e.db().claimable_jobs(true, false, false).unwrap(), 2);
    assert_eq!(e.db().claimable_jobs(false, true, false).unwrap(), 2);
    assert_eq!(e.db().claimable_jobs(false, false, false).unwrap(), 1);
    assert_eq!(
        e.db().claim_job(false, false, false).unwrap().unwrap().name,
        "b.txt"
    );
    assert!(e.db().claim_job(false, false, false).unwrap().is_none());
    // 有 mmproj 时图片 → 文档 → 音频，文档已领走
    assert_eq!(
        e.db().claim_job(true, true, true).unwrap().unwrap().name,
        "c.png"
    );
    assert_eq!(
        e.db().claim_job(true, true, true).unwrap().unwrap().name,
        "a.mp3"
    );
}

fn audio_space() -> Space {
    Space {
        id: "aud".into(),
        mmproj_sha256: Some("p".into()),
        preprocess: "audio-v1-s10-o2-q-60".into(),
        ..space()
    }
}

#[test]
fn audio_stored_copied_and_too_long() {
    let e = env(&[("a.mp3", b"a"), ("b.wav", b"a"), ("c.m4a", b"c")]);
    let a = e.db().claim_job(true, true, true).unwrap().unwrap();
    let b = e.db().claim_job(true, true, true).unwrap().unwrap();
    let c = e.db().claim_job(true, true, true).unwrap().unwrap();
    let meta = r#"{"duration_ms":12000,"codec":"mp3","has_gps":false}"#;
    let seg = |start: u64, end: u64| NewSegment {
        modality: "audio",
        locator_json: format!(r#"{{"start_ms":{start},"end_ms":{end}}}"#),
        text: None,
        vector: vec![0.6, 0.8],
        coarse: vec![0.6, 0.8],
        space: None,
    };
    let stored = Outcome::Stored {
        kind: "audio",
        quick_hash: vec![1],
        content_hash: Some(vec![7]),
        space: Box::new(audio_space()),
        segments: vec![seg(0, 10_000), seg(2_000, 12_000)],
        size: None,
        meta_json: Some(meta.into()),
        replace: None,
        more: false,
        media_failed: None,
    };
    e.db().apply(&[(a.clone(), stored)]).unwrap();
    let meta_of = |name: &str| {
        e.strings(&format!(
            "SELECT meta_json FROM files WHERE name = '{name}'"
        ))[0]
            .clone()
    };
    assert_eq!(meta_of(&a.name), meta);
    // 内容相同的音频：同一模型的空间才能复制（后端可以不同），复制片段、向量与元数据
    let db = e.db();
    let aud = audio_space();
    let gpu = Space {
        id: "aud-gpu".into(),
        backend: "gpu".into(),
        ..audio_space()
    };
    let other = Space {
        id: "other".into(),
        model_sha256: "m2".into(),
        ..audio_space()
    };
    assert_eq!(
        db.media_copy_source(&[7], &aud, b.file_id).unwrap(),
        Some(a.file_id)
    );
    assert_eq!(
        db.media_copy_source(&[7], &gpu, b.file_id).unwrap(),
        Some(a.file_id)
    );
    assert_eq!(db.media_copy_source(&[7], &other, b.file_id).unwrap(), None);
    assert_eq!(db.media_copy_source(&[8], &aud, b.file_id).unwrap(), None);
    assert_eq!(db.media_copy_source(&[7], &aud, a.file_id).unwrap(), None);
    drop(db);
    let copy = Outcome::Copied {
        kind: "audio",
        quick_hash: vec![1],
        content_hash: vec![7],
        space_id: "aud".into(),
        slice_version: String::new(),
        from: a.file_id,
    };
    assert!(!e.db().apply(&[(b.clone(), copy)]).unwrap());
    assert_eq!(e.job(&b.name).0, "stored");
    assert_eq!(meta_of(&b.name), meta);
    assert_eq!(
        e.count(&format!(
            "SELECT COUNT(*) FROM segments s JOIN vectors v ON v.segment_id = s.id
             WHERE s.file_id = {} AND s.modality = 'audio' AND v.params_version = 'audio-v1-s10-o2-q-60'",
            b.file_id
        )),
        2
    );
    assert_eq!(
        e.strings(&format!("SELECT kind FROM files WHERE id = {}", b.file_id)),
        ["audio"]
    );
    // 超过时长上限：只记元数据，删除作业，状态 too_large
    let long = r#"{"duration_ms":20000000,"has_gps":false}"#;
    e.db()
        .apply(&[(
            c.clone(),
            Outcome::TooLong {
                quick_hash: vec![3],
                meta_json: long.into(),
            },
        )])
        .unwrap();
    assert_eq!(meta_of(&c.name), long);
    assert_eq!(
        e.strings(&format!(
            "SELECT status FROM files WHERE id = {}",
            c.file_id
        )),
        ["too_large"]
    );
    assert_eq!(
        e.count(&format!(
            "SELECT COUNT(*) FROM jobs WHERE file_id = {}",
            c.file_id
        )),
        0
    );
}

#[test]
fn failures_then_blacklist() {
    let e = env(&[("a.txt", b"a")]);
    for i in 1..=3 {
        let job = e.db().claim_job(true, true, true).unwrap().unwrap();
        let requeued = e
            .db()
            .apply(&[(
                job,
                Outcome::Failed {
                    quick_hash: Some(vec![9]),
                    error: "坏了。".into(),
                },
            )])
            .unwrap();
        assert_eq!(requeued, i < 3);
    }
    assert_eq!(e.job("a.txt"), ("failed".into(), 3, Some("坏了。".into())));
    assert!(e.db().is_blacklisted(&[9]).unwrap());
    assert!(!e.db().is_blacklisted(&[8]).unwrap());
    assert!(e.db().claim_job(true, true, true).unwrap().is_none());
    assert_eq!(e.db().job_counts().unwrap()["document"].failed, 1);
}

/// 黑名单只在 `drop_unused_blacklist` 时清理：移动后的文件（删旧行加新行）重新算出相同的 `quick_hash` 时保留，
/// 已没有文件使用时删除
#[test]
fn blacklist_dropped_with_last_file() {
    // b.bin 只按文件名索引，没有作业
    let e = env(&[("a.txt", b"a"), ("b.bin", b"b")]);
    for _ in 0..MAX_ATTEMPTS {
        let job = e.db().claim_job(true, true, true).unwrap().unwrap();
        let failed = Outcome::Failed {
            quick_hash: Some(vec![9]),
            error: "坏了。".into(),
        };
        e.db().apply(&[(job, failed)]).unwrap();
    }
    assert!(e.db().is_blacklisted(&[9]).unwrap());
    // a.txt 被移动：旧行删除，删除本身不动黑名单
    e.db()
        .conn
        .execute("DELETE FROM files WHERE name = 'a.txt'", [])
        .unwrap();
    assert!(e.db().is_blacklisted(&[9]).unwrap());
    // 新位置的文件（这里借用 b.bin 的行）重新算出相同的 quick_hash：仍有文件使用，清理时保留
    e.db()
        .conn
        .execute(
            "UPDATE files SET quick_hash = x'09' WHERE name = 'b.bin'",
            [],
        )
        .unwrap();
    assert_eq!(e.db().drop_unused_blacklist().unwrap(), 0);
    assert!(e.db().is_blacklisted(&[9]).unwrap());
    // 内容变化，扫描清空 quick_hash：扫描本身不移出（只改修改时间的文件仍要命中），清理时已没有文件使用，删除
    std::fs::write(e._root.path().join("b.bin"), b"changed").unwrap();
    let limits = crate::scan::TEST_LIMITS;
    scan_root(&e.db, e.id, limits, &AtomicBool::new(false), &mut |_| {}).unwrap();
    assert!(e.db().is_blacklisted(&[9]).unwrap());
    assert_eq!(e.db().drop_unused_blacklist().unwrap(), 1);
    assert_eq!(e.count("SELECT COUNT(*) FROM blacklist"), 0);
}

#[test]
fn stale_results_dropped() {
    let e = env(&[("a.txt", b"a"), ("b.txt", b"b")]);
    let a = e.db().claim_job(true, true, true).unwrap().unwrap();
    let b = e.db().claim_job(true, true, true).unwrap().unwrap();
    // 处理期间文件变化（扫描已重置作业）、根目录被移除
    e.db()
        .conn
        .execute("UPDATE files SET size = 99 WHERE id = ?1", [a.file_id])
        .unwrap();
    e.db().apply(&[(a, stored(&[1], 1))]).unwrap();
    assert_eq!(e.count("SELECT COUNT(*) FROM segments"), 0);
    e.db().remove_root(e.id).unwrap();
    e.db().apply(&[(b, stored(&[2], 1))]).unwrap();
    assert_eq!(e.count("SELECT COUNT(*) FROM segments"), 0);
    assert_eq!(e.count("SELECT COUNT(*) FROM spaces"), 0);
}

#[test]
fn copy_from_same_content() {
    let e = env(&[("a.png", b"a"), ("b.png", b"a")]);
    let a = e.db().claim_job(true, true, true).unwrap().unwrap();
    let b = e.db().claim_job(true, true, true).unwrap().unwrap();
    let img = Outcome::Stored {
        kind: "image",
        quick_hash: vec![1],
        content_hash: Some(vec![7]),
        space: Box::new(Space {
            id: "img".into(),
            preprocess: "image-v1-t140".into(),
            ..space()
        }),
        segments: vec![NewSegment {
            modality: "image",
            locator_json: "{}".into(),
            text: None,
            vector: vec![0.6, 0.8],
            coarse: vec![0.6, 0.8],
            space: None,
        }],
        size: Some((640, 480)),
        meta_json: None,
        replace: None,
        more: false,
        media_failed: None,
    };
    e.db().apply(&[(a.clone(), img)]).unwrap();
    let img = Space {
        id: "img".into(),
        preprocess: "image-v1-t140".into(),
        ..space()
    };
    let source = |space: &Space, file| e.db().copy_source(&[7], space, "sv", 2.5, file).unwrap();
    assert_eq!(source(&img, b.file_id), Some(a.file_id));
    // 只是后端不同的空间也能复制；模型或档位不同的不能
    let vulkan = Space {
        id: "img-vulkan".into(),
        backend: "vulkan".into(),
        ..img.clone()
    };
    assert_eq!(source(&vulkan, b.file_id), Some(a.file_id));
    for other in [
        Space {
            id: "other".into(),
            model_sha256: "m2".into(),
            ..img.clone()
        },
        Space {
            id: "img280".into(),
            preprocess: "image-v1-t280".into(),
            ..img.clone()
        },
    ] {
        assert_eq!(source(&other, b.file_id), None);
    }
    assert_eq!(source(&img, a.file_id), None);
    let copy = Outcome::Copied {
        kind: "image",
        quick_hash: vec![1],
        content_hash: vec![7],
        space_id: "img".into(),
        slice_version: "sv".into(),
        from: a.file_id,
    };
    // 目标空间不同、内容哈希不同：放回 pending
    for (space_id, hash) in [("other", vec![7]), ("img", vec![8])] {
        let wrong = Outcome::Copied {
            kind: "image",
            quick_hash: vec![1],
            content_hash: hash,
            space_id: space_id.into(),
            slice_version: "sv".into(),
            from: a.file_id,
        };
        assert!(e.db().apply(&[(b.clone(), wrong)]).unwrap());
        assert_eq!(e.job(&b.name).0, "pending");
        e.db()
            .conn
            .execute(
                "UPDATE jobs SET stage = 'preprocessing' WHERE file_id = ?1",
                [b.file_id],
            )
            .unwrap();
    }
    e.db().apply(&[(b.clone(), copy.clone())]).unwrap();
    assert_eq!(
        e.count("SELECT COUNT(*) FROM vectors WHERE params_version = 'image-v1-t140'"),
        2
    );
    // 复制来的片段带本次写入的时间（详情栏的索引时间）
    assert_eq!(
        e.count(&format!(
            "SELECT COUNT(*) FROM segments WHERE file_id = {} AND created_at > 0",
            b.file_id
        )),
        1
    );
    // 宽高随片段一起复制
    assert_eq!(
        e.count("SELECT COUNT(*) FROM files WHERE width = 640 AND height = 480"),
        2
    );
    assert_eq!(e.job(&b.name).0, "stored");

    // 来源已没有向量：放回 pending，不计失败
    e.db().conn.execute("DELETE FROM segments", []).unwrap();
    e.db()
        .conn
        .execute("UPDATE jobs SET stage = 'preprocessing'", [])
        .unwrap();
    assert!(e.db().apply(&[(b.clone(), copy)]).unwrap());
    assert_eq!(e.job(&b.name), ("pending".into(), 0, None));
}

/// 长图：来源须有当前参数的切片才能复制；复制时带上切片及其空间与参数版本，旧参数的切片不复制
#[test]
fn copy_long_image_with_slices() {
    let e = env(&[("a.png", b"a"), ("b.png", b"a")]);
    let a = e.db().claim_job(true, true, true).unwrap().unwrap();
    let b = e.db().claim_job(true, true, true).unwrap().unwrap();
    let img = Space {
        id: "img".into(),
        preprocess: "image-v1-t140".into(),
        ..space()
    };
    let seg = |locator: &str, slice: Option<(&str, &str)>| NewSegment {
        modality: "image",
        locator_json: locator.into(),
        text: None,
        vector: vec![0.6, 0.8],
        coarse: vec![0.6, 0.8],
        space: slice.map(|(id, version)| {
            Box::new((
                Space {
                    id: id.into(),
                    preprocess: "image-v1-t560".into(),
                    ..space()
                },
                version.to_string(),
            ))
        }),
    };
    let stored = |segments| Outcome::Stored {
        kind: "image",
        quick_hash: vec![1],
        content_hash: Some(vec![7]),
        space: Box::new(img.clone()),
        segments,
        size: Some((1000, 3000)),
        meta_json: None,
        replace: None,
        more: false,
        media_failed: None,
    };
    let reset = || {
        e.db()
            .conn
            .execute("UPDATE jobs SET stage = 'preprocessing'", [])
            .unwrap()
    };
    let source = |ratio| {
        e.db()
            .copy_source(&[7], &img, "sv1", ratio, b.file_id)
            .unwrap()
    };
    // 只有整图向量：按 2.5 是长图，不能复制；按 3.5 不是长图，可以
    e.db()
        .apply(&[(a.clone(), stored(vec![seg("{}", None)]))])
        .unwrap();
    assert_eq!(source(2.5), None);
    assert_eq!(source(3.5), Some(a.file_id));
    reset();
    let segs = vec![
        seg("{}", None),
        seg(r#"{"region":0}"#, Some(("img560", "sv1"))),
        seg(r#"{"region":1}"#, Some(("img560", "sv1"))),
    ];
    e.db().apply(&[(a.clone(), stored(segs))]).unwrap();
    assert_eq!(
        e.strings("SELECT space_id || ' ' || params_version FROM vectors ORDER BY rowid"),
        ["img image-v1-t140", "img560 sv1", "img560 sv1"]
    );
    assert_eq!(source(2.5), Some(a.file_id));
    assert_eq!(
        e.db()
            .copy_source(&[7], &img, "sv0", 2.5, b.file_id)
            .unwrap(),
        None
    );

    let copy = |version: &str| Outcome::Copied {
        kind: "image",
        quick_hash: vec![1],
        content_hash: vec![7],
        space_id: "img".into(),
        slice_version: version.into(),
        from: a.file_id,
    };
    let copied = || {
        e.strings(&format!(
            "SELECT s.locator_json || ' ' || v.space_id || ' ' || v.params_version
             FROM segments s JOIN vectors v ON v.segment_id = s.id
             WHERE s.file_id = {} ORDER BY s.id",
            b.file_id
        ))
    };
    e.db().apply(&[(b.clone(), copy("sv1"))]).unwrap();
    assert_eq!(
        copied(),
        [
            "{} img image-v1-t140",
            r#"{"region":0} img560 sv1"#,
            r#"{"region":1} img560 sv1"#
        ]
    );
    // 切片参数已变：只复制整图
    reset();
    e.db().apply(&[(b.clone(), copy("sv0"))]).unwrap();
    assert_eq!(copied(), ["{} img image-v1-t140"]);
    assert_eq!(e.job(&b.name).0, "stored");
    // 参数版本相同、但属于另一个模型的切片不复制
    reset();
    let other = NewSegment {
        space: Some(Box::new((
            Space {
                id: "npu560".into(),
                model_sha256: "m2".into(),
                preprocess: "image-v1-t560".into(),
                ..space()
            },
            "sv1".to_string(),
        ))),
        ..seg(r#"{"region":1}"#, None)
    };
    let segs = vec![
        seg("{}", None),
        seg(r#"{"region":0}"#, Some(("img560", "sv1"))),
        other,
    ];
    e.db().apply(&[(a.clone(), stored(segs))]).unwrap();
    reset();
    e.db().apply(&[(b.clone(), copy("sv1"))]).unwrap();
    assert_eq!(
        copied(),
        ["{} img image-v1-t140", r#"{"region":0} img560 sv1"#]
    );
}

#[test]
fn released_not_counted() {
    let e = env(&[("a.txt", b"a")]);
    let a = e.db().claim_job(true, true, true).unwrap().unwrap();
    assert!(e.db().apply(&[(a, Outcome::Released)]).unwrap());
    assert_eq!(e.job("a.txt"), ("pending".into(), 0, None));
}

#[test]
fn unfinished_jobs() {
    let e = env(&[("a.txt", b"a"), ("b.txt", b"b")]);
    assert!(e.db().has_unfinished_jobs().unwrap());
    let a = e.db().claim_job(true, true, true).unwrap().unwrap();
    let b = e.db().claim_job(true, true, true).unwrap().unwrap();
    e.db().apply(&[(a, stored(&[1], 1))]).unwrap();
    // 处理中的也算未完成
    assert!(e.db().has_unfinished_jobs().unwrap());
    let failed = Outcome::Failed {
        quick_hash: None,
        error: "坏了。".into(),
    };
    e.db()
        .conn
        .execute(
            "UPDATE jobs SET attempts = 2 WHERE file_id = ?1",
            [b.file_id],
        )
        .unwrap();
    e.db().apply(&[(b, failed)]).unwrap();
    assert!(!e.db().has_unfinished_jobs().unwrap());
}

#[test]
fn recover_and_release() {
    let e = env(&[("a.txt", b"a"), ("b.txt", b"b"), ("c.txt", b"c")]);
    let a = e.db().claim_job(true, true, true).unwrap().unwrap();
    e.db()
        .apply(&[(
            a,
            Outcome::Embedding {
                quick_hash: vec![5],
            },
        )])
        .unwrap();
    e.db().claim_job(true, true, true).unwrap().unwrap();
    let interrupted = || {
        e.db()
            .conn
            .execute(
                "UPDATE jobs SET stage = 'embedding', updated_at = updated_at - 5000 WHERE stage = 'pending'",
                [],
            )
            .unwrap()
    };
    // 不是核心自己崩溃（被系统结束等）：放回、不计失败；按最早的作业开始处理至今的时长查
    let mut window = Duration::ZERO;
    let n = e
        .db()
        .recover_jobs(|w| {
            window = w;
            false
        })
        .unwrap();
    assert_eq!(n, (2, false));
    assert!(window >= Duration::from_secs(0) && window < Duration::from_secs(5));
    assert_eq!(
        e.count("SELECT COUNT(*) FROM jobs WHERE stage = 'pending' AND attempts = 0"),
        3
    );
    // 没有中断的作业时不查
    assert_eq!(e.db().recover_jobs(|_| unreachable!()).unwrap(), (0, false));
    // 同一个文件连续两次在中断时都在处理：即使不是崩溃也计一次失败；上次没有中断的（第三个文件）只记下
    e.db()
        .conn
        .execute("UPDATE jobs SET stage = 'embedding'", [])
        .unwrap();
    assert_eq!(e.db().recover_jobs(|_| false).unwrap(), (3, false));
    assert_eq!(
        e.count("SELECT COUNT(*) FROM jobs WHERE stage = 'pending' AND attempts = 1"),
        2
    );
    assert_eq!(
        e.count("SELECT COUNT(*) FROM jobs WHERE stage = 'pending' AND attempts = 0 AND last_error IS NOT NULL"),
        1
    );
    e.db()
        .conn
        .execute("UPDATE jobs SET attempts = 0, last_error = NULL", [])
        .unwrap();
    // 核心崩溃：各计一次失败
    interrupted();
    e.db()
        .conn
        .execute(
            "UPDATE jobs SET stage = 'pending' WHERE file_id = (SELECT MAX(file_id) FROM jobs)",
            [],
        )
        .unwrap();
    assert_eq!(
        e.db()
            .recover_jobs(|w| w >= Duration::from_secs(5))
            .unwrap(),
        (2, true)
    );
    assert_eq!(
        e.count("SELECT COUNT(*) FROM jobs WHERE stage = 'pending' AND attempts = 1"),
        2
    );
    // 第三次崩溃：失败，已知 quick_hash 的进黑名单
    e.db()
        .conn
        .execute("UPDATE jobs SET stage = 'embedding', attempts = 2", [])
        .unwrap();
    assert_eq!(e.db().recover_jobs(|_| true).unwrap(), (3, true));
    assert_eq!(
        e.count("SELECT COUNT(*) FROM jobs WHERE stage = 'failed'"),
        3
    );
    assert_eq!(e.count("SELECT COUNT(*) FROM blacklist"), 1);
    assert!(e.db().is_blacklisted(&[5]).unwrap());

    e.db()
        .conn
        .execute("UPDATE jobs SET stage = 'preprocessing', attempts = 0", [])
        .unwrap();
    e.db().release_jobs().unwrap();
    assert_eq!(
        e.count("SELECT COUNT(*) FROM jobs WHERE stage = 'pending' AND attempts = 0"),
        3
    );
}

#[test]
fn skip_video_not_video_encrypted() {
    let e = env(&[
        ("a.mp4", b"a"),
        ("b.ts", b"b"),
        ("c.ts", b"c"),
        ("d.pdf", b"d"),
    ]);
    // 跳过的作业（如不做语义索引的种类）删除作业，状态不变
    let mut out = Vec::new();
    while let Some(job) = e.db().claim_job(true, true, true).unwrap() {
        let o = match job.name.as_str() {
            "a.mp4" => Outcome::Skipped,
            "b.ts" => Outcome::Video,
            "d.pdf" => Outcome::Encrypted {
                quick_hash: vec![4],
            },
            _ => Outcome::NotVideo {
                quick_hash: vec![3],
            },
        };
        out.push((job, o));
    }
    e.db().apply(&out).unwrap();
    assert_eq!(e.count("SELECT COUNT(*) FROM jobs"), 1);
    assert_eq!(
        e.count("SELECT COUNT(*) FROM files WHERE name = 'a.mp4' AND status = 'pending'"),
        1
    );
    // 加密：直接 failed，不计次数、不进黑名单
    assert_eq!(e.job("d.pdf"), ("failed".into(), 0, Some(ENCRYPTED.into())));
    assert_eq!(e.count("SELECT COUNT(*) FROM blacklist"), 0);
    assert_eq!(
        e.count("SELECT COUNT(*) FROM files WHERE name = 'b.ts' AND kind = 'video'"),
        1
    );
    assert_eq!(
        e.count(
            "SELECT COUNT(*) FROM files WHERE name = 'c.ts' AND kind = 'other' AND quick_hash IS NOT NULL"
        ),
        1
    );
}
