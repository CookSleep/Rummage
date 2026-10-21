//! 显卡屏蔽与本次运行停用的测试：解除屏蔽后只重测一块时其余设备照缓存显示、解除只影响这一块、不能当场重测的各种情况、
//! 检测结果中哪些已屏蔽的显卡列出、显卡现在的情况、过期判断、检测后的停用与出问题的设备、自动检测的拦截
use super::devices::{SAME, add_pending, start_with_pipeline, wait_selftest, write_cache};
use super::*;
use rummage_proto::DeviceResult;

/// 改写 `write_cache` 写好的缓存文件
fn edit_cache(dir: &Path, f: impl FnOnce(&mut serde_json::Value)) {
    let path = cfg(dir).selftest_cache;
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    f(&mut json);
    std::fs::write(path, json.to_string()).unwrap();
}

/// 屏蔽缓存报告中的第 `i` 个候选，检测时的驱动版本记为 `driver`
fn block(dir: &Path, i: usize, driver: &str) {
    let cache = cfg(dir).selftest_cache;
    let mut r = embed_selftest::last_report(&cache).unwrap().candidates[i].clone();
    r.driver = Some(driver.to_string());
    embed_selftest::blacklist_gpu(&cache, &r, &format!("驱动 {driver} 时设备丢失。")).unwrap();
}

/// 缓存文件中的屏蔽与未通过记录：（描述、驱动版本、种类）
fn records(dir: &Path) -> Vec<(String, String, String)> {
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(cfg(dir).selftest_cache).unwrap()).unwrap();
    json["blacklist"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            let s = |v: &serde_json::Value| v.as_str().unwrap().to_string();
            (
                s(&b["key"]["description"]),
                s(&b["key"]["driver"]),
                s(&b["kind"]),
            )
        })
        .collect()
}

fn unblock(state: &Arc<State>, device: &str) -> Result<Reply, ApiError> {
    respond(
        state,
        Request::UnblockGpu {
            device: device.into(),
            failed: false,
        },
    )
}

/// 检测结果中各设备的标识
fn ids(st: &SelftestStatus) -> Vec<&str> {
    st.devices.iter().map(|d| d.id.as_str()).collect()
}

fn disabled_ids(d: &Disabled) -> Vec<String> {
    d.keys.iter().map(|k| k.device_id()).collect()
}

#[test]
fn retest_error_keeps_others_as_cached() {
    let dir = crate::test_dir();
    let state = start_with_pipeline(dir.path());
    write_cache(dir.path(), 1, "RTX", SAME);
    edit_cache(dir.path(), |j| {
        let c = &mut j["cached"]["report"]["candidates"];
        c[0]["reason"] = "CPU 的说明。".into();
        c[1]["reason"] = "RTX 的说明。".into();
    });
    let cache = cfg(dir.path()).selftest_cache;
    let before = embed_selftest::last_report(&cache).unwrap();
    block(dir.path(), 2, "1.0");
    // 解除 GTX 的屏蔽，随即只重测它（此处没有黄金样本，检测出错）
    unblock(&state, "gpu:GTX|").unwrap();
    assert_eq!(lock(&state.setup.only).as_deref(), Some("gpu:GTX|"));
    assert!(wait_selftest(&state).error.is_some());
    // 其余设备的状态、耗时与原因与上次缓存完全相同，顺序不变；只有 GTX 为未检测
    let st = setup(&state).selftest;
    assert_eq!(ids(&st), ["cpu", "gpu:RTX|", "gpu:GTX|"]);
    let shown: Vec<DeviceResult> = st
        .devices
        .iter()
        .map(|d| DeviceResult {
            state: None,
            ..d.clone()
        })
        .collect();
    let want = selftest::devices(&before).0;
    assert_eq!(shown[..2], want[..2]);
    let gtx = &st.devices[2];
    assert_eq!(
        (gtx.status, gtx.reason.as_deref()),
        (
            DeviceStatus::Skipped,
            Some(embed_selftest::UNBLOCKED_REASON)
        )
    );
    assert_eq!(
        (gtx.text_ms, gtx.image_ms, gtx.audio_ms, gtx.video_ms),
        (None, None, None, None)
    );
    assert_eq!(st.selected, Some(1));
    // 缓存中也只有它改为未检测
    let after = embed_selftest::last_report(&cache).unwrap();
    assert_eq!(after.candidates[..2], before.candidates[..2]);
    assert_eq!(after.candidates[2].status, embed_selftest::Status::Skipped);
    state.close();
}

#[test]
fn blocked_listed_only_when_tested_and_failed() {
    let dir = crate::test_dir();
    let state = open(dir.path());
    write_cache(dir.path(), 1, "RTX", SAME);
    let report = embed_selftest::last_report(&cfg(dir.path()).selftest_cache).unwrap();
    block(dir.path(), 1, "1.0");
    // 本次运行中检测完成（不在进行中）、RTX 已屏蔽，它在本次结果中为 `status`；`only` 为只测的设备
    let listed = |status: DeviceStatus, only: Option<&str>| {
        *lock(&state.setup.selftest) = SelftestStatus {
            devices: selftest::devices(&report).0,
            ..SelftestStatus::default()
        };
        lock(&state.setup.selftest).devices[1].status = status;
        *lock(&state.setup.only) = only.map(str::to_string);
        let st = setup(&state).selftest;
        // 已屏蔽的显卡中始终列出（可以解除）
        assert_eq!(st.blocked.len(), 1);
        let d = st.devices.iter().find(|d| d.id == "gpu:RTX|");
        // 列出时标为已屏蔽，不说成可用
        assert!(d.is_none_or(|d| d.state == Some(GpuState::Blocked)));
        d.is_some()
    };
    // 手动检测中真正测了它、未通过：列出，看得到原因
    assert!(listed(DeviceStatus::Failed, None));
    // 因屏蔽没测、或通过后才在运行中被屏蔽：不列出
    assert!(!listed(DeviceStatus::Blacklisted, None));
    assert!(!listed(DeviceStatus::Passed, None));
    // 只测另一块时，它的结果不是本次测的：不列出；只测它时列出
    assert!(!listed(DeviceStatus::Failed, Some("gpu:GTX|")));
    assert!(listed(DeviceStatus::Failed, Some("gpu:RTX|")));
}

#[test]
fn gpu_state_absent_over_disabled() {
    let dir = crate::test_dir();
    let state = open(dir.path());
    // RTX2 检测时在、现在不在系统中
    write_cache(dir.path(), 1, "RTX2", &["GTX", "RTX2"]);
    let report = embed_selftest::last_report(&cfg(dir.path()).selftest_cache).unwrap();
    lock(&state.disabled)
        .keys
        .push(embed_selftest::report_key(&report.candidates[1]));
    let states = || {
        let st = setup(&state).selftest;
        st.devices.iter().map(|d| d.state).collect::<Vec<_>>()
    };
    // 不在系统中优先于本次运行停用；CPU 没有这一项
    assert_eq!(
        states(),
        [None, Some(GpuState::Absent), Some(GpuState::Usable)]
    );
    // 所有显卡都停用：在系统中的显卡都为停用
    lock(&state.disabled).all = true;
    assert_eq!(
        states(),
        [None, Some(GpuState::Absent), Some(GpuState::Disabled)]
    );
    // 只停用 GTX
    *lock(&state.disabled) = Disabled {
        keys: vec![embed_selftest::report_key(&report.candidates[2])],
        ..Default::default()
    };
    assert_eq!(
        states(),
        [None, Some(GpuState::Absent), Some(GpuState::Disabled)]
    );
}

#[test]
fn stale_retry_only_before_tested() {
    let dir = crate::test_dir();
    let state = open(dir.path());
    let transient = |recorded: &[&str]| {
        write_cache(dir.path(), 1, "RTX", recorded);
        edit_cache(dir.path(), |j| {
            let gtx = &mut j["cached"]["report"]["candidates"][2];
            gtx["status"] = "failed".into();
            gtx["transient"] = true.into();
        });
    };
    // 缓存中有暂时性失败的显卡：本次运行还没检测过时算过期（下次启动重测）
    transient(SAME);
    assert!(setup(&state).selftest.stale);
    // 本次运行中检测过：它下次启动才重测，现在不算过期
    state.setup.tested.store(true, Ordering::Relaxed);
    assert!(!setup(&state).selftest.stale);
    // 显卡有增减仍算过期
    transient(&["GTX", "RTX", "旧显卡"]);
    assert!(setup(&state).selftest.stale);
    // 用户选了只用 CPU：都不算
    state.setup.tested.store(false, Ordering::Relaxed);
    transient(SAME);
    db(&state)
        .unwrap()
        .meta_set(PREFERRED_DEVICE_KEY, CPU_DEVICE)
        .unwrap();
    assert!(!setup(&state).selftest.stale);
}

#[test]
fn unblock_affects_only_this_gpu() {
    let dir = crate::test_dir();
    let state = open(dir.path());
    write_cache(dir.path(), 1, "RTX", SAME);
    let report = embed_selftest::last_report(&cfg(dir.path()).selftest_cache).unwrap();
    // GTX 在两个驱动版本下都有屏蔽记录，另有一条未通过记录；RTX 也被屏蔽
    block(dir.path(), 2, "1.0");
    block(dir.path(), 2, "2.0");
    block(dir.path(), 1, "1.0");
    edit_cache(dir.path(), |j| {
        let mut failed = j["blacklist"][0].clone();
        failed["kind"] = "failed".into();
        failed["key"]["driver"] = "0.9".into();
        j["blacklist"].as_array_mut().unwrap().push(failed);
    });
    let key = |i: usize| embed_selftest::report_key(&report.candidates[i]);
    *lock(&state.disabled) = Disabled {
        keys: vec![key(2), key(1)],
        all: true,
        failed: Some("gpu:GTX|".into()),
    };
    // 没有模型、检测没能开始：照样解除并说明原因，不登记检测已开始
    let Ok(Reply::DoneWithWarning { message }) = unblock(&state, "gpu:GTX|") else {
        panic!("没能当场重新检测时应说明原因");
    };
    assert!(message.contains("请先下载或导入模型"), "{message}");
    assert!(!state.setup.started.load(Ordering::Relaxed));
    // GTX 的屏蔽记录不论驱动版本全部删除，未通过记录与 RTX 的记录不动
    let mut recs = records(dir.path());
    recs.sort();
    let rec = |d: &str, v: &str, k: &str| (d.to_string(), v.to_string(), k.to_string());
    assert_eq!(
        recs,
        [
            rec("GTX", "0.9", "failed"),
            rec("RTX", "1.0", "blacklisted")
        ]
    );
    // 只清掉 GTX 本次运行的停用；所有显卡都停用的状态与出问题的设备保留，到重测通过为止
    {
        let d = lock(&state.disabled);
        assert_eq!(disabled_ids(&d), ["gpu:RTX|"]);
        assert!(d.all);
        assert_eq!(d.failed.as_deref(), Some("gpu:GTX|"));
    }
    let st = setup(&state).selftest;
    assert_eq!(
        st.blocked.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
        ["gpu:RTX|"]
    );
    assert_eq!(ids(&st), ["cpu", "gpu:GTX|"]);
    assert_eq!(st.devices[1].status, DeviceStatus::Skipped);
    assert_eq!(st.devices[1].state, Some(GpuState::Disabled));
    assert_eq!(
        (st.selected, st.failed.as_deref()),
        (Some(0), Some("gpu:GTX|"))
    );
}

#[test]
fn unblock_needs_full_retest_without_media() {
    let dir = crate::test_dir();
    let state = open(dir.path());
    // 有主模型与 mmproj，要处理图片与音视频；缓存的结果只测了文本
    let c = cfg(dir.path());
    fake_model(&c.model, &space::MODEL);
    fake_model(&c.mmproj, &space::MMPROJ);
    write_cache(dir.path(), 1, "RTX", SAME);
    edit_cache(dir.path(), |j| j["cached"]["mmproj"] = false.into());
    block(dir.path(), 2, "1.0");
    let Ok(Reply::DoneWithWarning { message }) = unblock(&state, "gpu:GTX|") else {
        panic!("缓存不够用时应请用户整体重新检测");
    };
    assert!(message.contains("重新检测全部设备"), "{message}");
    assert!(!state.setup.started.load(Ordering::Relaxed));
    let st = setup(&state).selftest;
    assert!(!st.running && st.blocked.is_empty() && st.stale);
    assert_eq!(st.devices[2].status, DeviceStatus::Skipped);
}

#[test]
fn unblock_during_model_task() {
    let dir = crate::test_dir();
    let state = open(dir.path());
    fake_model(&cfg(dir.path()).model, &space::MODEL);
    write_cache(dir.path(), 1, "RTX", SAME);
    let report = embed_selftest::last_report(&cfg(dir.path()).selftest_cache).unwrap();
    block(dir.path(), 2, "1.0");
    lock(&state.disabled)
        .keys
        .push(embed_selftest::report_key(&report.candidates[2]));
    *lock(&state.setup.task) = Some(ModelTask {
        kind: ModelTaskKind::Download,
        state: ModelTaskState::Running,
        phase: ModelTaskPhase::Preparing,
        file: None,
        index: 0,
        count: 0,
        done: 0,
        total: 0,
        speed: None,
        message: None,
    });
    // 模型任务进行中：照样解除并清掉停用，不当场重测，说明原因；之后仍可自动检测
    let Ok(Reply::DoneWithWarning { message }) = unblock(&state, "gpu:GTX|") else {
        panic!("检测没能开始时应说明原因");
    };
    assert!(message.contains("正在下载、导入或删除模型"), "{message}");
    assert!(!state.setup.started.load(Ordering::Relaxed));
    assert!(lock(&state.disabled).keys.is_empty());
    let st = setup(&state).selftest;
    assert!(!st.running && st.blocked.is_empty());
    assert_eq!(st.devices[2].status, DeviceStatus::Skipped);
}

#[test]
fn after_report_keeps_what_was_not_retested() {
    let dir = crate::test_dir();
    write_cache(dir.path(), 1, "RTX", SAME);
    let mut report = embed_selftest::last_report(&cfg(dir.path()).selftest_cache).unwrap();
    let key =
        |r: &embed_selftest::SelftestReport, i: usize| embed_selftest::report_key(&r.candidates[i]);
    let rtx = selftest::Mode::Retest("gpu:RTX|".into());
    // 手动检测：停用全部清除，本次暂时性失败的显卡重新停用
    report.candidates[2].status = embed_selftest::Status::Failed;
    report.candidates[2].transient = true;
    let mut d = Disabled {
        keys: vec![key(&report, 1)],
        all: true,
        failed: None,
    };
    selftest::after_report(&mut d, &selftest::Mode::Manual, &report, None, &[], true);
    assert!(!d.all);
    assert_eq!(disabled_ids(&d), ["gpu:GTX|"]);
    report.candidates[2].status = embed_selftest::Status::Passed;
    report.candidates[2].transient = false;
    // 列显卡卡住（不保存，只测了 CPU）：停用与出问题的显卡都保留
    let cpu_only = embed_selftest::SelftestReport {
        candidates: vec![report.candidates[0].clone()],
        selected: Some(0),
        from_cache: false,
    };
    let mut d = Disabled {
        keys: vec![key(&report, 1)],
        all: true,
        failed: Some("gpu:RTX|".into()),
    };
    selftest::after_report(&mut d, &selftest::Mode::Manual, &cpu_only, None, &[], false);
    assert!(d.all);
    assert_eq!(disabled_ids(&d), ["gpu:RTX|"]);
    assert_eq!(d.failed.as_deref(), Some("gpu:RTX|"));
    // 重测 RTX 通过、此前没有停用所有显卡：其他显卡的停用照旧，不新增
    let mut d = Disabled {
        keys: vec![key(&report, 2)],
        all: false,
        failed: Some("gpu:RTX|".into()),
    };
    selftest::after_report(&mut d, &rtx, &report, None, &[], true);
    assert!(!d.all);
    assert_eq!(disabled_ids(&d), ["gpu:GTX|"]);
    assert_eq!(d.failed, None);
    // 重测 RTX 没通过：出问题的设备、所有显卡停用都保留
    report.candidates[1].status = embed_selftest::Status::Failed;
    let mut d = Disabled {
        keys: vec![key(&report, 2)],
        all: true,
        failed: Some("gpu:RTX|".into()),
    };
    selftest::after_report(&mut d, &rtx, &report, None, &[], true);
    assert!(d.all);
    assert_eq!(disabled_ids(&d), ["gpu:GTX|"]);
    assert_eq!(d.failed.as_deref(), Some("gpu:RTX|"));
    // 自动检测：通过也不清除停用与出问题的设备
    report.candidates[1].status = embed_selftest::Status::Passed;
    let mut d = Disabled {
        keys: vec![key(&report, 1)],
        all: false,
        failed: Some("gpu:RTX|".into()),
    };
    selftest::after_report(&mut d, &selftest::Mode::Auto, &report, None, &[], true);
    assert_eq!(disabled_ids(&d), ["gpu:RTX|"]);
    assert_eq!(d.failed.as_deref(), Some("gpu:RTX|"));
}

#[test]
fn auto_not_after_gpu_failure_or_manual() {
    let dir = crate::test_dir();
    let docs = crate::test_dir();
    let state = start_with_pipeline(dir.path());
    // 过期、有待建立索引的文件、没有暂停
    write_cache(dir.path(), 1, "RTX", &["GTX", "RTX", "旧显卡"]);
    add_pending(&state, docs.path());
    // 本次运行中显卡出过问题：不自动重试
    lock(&state.disabled).failed = Some("gpu:RTX|".into());
    state.auto_selftest();
    assert!(!state.setup.started.load(Ordering::Relaxed));
    lock(&state.disabled).failed = None;
    // 本次运行手动检测过（此处出错）：不再自动发起，界面保留手动检测的出错说明
    respond(&state, Request::RunSelftest { onboarding: false }).unwrap();
    assert!(wait_selftest(&state).error.is_some());
    state.auto_selftest();
    let st = lock(&state.setup.selftest).clone();
    assert!(!st.running && !st.auto && st.error.is_some(), "{st:?}");
    state.close();
}
