//! 设备检测的测试：实际生效的设备、自动检测、检测后的停用与出问题的设备、解除屏蔽后的重测、暂停中止自动检测
use super::*;

/// 写一份自检记录：CPU 与描述为 `rtx`、“GTX”的两块显卡都通过，选中 `selected`（它最快；其余按 `rtx`、GTX、CPU 由快到慢）；
/// 检测时系统中的显卡记为 `recorded`。
/// 现在系统中的显卡为 `fake_gpus`：`recorded` 与它不同就是检测后显卡有增减或更换，`rtx` 不在其中就是已拔出
pub(super) fn write_cache(dir: &Path, selected: usize, rtx: &str, recorded: &[&str]) {
    let model = format!("{}+{}", space::MODEL.sha256, space::MMPROJ.sha256);
    let key = |description: &str, driver: &str| {
        serde_json::json!({
            "description": description, "pci_id": null, "driver": driver,
            "runtime": space::RUNTIME, "model": model,
        })
    };
    let report = |i: usize, backend: &str, description: &str, driver: Option<&str>| {
        let ms = if i == selected {
            1.0
        } else {
            [9.0, 5.0, 7.0][i]
        };
        serde_json::json!({
            "backend": backend, "name": "x", "description": description, "pci_id": null,
            "driver": driver, "status": "passed", "reason": null, "samples": [],
            "init_ms": null, "transient": false, "driver_error": false, "text_ms": ms, "image_ms": ms, "audio_ms": 3.0, "video_ms": 4.0,
        })
    };
    // 键中的驱动版本与现在的驱动无关：只更新驱动不算过期
    let json = serde_json::json!({
        "cached": {
            "keys": [key("", ""), key(rtx, "1.0"), key("GTX", "1.0")],
            "report": {
                "candidates": [
                    report(0, "cpu", "", None),
                    report(1, "vulkan", rtx, Some("1.0")),
                    report(2, "vulkan", "GTX", Some("1.0")),
                ],
                "selected": selected,
                "from_cache": false,
            },
            "mmproj": true,
            "adapters": recorded,
        },
        "blacklist": [],
    });
    let path = cfg(dir).selftest_cache;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, json.to_string()).unwrap();
}

/// 检测时的显卡与现在相同
pub(super) const SAME: &[&str] = &["GTX", "RTX"];

/// 检测后拔出了一块显卡
pub(super) const REMOVED: &[&str] = &["GTX", "RTX", "旧显卡"];

#[test]
fn gpu_fallback_shown() {
    let dir = crate::test_dir();
    let state = open(dir.path());
    let set_preferred = |v: &str| {
        db(&state)
            .unwrap()
            .meta_set(PREFERRED_DEVICE_KEY, v)
            .unwrap()
    };
    // 设备列表来自缓存中最近一次的结果
    write_cache(dir.path(), 1, "RTX", SAME);
    let st = setup(&state).selftest;
    assert_eq!(st.devices[1].id, "gpu:RTX|");
    assert_eq!((st.selected, st.stale, st.failed), (Some(1), false, None));
    // 另一块显卡拔出：仍用这块卡，结果算过期（会自动重新检测）
    write_cache(dir.path(), 1, "RTX", REMOVED);
    let st = setup(&state).selftest;
    assert_eq!((st.selected, st.stale), (Some(1), true));
    // 运行中 RTX 出了明确不是驱动的问题（流水线停用它并记下）：改用剩下的设备中最快的（另一块显卡），并指出出问题的设备
    write_cache(dir.path(), 1, "RTX", SAME);
    let cache = cfg(dir.path()).selftest_cache;
    let report = embed_selftest::last_report(&cache).unwrap();
    {
        let mut d = lock(&state.disabled);
        d.keys
            .push(embed_selftest::report_key(&report.candidates[1]));
        d.failed = Some("gpu:RTX|".into());
    }
    let st = setup(&state).selftest;
    assert_eq!(
        (st.selected, st.failed.as_deref(), st.stale),
        (Some(2), Some("gpu:RTX|"), false)
    );
    // 只在本次运行停用，重启后照常使用
    assert_eq!(
        (
            st.devices[0].state,
            st.devices[1].state,
            st.devices[2].state
        ),
        (None, Some(GpuState::Disabled), Some(GpuState::Usable))
    );
    assert!(st.blocked.is_empty());
    // 之后被屏蔽：仍指出它，重启后也不再使用；不在检测结果中，只列为被屏蔽的显卡
    embed_selftest::blacklist_gpu(&cache, &report.candidates[1], "设备丢失。").unwrap();
    let st = setup(&state).selftest;
    assert_eq!(
        (st.selected, st.failed.as_deref()),
        (Some(1), Some("gpu:RTX|"))
    );
    assert_eq!(
        st.devices.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
        ["cpu", "gpu:GTX|"]
    );
    assert_eq!(st.blocked.len(), 1);
    assert_eq!(
        (st.blocked[0].id.as_str(), st.blocked[0].reason.as_deref()),
        ("gpu:RTX|", Some("设备丢失。"))
    );
    // 选了已屏蔽的显卡：按自动选择并说明
    set_preferred("gpu:RTX|");
    assert!(
        setup(&state)
            .selftest
            .preferred_error
            .unwrap()
            .contains("「RTX」已屏蔽")
    );
    set_preferred(AUTO_DEVICE);
    // 检测进行中：因屏蔽而没测的不列出；正在测的照常列出，不在已屏蔽中重复
    {
        let mut st = lock(&state.setup.selftest);
        st.running = true;
        st.devices = selftest::devices(&report).0;
        st.devices[1].status = DeviceStatus::Blacklisted;
    }
    let st = setup(&state).selftest;
    assert_eq!((st.devices.len(), st.blocked.len()), (2, 1));
    // 等待检测的也不列出，免得先显示“等待检测”又消失
    lock(&state.setup.selftest).devices[1].status = DeviceStatus::Pending;
    assert_eq!(setup(&state).selftest.devices.len(), 2);
    // 正在测的照常列出，已屏蔽的显卡中仍列出它（可以解除）
    lock(&state.setup.selftest).devices[1].status = DeviceStatus::Testing;
    let st = setup(&state).selftest;
    assert_eq!((st.devices.len(), st.blocked.len()), (3, 1));
    // 只测另一块（解除屏蔽后的重测）时，它不是本次测的：缓存中通过也不列出
    *lock(&state.setup.only) = Some("gpu:GTX|".into());
    lock(&state.setup.selftest).devices[1].status = DeviceStatus::Passed;
    assert_eq!(setup(&state).selftest.devices.len(), 2);
    lock(&state.setup.selftest).devices[1].status = DeviceStatus::Pending;
    assert_eq!(setup(&state).selftest.devices.len(), 2);
    *lock(&state.setup.only) = None;
    // 检测进行中不能解除屏蔽
    let unblock = || {
        respond(
            &state,
            Request::UnblockGpu {
                device: "gpu:RTX|".into(),
                failed: false,
            },
        )
    };
    assert!(unblock().unwrap_err().message.contains("检测正在进行"));
    *lock(&state.setup.selftest) = SelftestStatus::default();
    // 解除屏蔽：清掉它本次运行的停用；没有模型、检测没能开始时照样解除并说明原因；回到检测结果中，显示为未检测（结果过期）
    let Ok(Reply::DoneWithWarning { message }) = unblock() else {
        panic!("没能当场重新检测时应说明原因");
    };
    assert!(message.contains("请先下载或导入模型"), "{message}");
    let st = setup(&state).selftest;
    assert!(st.blocked.is_empty() && !st.running);
    assert_eq!(
        (st.devices[1].status, st.devices[1].reason.as_deref()),
        (
            DeviceStatus::Skipped,
            Some(embed_selftest::UNBLOCKED_REASON)
        )
    );
    assert_eq!(st.devices[1].state, Some(GpuState::Usable));
    assert_eq!((st.selected, st.stale), (Some(2), true));
    assert!(unblock().unwrap_err().message.contains("没有被屏蔽"));
    // 无法排除驱动原因：所有显卡都停用，改用 CPU
    write_cache(dir.path(), 1, "RTX", SAME);
    lock(&state.disabled).all = true;
    let st = setup(&state).selftest;
    assert_eq!(
        (st.selected, st.failed.as_deref(), st.gpus_off),
        (Some(0), Some("gpu:RTX|"), true)
    );
    // 用户手动检测不算自动重试，照样可以检测（此处没有黄金样本，很快出错，不初始化任何设备）
    fake_model(&cfg(dir.path()).model, &space::MODEL);
    respond(&state, Request::RunSelftest { onboarding: false }).unwrap();
    assert!(wait_selftest(&state).error.is_some());
    *lock(&state.setup.selftest) = SelftestStatus::default();
    // 用户选的显卡也不用
    set_preferred("gpu:GTX|");
    let st = setup(&state).selftest;
    assert_eq!((st.selected, st.preferred_error), (Some(0), None));
    // 用户选了 CPU：用 CPU，不指出出问题的显卡；显卡的变化不算过期
    set_preferred(CPU_DEVICE);
    let st = setup(&state).selftest;
    assert_eq!(
        (st.preferred.as_str(), st.selected, st.failed),
        (CPU_DEVICE, Some(0), None)
    );
    *lock(&state.disabled) = Default::default();
    write_cache(dir.path(), 1, "RTX", REMOVED);
    assert!(!setup(&state).selftest.stale);
    // 选了能用的显卡，即使自动选中的是 CPU
    write_cache(dir.path(), 0, "RTX", SAME);
    set_preferred("gpu:RTX|");
    let st = setup(&state).selftest;
    assert_eq!(
        (st.selected, st.stale, st.preferred_error),
        (Some(1), false, None)
    );
    // 选了的显卡仍能用、另一块拔出：照常用它，结果算过期
    write_cache(dir.path(), 0, "RTX", REMOVED);
    let st = setup(&state).selftest;
    assert_eq!(
        (st.selected, st.stale, st.preferred_error),
        (Some(1), true, None)
    );
    // 选了的显卡不在记录中：按自动选择并说明原因
    set_preferred("gpu:其他|");
    let st = setup(&state).selftest;
    assert_eq!(st.selected, Some(0));
    assert!(st.preferred_error.unwrap().contains("没有找到"));
    // 选了的显卡拔出：按自动选择；自动选中的也是它时改用剩下的设备中最快的（GTX），结果算过期
    write_cache(dir.path(), 1, "RTX2", &["GTX", "RTX2"]);
    set_preferred("gpu:RTX2|");
    let st = setup(&state).selftest;
    assert_eq!((st.selected, st.stale), (Some(2), true));
    assert!(st.preferred_error.unwrap().contains("「RTX2」目前不能使用"));
    set_preferred(AUTO_DEVICE);
    let st = setup(&state).selftest;
    assert_eq!(
        (st.selected, st.stale, st.preferred_error, st.failed),
        (Some(2), true, None, None)
    );
}

/// 有主模型（按大小判断，稀疏文件）、没有黄金样本：检测一开始就因样本无效出错，不会初始化任何设备。
/// 流水线找不到主模型（另给的路径），不创建推理后端，作业留在 pending
pub(super) fn start_with_pipeline(dir: &Path) -> Arc<State> {
    let c = cfg(dir);
    fake_model(&c.model, &space::MODEL);
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
        mmproj: dir.join("无.gguf"),
        selftest_cache: c.selftest_cache.clone(),
        ffmpeg: dir.join("ffmpeg"),
        embed_tmp: dir.join("tmp"),
        available: |path, _| path.is_file(),
        factory: Box::new(|_| unreachable!("没有主模型时不创建推理后端")),
        driver_events: Box::new(|_| unreachable!("没有推理后端，不会失效")),
        file_timeout: None,
        gpus: fake_gpus,
        litert: None,
    });
    state.configure(c);
    state
}

/// 扫描一个有文件的文件夹，作业留在 pending
pub(super) fn add_pending(state: &State, docs: &Path) {
    std::fs::write(docs.join("a.txt"), b"a").unwrap();
    let Ok(core) = &state.db else {
        panic!("数据库应可用");
    };
    let id = lock(&core.db)
        .add_root(&normalize_root(docs).unwrap())
        .unwrap();
    let limits = crate::TEST_LIMITS;
    scan_root(&core.db, id, limits, &AtomicBool::new(false), &mut |_| {}).unwrap();
}

/// 等检测结束（没有黄金样本，很快出错）
pub(super) fn wait_selftest(state: &State) -> SelftestStatus {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let st = lock(&state.setup.selftest).clone();
        if !st.running {
            return st;
        }
        assert!(std::time::Instant::now() < deadline, "等待检测结束超时");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn auto_selftest_when_stale() {
    let dir = crate::test_dir();
    let docs = crate::test_dir();
    let state = start_with_pipeline(dir.path());
    let started = || state.setup.started.load(Ordering::Relaxed);
    // 结果过期，但没有待建立索引的文件：不检测
    write_cache(dir.path(), 1, "RTX", REMOVED);
    state.auto_selftest();
    assert!(!started());
    // 有文件，但处于暂停（手动、电池、全屏等都算）：不检测
    add_pending(&state, docs.path());
    let p = state.pipeline.get().unwrap();
    p.set_paused(Some(PauseReason::Battery));
    state.auto_selftest();
    assert!(!started());
    p.set_paused(None);
    // 用户选了只用 CPU：不检测
    db(&state)
        .unwrap()
        .meta_set(PREFERRED_DEVICE_KEY, CPU_DEVICE)
        .unwrap();
    state.auto_selftest();
    assert!(!started());
    db(&state)
        .unwrap()
        .meta_delete(PREFERRED_DEVICE_KEY)
        .unwrap();
    // 结果没有过期：不检测
    write_cache(dir.path(), 1, "RTX", SAME);
    state.auto_selftest();
    assert!(!started());
    // 过期、有文件、没有暂停：开始检测，状态与手动检测相同，另标明是自动发起的；期间流水线不暂停（照常用 CPU）
    write_cache(dir.path(), 1, "RTX", REMOVED);
    state.auto_selftest();
    assert!(started());
    assert!(!p.held());
    let st = wait_selftest(&state);
    // 自动检测出错（此处为样本无效）只写日志：不写 error，界面照旧显示上次缓存的结果
    assert!(!st.auto && st.error.is_none(), "{st:?}");
    assert!(!setup(&state).selftest.devices.is_empty());
    assert!(!p.held());
    // 每次运行只自动检测一次：失败后不再发起
    state.auto_selftest();
    let st = lock(&state.setup.selftest).clone();
    assert!(!st.running && !st.auto);
    state.close();
}

#[test]
fn after_report_clears_user_tested() {
    let dir = crate::test_dir();
    write_cache(dir.path(), 1, "RTX", SAME);
    let mut report = embed_selftest::last_report(&cfg(dir.path()).selftest_cache).unwrap();
    // 候选：CPU、RTX、GTX 都通过
    let rtx = "gpu:RTX|".to_string();
    let all_off = || Disabled {
        keys: Vec::new(),
        all: true,
        failed: Some("gpu:GTX|".into()),
    };
    // 只测 RTX 且通过：只恢复它，其他显卡逐块停用；出问题的是 GTX，不清除
    let mut d = all_off();
    selftest::after_report(
        &mut d,
        &selftest::Mode::Retest(rtx.clone()),
        &report,
        None,
        &[],
        true,
    );
    assert!(!d.all);
    assert_eq!(
        d.keys.iter().map(|k| k.device_id()).collect::<Vec<_>>(),
        ["gpu:GTX|"]
    );
    assert_eq!(d.failed.as_deref(), Some("gpu:GTX|"));
    // 出问题的正是 RTX：通过后清除
    let mut d = Disabled {
        failed: Some(rtx.clone()),
        ..all_off()
    };
    selftest::after_report(
        &mut d,
        &selftest::Mode::Retest(rtx.clone()),
        &report,
        None,
        &[],
        true,
    );
    assert_eq!(d.failed, None);
    // 手动检测：停用全部清除，通过的出问题设备也清除；列显卡卡住（不保存）时不清除
    let mut d = all_off();
    selftest::after_report(&mut d, &selftest::Mode::Manual, &report, None, &[], true);
    assert!(!d.all && d.keys.is_empty() && d.failed.is_none());
    let mut d = all_off();
    selftest::after_report(&mut d, &selftest::Mode::Manual, &report, None, &[], false);
    assert!(d.all);
    // 自动检测不清除；暂时性失败的照常停用
    report.candidates[1].transient = true;
    let mut d = all_off();
    selftest::after_report(&mut d, &selftest::Mode::Auto, &report, None, &[], true);
    assert!(d.all && d.failed.is_some());
    assert_eq!(d.keys.len(), 1);
    // 只测的那块没通过：不恢复
    report.candidates[1].status = embed_selftest::Status::Failed;
    let mut d = all_off();
    selftest::after_report(
        &mut d,
        &selftest::Mode::Retest(rtx),
        &report,
        None,
        &[],
        true,
    );
    assert!(d.all);
}

#[test]
fn manual_selftest_clears_failed() {
    let dir = crate::test_dir();
    write_cache(dir.path(), 1, "RTX", SAME);
    let mut report = embed_selftest::last_report(&cfg(dir.path()).selftest_cache).unwrap();
    // 出问题的 GTX 在本次手动检测中没有通过：结果以本次为准，照样清除
    report.candidates[2].status = embed_selftest::Status::Failed;
    let gtx = || Disabled {
        failed: Some("gpu:GTX|".into()),
        ..Default::default()
    };
    let mut d = gtx();
    selftest::after_report(&mut d, &selftest::Mode::Manual, &report, None, &[], true);
    assert_eq!(d.failed, None);
    // 它此刻已被屏蔽：不清除
    let mut d = gtx();
    let blocked = ["gpu:GTX|".to_string()];
    selftest::after_report(
        &mut d,
        &selftest::Mode::Manual,
        &report,
        None,
        &blocked,
        true,
    );
    assert_eq!(d.failed.as_deref(), Some("gpu:GTX|"));
}

#[test]
fn retest_keeps_previous_gpus_disabled() {
    let dir = crate::test_dir();
    let cache = cfg(dir.path()).selftest_cache;
    // 上次检测时还有一块“旧显卡”（这次检测时已拔出）
    write_cache(dir.path(), 1, "旧显卡", SAME);
    let previous = embed_selftest::last_report(&cache).unwrap();
    write_cache(dir.path(), 1, "RTX", SAME);
    let report = embed_selftest::last_report(&cache).unwrap();
    // 所有显卡都停用时重测 RTX 通过：只恢复它，本次报告与上次报告中的其他显卡都停用，同一块不重复
    let mut d = Disabled {
        all: true,
        ..Default::default()
    };
    let rtx = selftest::Mode::Retest("gpu:RTX|".into());
    selftest::after_report(&mut d, &rtx, &report, Some(&previous), &[], true);
    assert!(!d.all);
    assert_eq!(
        d.keys.iter().map(|k| k.device_id()).collect::<Vec<_>>(),
        ["gpu:GTX|", "gpu:旧显卡|"]
    );
}

#[test]
fn unblock_retests() {
    let dir = crate::test_dir();
    let state = start_with_pipeline(dir.path());
    write_cache(dir.path(), 1, "RTX", SAME);
    let cache = cfg(dir.path()).selftest_cache;
    let report = embed_selftest::last_report(&cache).unwrap();
    embed_selftest::blacklist_gpu(&cache, &report.candidates[2], "设备丢失。").unwrap();
    // 解除后立即开始检测，按手动检测对待（不标为自动发起）
    respond(
        &state,
        Request::UnblockGpu {
            device: "gpu:GTX|".into(),
            failed: false,
        },
    )
    .unwrap();
    assert!(state.setup.started.load(Ordering::Relaxed));
    let st = wait_selftest(&state);
    // 检测出错（此处为样本无效）：说明原因，照旧显示缓存的结果，解除的显卡为未检测
    assert!(!st.auto && st.error.is_some(), "{st:?}");
    let st = setup(&state).selftest;
    assert!(st.blocked.is_empty());
    assert_eq!(st.devices.len(), 3);
    assert_eq!(st.devices[2].status, DeviceStatus::Skipped);
    assert!(!state.pipeline.get().unwrap().held());
    // 不在系统中的显卡：照样解除，不当场重测，说明原因
    write_cache(dir.path(), 1, "RTX2", &["GTX", "RTX2"]);
    let report = embed_selftest::last_report(&cache).unwrap();
    embed_selftest::blacklist_gpu(&cache, &report.candidates[1], "设备丢失。").unwrap();
    let Ok(Reply::DoneWithWarning { message }) = respond(
        &state,
        Request::UnblockGpu {
            device: "gpu:RTX2|".into(),
            failed: false,
        },
    ) else {
        panic!("不在系统中时应说明原因");
    };
    assert!(message.contains("不在系统中"), "{message}");
    assert!(!lock(&state.setup.selftest).running);
    // 重测未通过的设备只删未通过记录：只有屏蔽记录时说明没有可清除的，屏蔽照旧
    embed_selftest::blacklist_gpu(&cache, &report.candidates[1], "设备丢失。").unwrap();
    let err = respond(
        &state,
        Request::UnblockGpu {
            device: "gpu:RTX2|".into(),
            failed: true,
        },
    )
    .unwrap_err();
    assert!(err.message.contains("未通过"), "{}", err.message);
    assert_eq!(setup(&state).selftest.blocked.len(), 1);
    state.close();
}

#[test]
fn pause_stops_auto_selftest() {
    let dir = crate::test_dir();
    let state = start_with_pipeline(dir.path());
    let cancelled = || state.setup.auto_cancel.load(Ordering::Relaxed);
    // 手动检测进行中进入暂停：不中止
    *lock(&state.setup.selftest) = SelftestStatus {
        running: true,
        ..SelftestStatus::default()
    };
    state.set_manual_pause(Some(None)).unwrap();
    assert!(!cancelled());
    state.set_manual_pause(None).unwrap();
    // 自动检测进行中：没有暂停时不中止，进入暂停（手动暂停、即将睡眠等）时中止
    *lock(&state.setup.selftest) = SelftestStatus {
        running: true,
        auto: true,
        ..SelftestStatus::default()
    };
    state.refresh_pause();
    assert!(!cancelled());
    state.set_manual_pause(Some(None)).unwrap();
    assert!(cancelled());
    *lock(&state.setup.selftest) = SelftestStatus::default();
    state.close();
}
