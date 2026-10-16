//! 选择设备的测试：按缓存选用显卡、选最快的、本次运行停用的显卡、停用时换下一块、停用的键
use std::fs;

use super::super::cache::{Blacklisted, key_at};
use super::*;

#[test]
fn usable_gpu_from_cache() {
    let g = Candidate::Gpu(gpu("GPU"));
    let keys = vec![
        key_with(&Candidate::Cpu, None),
        key_with(&g, Some("1.0".to_string())),
    ];
    let mut passed = CandidateReport::new(&g, &keys[1]);
    passed.status = Status::Passed;
    // 选中 GPU（1）时它比 CPU 快，选中 CPU（0）时比 CPU 慢，没有选中时都未通过
    let cache = |selected: Option<usize>, blacklist: Vec<Blacklisted>| {
        let mut cpu = report(Status::Passed, Some(9.0));
        cpu.text_ms = Some(9.0);
        let mut gpu = passed.clone();
        gpu.image_ms = Some(if selected == Some(1) { 1.0 } else { 99.0 });
        gpu.text_ms = gpu.image_ms;
        if selected.is_none() {
            cpu.status = Status::Failed;
            gpu.status = Status::Failed;
        }
        CacheFile {
            cached: Some(Cached {
                keys: keys.clone(),
                report: SelftestReport {
                    candidates: vec![cpu, gpu],
                    selected,
                    from_cache: false,
                },
                mmproj: true,
                adapters: vec!["GPU".to_string()],
            }),
            blacklist,
        }
    };
    // 现在系统中的显卡；拔出后没有它
    let here = vec!["GPU".to_string()];
    let same = here.as_slice();
    let gone: &[String] = &[];
    let got = cache(Some(1), Vec::new())
        .usable_gpu(same, true, None, &[])
        .unwrap();
    assert_eq!(got.gpu().unwrap().description, "GPU");
    // 选中的是 CPU、没有选中、已拔出、被屏蔽、没有记录：都用 CPU；只更新驱动不影响（不核对键中的驱动版本）。
    assert!(
        cache(Some(0), Vec::new())
            .usable_gpu(same, true, None, &[])
            .is_none()
    );
    assert!(
        cache(None, Vec::new())
            .usable_gpu(same, true, None, &[])
            .is_none()
    );
    assert!(
        cache(Some(1), Vec::new())
            .usable_gpu(gone, true, None, &[])
            .is_none()
    );
    // 检测时 DXGI 中就没有同名的（描述与 Vulkan 不一致）：无法判断是否拔出，照用
    let mut unnamed = cache(Some(1), Vec::new());
    unnamed.cached.as_mut().unwrap().adapters = vec!["GPU (DXGI)".to_string()];
    assert!(unnamed.usable_gpu(gone, true, None, &[]).is_some());
    let black = vec![Blacklisted {
        key: keys[1].clone(),
        reason: "设备丢失".to_string(),
        kind: Record::Blacklisted,
    }];
    assert!(
        cache(Some(1), black.clone())
            .usable_gpu(same, true, None, &[])
            .is_none()
    );
    assert!(
        CacheFile::default()
            .usable_gpu(same, true, None, &[])
            .is_none()
    );
    assert!(
        cached_gpu(
            Path::new("不存在的目录/selftest.json"),
            same,
            false,
            None,
            &[]
        )
        .is_none()
    );
    // 用户指定的显卡：检测通过时不论是否选中都用它；标识不符、指定 CPU、被屏蔽、或已拔出时为空
    let id = passed.device_id();
    assert!(id.starts_with("gpu:GPU|"), "{id}");
    assert_eq!(report(Status::Passed, Some(9.0)).device_id(), "cpu");
    let got = cache(Some(0), Vec::new())
        .usable_gpu(same, true, Some(&id), &[])
        .unwrap();
    assert_eq!(got.description, "GPU");
    assert!(
        cache(Some(1), Vec::new())
            .usable_gpu(same, true, Some("gpu:GPU|9"), &[])
            .is_none()
    );
    assert!(
        cache(Some(1), Vec::new())
            .usable_gpu(same, true, Some("cpu"), &[])
            .is_none()
    );
    assert!(
        cache(Some(1), black)
            .usable_gpu(same, true, Some(&id), &[])
            .is_none()
    );
    assert!(
        cache(Some(0), Vec::new())
            .usable_gpu(gone, true, Some(&id), &[])
            .is_none()
    );

    // 运行中设备丢失：写入屏蔽记录后不再选它，重复写入不重复记录
    let dir = std::env::temp_dir().join(format!("rummage-lost-{}", std::process::id()));
    let path = dir.join("cache").join("selftest.json");
    cache(Some(1), Vec::new()).save(&path).unwrap();
    let gpu = CacheFile::load(&path)
        .usable_gpu(same, true, None, &[])
        .unwrap();
    blacklist_gpu(&path, &gpu, "显卡设备丢失。").unwrap();
    blacklist_gpu(&path, &gpu, "显卡设备丢失（第二次）。").unwrap();
    let back = CacheFile::load(&path);
    assert!(back.usable_gpu(same, true, None, &[]).is_none());
    assert_eq!(back.blacklist.len(), 1);
    // 按现在的键（`report_key`，驱动版本取当前值）记录
    assert_eq!(
        back.blacklisted(&report_key(&gpu)).unwrap().reason,
        "显卡设备丢失（第二次）。"
    );
    // 自检结果仍在，CPU 等其他候选不受影响
    assert!(back.cached_report(&keys, true).is_some());
    // 只测了文本的记录：只处理文本时可用，要处理图片时不选 GPU、也不当作缓存命中
    let mut text_only = cache(Some(1), Vec::new());
    text_only.cached.as_mut().unwrap().mmproj = false;
    assert!(text_only.usable_gpu(same, false, None, &[]).is_some());
    assert!(text_only.usable_gpu(same, true, None, &[]).is_none());
    assert!(text_only.cached_report(&keys, false).is_some());
    assert!(text_only.cached_report(&keys, true).is_none());
    assert!(back.blacklisted(&keys[0]).is_none());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn selects_fastest_passed() {
    let reports = vec![
        report(Status::Passed, Some(400.0)),
        report(Status::Failed, Some(10.0)),
        report(Status::Passed, Some(50.0)),
        report(Status::Blacklisted, None),
        report(Status::Passed, Some(80.0)),
    ];
    assert_eq!(select(&reports, true), Some(2));
    assert_eq!(select(&reports[..2], true), Some(0));
    assert_eq!(select(&reports[1..2], true), None);
    assert_eq!(select(&[], true), None);
    // 只测文本时按文本耗时选
    let mut text = reports.clone();
    text[0].text_ms = Some(5.0);
    text[2].text_ms = Some(9.0);
    assert_eq!(select(&text, false), Some(0));
    // 没有这项耗时的排在最后，但仍可选
    assert_eq!(select(&reports, false), Some(0));
    assert_eq!(by_speed(&reports, true), [2, 4, 0]);
    assert_eq!(by_speed(&text, false), [0, 2, 4]);
}

#[test]
fn session_disabled_gpu_not_tested_again() {
    let g = Candidate::Gpu(gpu("RTX"));
    let key = device_key(&g);
    // 第一次检测中这块卡暂时性失败：调用方按报告的键记住它
    let mut first = CandidateReport::new(&g, &key);
    first.transient = true;
    assert_eq!(report_key(&first), key);
    let disabled = vec![report_key(&first)];
    // 重新检测：直接跳过，不调用测试（不初始化这块卡）
    let mut reports = vec![
        CandidateReport::new(&Candidate::Cpu, &key_with(&Candidate::Cpu, None)),
        CandidateReport::new(&g, &key),
    ];
    let keys = [key_with(&Candidate::Cpu, None), key.clone()];
    let mut tested = Vec::new();
    test_in_order(&mut reports, &[], |i, r| {
        if r.backend == Backend::Vulkan
            && skip_gpu(r, &keys[i], &CacheFile::default(), &disabled, false)
        {
            return;
        }
        tested.push(i);
    });
    assert_eq!(tested, [0]);
    assert_eq!(reports[1].status, Status::Skipped);
    assert_eq!(reports[1].reason.as_deref(), Some(SESSION_DISABLED_REASON));
    // 不在集合中的卡照常测试
    let mut r = CandidateReport::new(&g, &key);
    assert!(!skip_gpu(&mut r, &key, &CacheFile::default(), &[], false));
}

#[test]
fn next_fastest_when_disabled() {
    let (cache, _) = three([900.0, 10.0, 50.0]);
    let keys = now_keys(&cache);
    let here = ["A".to_string(), "B".to_string()];
    let pick = |disabled: &[DeviceKey], device: Option<&str>| {
        cache
            .usable_gpu(&here, true, device, disabled)
            .map(|r| r.description)
    };
    assert_eq!(pick(&[], None).as_deref(), Some("A"));
    // A 出了问题：改用剩下的设备中最快的 B（另一块显卡），再出问题才用 CPU
    assert_eq!(pick(&keys[1..2], None).as_deref(), Some("B"));
    assert_eq!(pick(&keys[1..], None), None);
    // 用户选了 B 而它出了问题：指定的不能用，由调用方按自动选择（这里是 A）
    let b = cache.cached.as_ref().unwrap().report.candidates[2].device_id();
    assert_eq!(pick(&[], Some(&b)).as_deref(), Some("B"));
    assert_eq!(pick(&keys[2..], Some(&b)), None);
    assert_eq!(pick(&keys[2..], None).as_deref(), Some("A"));
    // CPU 比 B 快：A 出了问题时用 CPU
    let (cache, _) = three([30.0, 10.0, 50.0]);
    let keys = now_keys(&cache);
    assert!(cache.usable_gpu(&here, true, None, &keys[1..2]).is_none());
    // 被屏蔽（含运行中算出 NaN 记为未通过）的同样跳过
    let (mut cache, keys) = three([900.0, 10.0, 50.0]);
    cache.record(keys[1].clone(), Record::Failed, "算出无效结果。");
    let got = cache.usable_gpu(&here, true, None, &[]).unwrap();
    assert_eq!(got.description, "B");
}

#[test]
fn report_key_uses_current_driver() {
    let g = Candidate::Gpu(gpu("A"));
    let old = key_with(&g, Some("1.0".to_string()));
    let r = CandidateReport::new(&g, &old);
    // 现在的驱动版本查得到时用它，与现在检测时的键（`key_with` 加当前驱动）一致；查不到（已拔出）时用检测时记下的
    assert_eq!(
        key_at(&r, Some("2.0".to_string())),
        key_with(&g, Some("2.0".to_string()))
    );
    assert_eq!(report_key(&r), old);
}

#[test]
fn session_disabled_ignores_driver() {
    // 外接显卡拔出时停用的键驱动版本为未知，插回后（驱动查得到）本次运行中仍不选它、不测它
    let (cache, keys) = three([900.0, 10.0, 50.0]);
    let here = ["A".to_string(), "B".to_string()];
    let mut unplugged = keys[1].clone();
    unplugged.driver = UNKNOWN_DRIVER.to_string();
    let got = cache
        .usable_gpu(&here, true, None, &[unplugged.clone()])
        .unwrap();
    assert_eq!(got.description, "B");
    let mut r = CandidateReport::new(&Candidate::Gpu(gpu("A")), &keys[1]);
    assert!(skip_gpu(
        &mut r,
        &keys[1],
        &CacheFile::default(),
        &[unplugged],
        false
    ));
    assert_eq!(r.status, Status::Skipped);
}
