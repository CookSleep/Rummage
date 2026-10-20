//! 核心状态与请求处理的测试：参数、根目录与扫描、数据库不可用
use rummage_proto::ScanStats;

use super::*;

fn open(dir: &std::path::Path) -> Arc<State> {
    State::new("t".into(), Db::open(dir))
}

#[test]
fn params_persist_across_restart() {
    let dir = crate::test_dir();
    let state = open(dir.path());
    let body = Request::SetParam {
        key: "result_limit".into(),
        value: ParamValue::Int(50),
    };
    assert_eq!(respond(body, &state), Ok(Reply::Done));
    drop(state);

    let state = open(dir.path());
    let Ok(Reply::Params { values }) = respond(Request::GetParams, &state) else {
        panic!("应返回参数");
    };
    assert_eq!(values["result_limit"], ParamValue::Int(50));
    let mut expected = params::defaults();
    expected.insert("result_limit".into(), ParamValue::Int(50));
    assert_eq!(values, expected);
}

#[test]
fn invalid_stored_params_dropped() {
    let dir = crate::test_dir();
    {
        let db = Db::open(dir.path()).unwrap();
        // 超出范围、未知参数、类型不对
        db.set_param("result_limit", &serde_json::json!(100_000))
            .unwrap();
        db.set_param("no_such_param", &serde_json::json!(1))
            .unwrap();
        db.set_param("weight_name", &serde_json::json!("x"))
            .unwrap();
    }
    let state = open(dir.path());
    assert_eq!(state.params(), params::defaults());
}

/// 值没变时直接回复：不保存、不广播 ParamsChanged、不重新扫描
#[test]
fn unchanged_param_ignored() {
    let dir = crate::test_dir();
    let state = open(dir.path());
    let mut rx = state.events.subscribe();
    state.start_scanner();
    // 等启动时排队的扫描结束
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while state.scan_busy() {
        assert!(std::time::Instant::now() < deadline, "等待扫描结束超时");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let changed = |rx: &mut broadcast::Receiver<Event>| {
        std::iter::from_fn(|| rx.try_recv().ok()).any(|e| matches!(e, Event::ParamsChanged { .. }))
    };
    changed(&mut rx);
    let set = |v| Request::SetParam {
        key: "max_doc_mb".into(),
        value: ParamValue::Int(v),
    };
    let same = params::defaults()["max_doc_mb"];
    let ParamValue::Int(same) = same else {
        panic!("max_doc_mb 是整数参数");
    };
    assert_eq!(respond(set(same), &state), Ok(Reply::Done));
    assert!(!state.scan_busy());
    assert!(!changed(&mut rx));
    assert_eq!(respond(set(same + 1), &state), Ok(Reply::Done));
    assert!(changed(&mut rx));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while Arc::strong_count(&state) > 1 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// 等到第一个 done 为 true 的 ScanProgress
fn wait_done(rx: &mut broadcast::Receiver<Event>) -> (i64, ScanStats) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        assert!(std::time::Instant::now() < deadline, "等待扫描结束超时");
        match rx.try_recv() {
            Ok(Event::ScanProgress {
                root_id,
                done: true,
                stats,
            }) => return (root_id, stats),
            Ok(_) => {}
            Err(broadcast::error::TryRecvError::Empty) => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            Err(err) => panic!("{err}"),
        }
    }
}

fn roots(state: &Arc<State>) -> Vec<RootInfo> {
    let Ok(Reply::Roots { roots }) = respond(Request::ListRoots, state) else {
        panic!("应返回根目录列表");
    };
    roots
}

#[test]
fn add_root_scans() {
    let dir = crate::test_dir();
    let docs = crate::test_dir();
    std::fs::write(docs.path().join("a.txt"), b"x").unwrap();
    std::fs::write(docs.path().join("b.png"), b"x").unwrap();
    std::fs::create_dir(docs.path().join("sub")).unwrap();
    std::fs::write(docs.path().join("sub").join("c.mp3"), b"x").unwrap();
    std::fs::write(docs.path().join("big.txt"), vec![b'x'; 2 * 1024 * 1024]).unwrap();
    let state = open(dir.path());
    let mut rx = state.events.subscribe();
    state.start_scanner();
    let path = docs.path().to_string_lossy().into_owned();
    let body = Request::AddRoot {
        path: path.clone(),
        excludes: Vec::new(),
    };
    let Ok(Reply::Root { root }) = respond(body, &state) else {
        panic!("AddRoot 应回复新建的文件夹");
    };
    let (id, stats) = wait_done(&mut rx);
    assert_eq!(stats.added, 4);
    assert_eq!(root.id, id);
    let list = roots(&state);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, id);
    assert_eq!(list[0].files, 4);
    assert!(!list[0].offline);

    let err = respond(
        Request::AddRoot {
            path,
            excludes: Vec::new(),
        },
        &state,
    )
    .unwrap_err();
    assert_eq!(err.code, ApiError::BAD_REQUEST);
    assert!(err.message.contains("已包含"), "{}", err.message);

    // 启动对账与 AddRoot 的扫描可能都扫到这个根目录，先把排队的结束事件取完再重新对账
    state.queue_scan(ScanJob::All);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let (_, stats) = wait_done(&mut rx);
        if stats == ScanStats::default() {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
    }

    // 修改大小上限后自动重新对账
    let body = Request::SetParam {
        key: "max_doc_mb".into(),
        value: ParamValue::Int(1),
    };
    assert_eq!(respond(body, &state), Ok(Reply::Done));
    // 之前排队的扫描可能还有结束事件没取，找到 big.txt 被重新判定的那一次
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while wait_done(&mut rx).1.updated != 1 {
        assert!(std::time::Instant::now() < deadline);
    }

    assert_eq!(respond(Request::RemoveRoot { id }, &state), Ok(Reply::Done));
    assert!(roots(&state).is_empty());
    let err = respond(Request::RemoveRoot { id }, &state).unwrap_err();
    assert_eq!(err.code, ApiError::BAD_REQUEST);
    // 等扫描线程释放 State，数据库关闭后临时目录才能删掉
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while Arc::strong_count(&state) > 1 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// 添加后到首次扫完之前算统计中，结束事件发出时已不算
#[test]
fn add_root_counting() {
    let dir = crate::test_dir();
    let docs = crate::test_dir();
    std::fs::write(docs.path().join("a.txt"), b"x").unwrap();
    let state = open(dir.path());
    // 代替扫描线程接收任务，由测试决定何时执行
    let (tx, jobs) = std_mpsc::channel();
    assert!(state.scan_tx.set(tx).is_ok());
    let body = Request::AddRoot {
        path: docs.path().to_string_lossy().into_owned(),
        excludes: Vec::new(),
    };
    let Ok(Reply::Root { root }) = respond(body, &state) else {
        panic!("AddRoot 应回复新建的文件夹");
    };
    assert!(root.scanning);
    assert!(roots(&state)[0].scanning);
    let Ok(Reply::Estimate(e)) = respond(Request::Estimate, &state) else {
        panic!("应返回预估");
    };
    assert!(e.roots[0].scanning);

    let mut rx = state.events.subscribe();
    scan::scan_job(
        &Arc::downgrade(&state),
        &state.events,
        jobs.try_recv().unwrap(),
    );
    assert_eq!(wait_done(&mut rx).0, root.id);
    let list = roots(&state);
    assert_eq!((list[0].files, list[0].scanning), (1, false));
    let Ok(Reply::Estimate(e)) = respond(Request::Estimate, &state) else {
        panic!("应返回预估");
    };
    assert!(!e.roots[0].scanning);

    // 其他重新对账不算统计中
    state.queue_scan(ScanJob::All);
    assert!(!roots(&state)[0].scanning);
}

#[test]
fn add_root_with_excludes() {
    let dir = crate::test_dir();
    let docs = crate::test_dir();
    let state = open(dir.path());
    let path = docs.path().to_string_lossy().into_owned();
    // 有写法不对的规则时不添加
    let body = Request::AddRoot {
        path: path.clone(),
        excludes: vec!["*.tmp".into(), "[abc".into()],
    };
    let err = respond(body, &state).unwrap_err();
    assert_eq!(err.code, ApiError::BAD_REQUEST);
    assert!(err.message.contains("[abc"), "{}", err.message);
    assert!(roots(&state).is_empty());
    let body = Request::AddRoot {
        path,
        excludes: vec!["*.tmp".into(), "node_modules/".into()],
    };
    assert!(matches!(respond(body, &state), Ok(Reply::Root { .. })));
    assert_eq!(roots(&state)[0].excludes, ["*.tmp", "node_modules"]);
}

#[test]
fn unavailable_db() {
    let dir = crate::test_dir();
    // 用普通文件占住数据目录的位置
    let blocked = dir.path().join("data");
    std::fs::write(&blocked, b"").unwrap();
    let state = open(&blocked);
    assert_eq!(respond(Request::Ping, &state), Ok(Reply::Pong));
    let err = respond(Request::GetParams, &state).unwrap_err();
    assert_eq!(err.code, ApiError::UNAVAILABLE);
    assert!(err.message.contains("不可写"), "{}", err.message);
    let body = Request::SetParam {
        key: "result_limit".into(),
        value: ParamValue::Int(50),
    };
    assert_eq!(
        respond(body, &state).unwrap_err().code,
        ApiError::UNAVAILABLE
    );
    for body in [
        Request::ListRoots,
        Request::RemoveRoot { id: 1 },
        Request::AddRoot {
            path: ".".into(),
            excludes: Vec::new(),
        },
        Request::IndexStatus,
        Request::PauseIndexing { until: None },
        Request::ResumeIndexing,
    ] {
        assert_eq!(
            respond(body, &state).unwrap_err().code,
            ApiError::UNAVAILABLE
        );
    }
    assert_eq!(state.params(), params::defaults());
}

/// 建一个根目录，扫描后把全部作业写成已完成（每个文件一个片段）
fn indexed(dir: &std::path::Path, docs: &std::path::Path) {
    let db = Mutex::new(Db::open(dir).unwrap());
    let id = lock(&db).add_root(&normalize_root(docs).unwrap()).unwrap();
    let limits = crate::TEST_LIMITS;
    rummage_index::scan_root(&db, id, limits, &AtomicBool::new(false), &mut |_| {}).unwrap();
    loop {
        // 先释放锁再写入：while let 的条件中的临时值要到循环体结束才释放
        let Some(job) = lock(&db).claim_job(true, true, true).unwrap() else {
            break;
        };
        let (kind, preprocess, mmproj, size) = if job.ext == "png" {
            ("image", "image-v1-t280", Some("p".into()), Some((640, 480)))
        } else {
            ("document", "text-v1-t64-c384-o15-m64", None, None)
        };
        let space = rummage_index::Space {
            id: kind.into(),
            family: "f".into(),
            model_sha256: "m".into(),
            mmproj_sha256: mmproj,
            runtime: "r".into(),
            backend: "cpu".into(),
            precision: "q8_0".into(),
            prefix_scheme: "A".into(),
            preprocess: preprocess.into(),
        };
        let out = rummage_index::Outcome::Stored {
            kind,
            quick_hash: vec![1],
            content_hash: None,
            space: Box::new(space),
            segments: vec![rummage_index::NewSegment {
                modality: "text",
                locator_json: "{}".into(),
                text: None,
                vector: vec![1.0, 0.0],
                coarse: vec![1.0, 0.0],
                space: None,
            }],
            size,
            meta_json: None,
            replace: None,
            more: false,
            media_failed: None,
        };
        lock(&db).apply(&[(job, out)]).unwrap();
    }
}

/// 有多模态模型、没有主模型：流水线照常运行、报告状态，不处理作业
fn start_busy(dir: &std::path::Path) -> Arc<State> {
    std::fs::write(dir.join("p.gguf"), b"x").unwrap();
    let sys = crate::resource::System {
        sense: Box::new(|_| crate::resource::Sensed {
            free_gb: Some(100.0),
            ..Default::default()
        }),
        now: Box::new(|| 0),
    };
    let state = State::with_system("t".into(), Db::open(dir), sys);
    state.start_pipeline(pipeline::Options {
        model: dir.join("无.gguf"),
        mmproj: dir.join("p.gguf"),
        selftest_cache: dir.join("cache").join("selftest.json"),
        ffmpeg: dir.join("ffmpeg"),
        embed_tmp: dir.join("tmp"),
        available: |path, _| path.is_file(),
        factory: Box::new(|_| unreachable!("没有主模型时不创建推理后端")),
        driver_events: Box::new(|_| unreachable!("没有推理后端，不会失效")),
        file_timeout: None,
        gpus: Vec::new,
        litert: false,
    });
    state
}

/// 推理替身：查询都报搜索内容过长
struct TooLong;

impl rummage_embed::EmbeddingBackend for TooLong {
    fn embed(
        &self,
        _: rummage_embed::Input,
        _: rummage_embed::Priority,
    ) -> Result<rummage_embed::Embedding, rummage_embed::Error> {
        Err(rummage_embed::Error::QueryTooLong(
            "超过 2048 个 token".into(),
        ))
    }

    fn count_tokens(
        &self,
        _: &rummage_embed::Input,
        _: rummage_embed::Priority,
    ) -> Result<usize, rummage_embed::Error> {
        Ok(1)
    }
}

#[test]
fn query_too_long_reported() {
    let dir = crate::test_dir();
    std::fs::write(dir.path().join("m.gguf"), b"x").unwrap();
    let state = State::new("t".into(), Db::open(dir.path()));
    state.start_pipeline(pipeline::Options {
        model: dir.path().join("m.gguf"),
        mmproj: dir.path().join("无.gguf"),
        selftest_cache: dir.path().join("cache").join("selftest.json"),
        ffmpeg: dir.path().join("ffmpeg"),
        embed_tmp: dir.path().join("tmp"),
        available: |path, _| path.is_file(),
        factory: Box::new(|_| Ok(Arc::new(TooLong) as Arc<dyn rummage_embed::EmbeddingBackend>)),
        driver_events: Box::new(|_| unreachable!("没有显卡引擎")),
        file_timeout: None,
        gpus: Vec::new,
        litert: false,
    });
    let body = Request::Search {
        text: Some("很长的搜索内容".into()),
        image_path: None,
        similar: None,
        kinds: Vec::new(),
        root_ids: Vec::new(),
        since: None,
        until: None,
        limit: None,
        snippets: false,
        supersede: false,
    };
    let Ok(Reply::Results {
        semantic_unavailable,
        unavailable,
        ..
    }) = respond(body, &state)
    else {
        panic!("应返回搜索结果");
    };
    // 说明是搜索内容过长，而不是笼统的“内容识别不可用”
    assert!(semantic_unavailable);
    assert_eq!(
        unavailable,
        Some(rummage_proto::SemanticUnavailable::QueryTooLong)
    );
    state.close();
}

#[test]
fn search_without_inference_skips_engine() {
    let dir = crate::test_dir();
    let docs = crate::test_dir();
    std::fs::write(docs.path().join("a.txt"), b"a").unwrap();
    indexed(dir.path(), docs.path());
    std::fs::write(dir.path().join("m.gguf"), b"x").unwrap();
    let state = State::new("t".into(), Db::open(dir.path()));
    // 只数本线程（请求处理）发起的建引擎；建不成，引擎槽一直空着，每次取引擎都会再建
    let me = std::thread::current().id();
    let builds = Arc::new(AtomicUsize::new(0));
    let counted = builds.clone();
    state.start_pipeline(pipeline::Options {
        model: dir.path().join("m.gguf"),
        mmproj: dir.path().join("无.gguf"),
        selftest_cache: dir.path().join("cache").join("selftest.json"),
        ffmpeg: dir.path().join("ffmpeg"),
        embed_tmp: dir.path().join("tmp"),
        available: |path, _| path.is_file(),
        factory: Box::new(move |_| {
            if std::thread::current().id() == me {
                counted.fetch_add(1, Ordering::Relaxed);
            }
            Err(rummage_embed::Error::Spawn("模拟启动失败".into()))
        }),
        driver_events: Box::new(|_| unreachable!("没有显卡引擎")),
        file_timeout: None,
        gpus: Vec::new,
        litert: false,
    });
    let search = |text: &str, similar| Request::Search {
        text: Some(text.into()),
        image_path: None,
        similar,
        kinds: Vec::new(),
        root_ids: Vec::new(),
        since: None,
        until: None,
        limit: None,
        snippets: false,
        supersede: false,
    };
    // 空查询与不带描述的找相似不推理，不取引擎
    assert!(respond(search(" ", None), &state).is_ok());
    let similar = rummage_proto::SimilarTo {
        file_id: 1,
        segment_id: None,
    };
    assert!(respond(search("", Some(similar)), &state).is_ok());
    assert_eq!(builds.load(Ordering::Relaxed), 0);
    assert!(respond(search("柯基", None), &state).is_ok());
    assert_eq!(builds.load(Ordering::Relaxed), 1);
    state.close();
}

#[test]
fn index_params_requeue_affected_kinds() {
    let dir = crate::test_dir();
    let docs = crate::test_dir();
    std::fs::write(docs.path().join("a.txt"), b"a").unwrap();
    std::fs::write(docs.path().join("b.png"), b"b").unwrap();
    indexed(dir.path(), docs.path());
    let state = start_busy(dir.path());
    // 按文件名排序的作业状态
    let stages = || -> Vec<String> {
        let mut list: Vec<(String, String)> = [1, 2]
            .iter()
            .map(|id| match files::info(&state, *id) {
                Ok(Reply::File(f)) => (f.name, f.job.unwrap().stage),
                other => panic!("{other:?}"),
            })
            .collect();
        list.sort();
        list.into_iter().map(|(_, stage)| stage).collect()
    };
    let rebuild = || {
        let Ok(Reply::Index(s)) = respond(Request::IndexStatus, &state) else {
            panic!("应返回索引进度");
        };
        s.rebuild
    };
    let set = |key: &str, value: ParamValue| {
        let body = Request::SetParam {
            key: key.into(),
            value,
        };
        assert_eq!(respond(body, &state), Ok(Reply::Done));
    };
    // 启动时检查：都符合当前设置
    assert_eq!(stages(), ["stored", "stored"]);
    assert_eq!(rebuild(), None);
    // 大小上限不影响已建好的索引；值没变时不重建
    set("max_doc_mb", ParamValue::Int(100));
    set("image_tokens", ParamValue::Int(280));
    assert_eq!(stages(), ["stored", "stored"]);
    // 关闭的类型不重建，重新开启时按当前设置检查
    set("index_documents", ParamValue::Bool(false));
    set("chunk_tokens", ParamValue::Int(300));
    assert_eq!(stages(), ["stored", "stored"]);
    set("index_documents", ParamValue::Bool(true));
    assert_eq!(stages(), ["pending", "stored"]);
    set("image_tokens", ParamValue::Int(560));
    assert_eq!(stages(), ["pending", "pending"]);
    let r = rebuild().unwrap();
    assert_eq!((r.done, r.total), (0, 2));
    assert_eq!(r.by_kind.len(), 2);
    // 重建期间旧片段保留，算作已完成
    let info = files::info(&state, 1);
    assert!(
        matches!(info, Ok(Reply::File(ref f)) if f.segments == 1),
        "{info:?}"
    );
    let Ok(Reply::Index(s)) = respond(Request::IndexStatus, &state) else {
        panic!("应返回索引进度");
    };
    assert_eq!((s.done, s.pending), (2, 0));
    state.close();
}

#[test]
fn excludes_rescan_root() {
    let dir = crate::test_dir();
    let docs = crate::test_dir();
    std::fs::write(docs.path().join("a.txt"), b"x").unwrap();
    std::fs::write(docs.path().join("b.tmp2"), b"x").unwrap();
    let state = open(dir.path());
    let mut rx = state.events.subscribe();
    state.start_scanner();
    let path = docs.path().to_string_lossy().into_owned();
    assert!(matches!(
        respond(
            Request::AddRoot {
                path,
                excludes: Vec::new(),
            },
            &state
        ),
        Ok(Reply::Root { .. })
    ));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while roots(&state)[0].files != 2 {
        wait_done(&mut rx);
        assert!(std::time::Instant::now() < deadline);
    }
    let id = roots(&state)[0].id;
    let body = Request::SetRootExcludes {
        id,
        excludes: vec!["*.tmp2".into()],
    };
    assert_eq!(respond(body, &state), Ok(Reply::Done));
    // 新排除的文件在重新扫描时删除
    while wait_done(&mut rx).1.removed != 1 {
        assert!(std::time::Instant::now() < deadline);
    }
    let list = roots(&state);
    assert_eq!(
        (list[0].files, list[0].excludes.clone()),
        (1, vec!["*.tmp2".to_string()])
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while Arc::strong_count(&state) > 1 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// 轮询到 `f` 为真，最多 10 秒
fn wait_until(what: &str, f: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !f() {
        assert!(std::time::Instant::now() < deadline, "等待{what}超时");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn watcher_reflects_changes() {
    let dir = crate::test_dir();
    let docs = crate::test_dir();
    let state = open(dir.path());
    let mut rx = state.events.subscribe();
    state.start_scanner();
    let path = docs.path().to_string_lossy().into_owned();
    assert!(matches!(
        respond(
            Request::AddRoot {
                path,
                excludes: Vec::new(),
            },
            &state
        ),
        Ok(Reply::Root { .. })
    ));
    wait_done(&mut rx);
    // 另开一个只读连接核对库中的文件
    let conn = rusqlite::Connection::open_with_flags(
        dir.path().join("index.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let names = || -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT rel_path FROM files ORDER BY rel_path")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };

    std::fs::create_dir(docs.path().join("sub")).unwrap();
    std::fs::write(docs.path().join("sub").join("a.txt"), b"hello").unwrap();
    let sub_a = format!("sub{}a.txt", std::path::MAIN_SEPARATOR);
    wait_until("新建的文件", || names() == [sub_a.clone()]);

    std::fs::rename(
        docs.path().join("sub").join("a.txt"),
        docs.path().join("b.txt"),
    )
    .unwrap();
    wait_until("改名", || names() == ["b.txt".to_string()]);

    std::fs::remove_file(docs.path().join("b.txt")).unwrap();
    wait_until("删除", || names().is_empty());

    drop(conn);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while Arc::strong_count(&state) > 1 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
