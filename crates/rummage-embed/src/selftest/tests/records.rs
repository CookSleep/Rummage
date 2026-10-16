//! 屏蔽与未通过记录的测试：写入与解除、只测一块、手动检测通过即解除、自动检测跳过、驱动或运行库更新后的匹配、同型号显卡按 PCI 号区分、不降级
use std::fs;

use super::super::cache::Blacklisted;
use super::*;

#[test]
fn cache_hit_and_blacklist() {
    let c = Candidate::Gpu(gpu("GPU"));
    let keys = vec![
        key_with(&Candidate::Cpu, None),
        key_with(&c, Some("1.0.0.1".to_string())),
    ];
    let rep = SelftestReport {
        candidates: vec![report(Status::Passed, Some(1.0))],
        selected: Some(0),
        from_cache: false,
    };
    let cache = CacheFile {
        cached: Some(Cached {
            keys: keys.clone(),
            report: rep.clone(),
            mmproj: true,
            adapters: vec!["GPU".to_string()],
        }),
        blacklist: vec![Blacklisted {
            key: keys[1].clone(),
            reason: "超时".to_string(),
            kind: Record::Blacklisted,
        }],
    };
    assert_eq!(cache.cached_report(&keys, true), Some(rep.clone()));
    // 只更新驱动仍命中；候选减少、换了显卡都不命中。
    let newer = vec![keys[0].clone(), key_with(&c, Some("1.0.0.2".to_string()))];
    assert_eq!(cache.cached_report(&newer, true), Some(rep));
    assert!(cache.cached_report(&keys[..1], true).is_none());
    let other = vec![
        keys[0].clone(),
        key_with(&Candidate::Gpu(gpu("GPU2")), Some("1.0.0.1".to_string())),
    ];
    assert!(cache.cached_report(&other, true).is_none());
    assert!(CacheFile::default().cached_report(&keys, true).is_none());

    assert_eq!(cache.blacklisted(&keys[1]).unwrap().reason, "超时");
    assert!(cache.blacklisted(&newer[1]).is_none());
    assert!(cache.blacklisted(&keys[0]).is_none());

    // 写入后读回一致；文件损坏时当作空记录。
    let dir = std::env::temp_dir().join(format!("rummage-selftest-{}", std::process::id()));
    let path = dir.join("cache").join("selftest.json");
    cache.save(&path).unwrap();
    let back = CacheFile::load(&path);
    assert!(back.cached_report(&keys, true).is_some());
    assert_eq!(back.blacklisted(&keys[1]).unwrap().reason, "超时");
    fs::write(&path, b"{").unwrap();
    assert!(CacheFile::load(&path).cached.is_none());
    assert!(CacheFile::load(&dir.join("none.json")).blacklist.is_empty());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn records_and_unblock() {
    let g = Candidate::Gpu(gpu("GPU"));
    let h = Candidate::Gpu(gpu("GPU2"));
    let keys = vec![
        key_with(&Candidate::Cpu, None),
        key_with(&g, Some("1.0".to_string())),
        key_with(&h, Some("1.0".to_string())),
    ];
    let dir = std::env::temp_dir().join(format!("rummage-clear-{}", std::process::id()));
    let path = dir.join("cache").join("selftest.json");
    let mut cache = CacheFile {
        cached: Some(Cached {
            keys: keys.clone(),
            report: SelftestReport {
                candidates: vec![report(Status::Passed, Some(1.0))],
                selected: Some(0),
                from_cache: false,
            },
            mmproj: true,
            adapters: vec!["GPU".to_string()],
        }),
        blacklist: Vec::new(),
    };
    cache.record(keys[1].clone(), Record::Failed, "余弦低于门槛。");
    cache.record(keys[2].clone(), Record::Blacklisted, "设备丢失。");
    // 同一个键再记一次：更新种类与原因，不重复
    cache.record(keys[1].clone(), Record::Failed, "余弦低于门槛（第二次）。");
    cache.save(&path).unwrap();
    let back = CacheFile::load(&path);
    assert_eq!(back.blacklist.len(), 2);
    assert_eq!(back.blacklisted(&keys[1]).unwrap().kind, Record::Failed);
    assert_eq!(
        back.blacklisted(&keys[2]).unwrap().kind,
        Record::Blacklisted
    );

    // 同一块显卡的其他屏蔽记录：运行中双键写入的另一条（原因为空）与驱动版本未知的
    cache.record(
        key_with(&h, Some("2.0".to_string())),
        Record::Blacklisted,
        "",
    );
    cache.record(key_with(&h, None), Record::Blacklisted, "驱动事件。");
    let c = cache.cached.as_mut().unwrap();
    c.adapters.push("GPU2".to_string());
    c.report.candidates.push(CandidateReport {
        status: Status::Passed,
        ..CandidateReport::new(&g, &keys[1])
    });
    c.report.candidates.push(CandidateReport {
        status: Status::Blacklisted,
        ..CandidateReport::new(&h, &keys[2])
    });
    cache.save(&path).unwrap();
    let (gid, hid) = (keys[1].device_id(), keys[2].device_id());
    // 每块显卡一项，原因取最后一条非空的；未通过的不算屏蔽
    let recs = gpu_records(&path, &["GPU".to_string(), "GPU2".to_string()]);
    assert_eq!(recs.blocked.len(), 1);
    assert_eq!(recs.blocked[0].device_id(), hid);
    assert_eq!(recs.blocked[0].reason.as_deref(), Some("驱动事件。"));
    assert!(recs.absent.is_empty());
    // 拔出后：检测时在、现在不在
    assert_eq!(
        gpu_records(&path, &["GPU".to_string()]).absent,
        std::slice::from_ref(&hid)
    );

    assert!(!unblock_gpu(&path, &gid).unwrap());
    assert!(unblock_gpu(&path, &hid).unwrap());
    let back = CacheFile::load(&path);
    // 这块显卡的屏蔽记录全部删除，另一块的未通过记录保留
    assert!(!back.ever_blacklisted(&keys[2]));
    assert_eq!(back.blacklist.len(), 1);
    assert_eq!(back.blacklisted(&keys[1]).unwrap().kind, Record::Failed);
    // 缓存中改为“已跳过”：自动检测会重测它（结果算过期）
    let r = &back.cached.as_ref().unwrap().report.candidates[2];
    assert_eq!(
        (r.status, r.reason.as_deref()),
        (Status::Skipped, Some(UNBLOCKED_REASON))
    );
    assert!(back.previous(&keys[2], true).is_none());
    assert!(back.previous(&keys[1], true).is_some());
    assert!(!back.current(&["GPU".to_string(), "GPU2".to_string()], true, false, true));
    assert!(!unblock_gpu(&path, &hid).unwrap());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn only_replaces_target() {
    let g = Candidate::Gpu(gpu("GPU"));
    let h = Candidate::Gpu(gpu("GPU2"));
    let cpu = key_with(&Candidate::Cpu, None);
    let gk = key_with(&g, Some("1.0".to_string()));
    let mut old_h = CandidateReport::new(&h, &key_with(&h, Some("1.0".to_string())));
    old_h.status = Status::Skipped;
    let mut cache = CacheFile {
        cached: Some(Cached {
            keys: vec![cpu.clone(), gk.clone()],
            report: SelftestReport {
                candidates: vec![
                    report(Status::Passed, Some(100.0)),
                    CandidateReport {
                        status: Status::Passed,
                        image_ms: Some(50.0),
                        ..CandidateReport::new(&g, &gk)
                    },
                ],
                selected: Some(1),
                from_cache: false,
            },
            mmproj: true,
            adapters: vec!["GPU".to_string()],
        }),
        blacklist: Vec::new(),
    };
    // 其余设备照缓存原样显示：不看驱动版本
    let gk2 = key_with(&g, Some("2.0".to_string()));
    assert_eq!(cache.shown(&gk2).unwrap().image_ms, Some(50.0));
    assert!(cache.shown(&key_with(&h, None)).is_none());
    // 追加缓存中没有的设备；更快就选它，其余照旧
    let hk = key_with(&h, Some("2.0".to_string()));
    let mut new_h = CandidateReport::new(&h, &hk);
    (new_h.status, new_h.image_ms) = (Status::Passed, Some(10.0));
    cache.replace(hk.clone(), new_h, true);
    let c = cache.cached.as_ref().unwrap();
    assert_eq!(c.keys, [cpu.clone(), gk.clone(), hk.clone()]);
    assert_eq!((c.report.selected, c.mmproj), (Some(2), true));
    // 替换已有的一项；本次只测了文本时整份不再算测过图片与音视频
    cache.replace(hk.clone(), old_h, false);
    let c = cache.cached.as_ref().unwrap();
    assert_eq!(c.keys.len(), 3);
    assert_eq!(c.report.candidates[2].status, Status::Skipped);
    assert!(!c.mmproj);
}

#[test]
fn manual_pass_unblocks() {
    let g = Candidate::Gpu(gpu("GPU"));
    let old = key_with(&g, Some("1.0".to_string()));
    let new = key_with(&g, Some("2.0".to_string()));
    let mut passed = CandidateReport::new(&g, &new);
    passed.status = Status::Passed;
    let report = SelftestReport {
        candidates: vec![passed],
        selected: Some(0),
        from_cache: false,
    };
    let blocked = || {
        let mut c = CacheFile::default();
        c.record(old.clone(), Record::Blacklisted, "设备丢失。");
        c.record(new.clone(), Record::Failed, "余弦低于门槛。");
        c
    };
    // 自动检测、只测一块时通过不解除
    let opts = |auto: bool, only: Option<&str>| Options {
        exe: PathBuf::new(),
        golden_dir: PathBuf::new(),
        cpu_runtime: PathBuf::new(),
        vulkan_runtime: None,
        model: PathBuf::new(),
        mmproj: PathBuf::new(),
        cache_file: PathBuf::new(),
        use_cache: false,
        save: true,
        images: true,
        auto,
        cancel: Arc::default(),
        only: only.map(str::to_string),
    };
    assert!(opts(false, None).manual());
    assert!(!opts(true, None).manual() && !opts(false, Some("gpu:X|")).manual());
    let mut cache = blocked();
    cache.merge(Vec::new(), &report, false);
    assert!(cache.ever_blacklisted(&new));
    // 手动检测中通过：删除屏蔽记录，未通过记录不动
    let mut cache = blocked();
    cache.merge(Vec::new(), &report, true);
    assert!(!cache.ever_blacklisted(&new));
    assert!(cache.records(&[]).blocked.is_empty());
    assert_eq!(cache.blacklisted(&new).unwrap().kind, Record::Failed);
    // 本次新记下的屏蔽照常合并（没有通过的显卡不解除）
    let mut cache = CacheFile::default();
    let failed = SelftestReport {
        candidates: vec![CandidateReport::new(&g, &new)],
        selected: None,
        from_cache: false,
    };
    cache.merge(
        vec![(new.clone(), Record::Blacklisted, "设备丢失。".to_string())],
        &failed,
        true,
    );
    assert!(cache.ever_blacklisted(&new));
}

#[test]
fn auto_skips_ever_blacklisted() {
    let g = Candidate::Gpu(gpu("RTX"));
    let old = key_with(&g, Some("1.0".to_string()));
    let new = key_with(&g, Some("2.0".to_string()));
    let mut cache = CacheFile::default();
    cache.record(old.clone(), Record::Blacklisted, "设备丢失。");
    // 驱动更新后：手动检测会再试，自动检测仍跳过
    let mut r = CandidateReport::new(&g, &new);
    assert!(!skip_gpu(&mut r, &new, &cache, &[], false));
    assert!(skip_gpu(&mut r, &new, &cache, &[], true));
    assert_eq!(r.status, Status::Blacklisted);
    assert_eq!(r.reason, None);
    // 未通过（余弦不达标）的记录不影响自动检测：换了驱动可以再试
    let mut failed = CacheFile::default();
    failed.record(old, Record::Failed, "余弦低于门槛。");
    let mut r = CandidateReport::new(&g, &new);
    assert!(!skip_gpu(&mut r, &new, &failed, &[], true));
    // 另一块显卡的记录不影响
    let h = Candidate::Gpu(gpu("RTX2"));
    let other = key_with(&h, Some("2.0".to_string()));
    let mut r = CandidateReport::new(&h, &other);
    assert!(!skip_gpu(&mut r, &other, &cache, &[], true));
}

#[test]
fn runtime_block_survives_driver_updates() {
    // 驱动 D1 时检测通过，D2 时运行中出错屏蔽，再升到 D3
    let (mut cache, keys) = three([900.0, 10.0, 50.0]);
    let here = ["A".to_string(), "B".to_string()];
    let a = cache.cached.as_ref().unwrap().report.candidates[1].clone();
    let at = |d: &str| key_with(&Candidate::Gpu(gpu("A")), Some(d.to_string()));
    assert_eq!(keys[1], at("1.0"));
    cache.block(&a, at("2.0"), "设备丢失。");
    assert!(cache.blacklisted(&at("1.0")).is_some());
    assert!(cache.blacklisted(&at("2.0")).is_some());
    assert!(cache.blacklisted(&at("3.0")).is_none());
    // 自动选用不选回它，结果算过期
    let got = cache.usable_gpu(&here, true, None, &[]).unwrap();
    assert_eq!(got.description, "B");
    assert!(!cache.current(&here, true, false, false));
    // 自动检测（D3）跳过它；手动检测在 D2 时仍不测，D3 时按新驱动重测
    let mut r = CandidateReport::new(&Candidate::Gpu(gpu("A")), &at("3.0"));
    assert!(skip_gpu(&mut r, &at("3.0"), &cache, &[], true));
    assert_eq!(r.status, Status::Blacklisted);
    let mut r = CandidateReport::new(&Candidate::Gpu(gpu("A")), &at("2.0"));
    assert!(skip_gpu(&mut r, &at("2.0"), &cache, &[], false));
    let mut r = CandidateReport::new(&Candidate::Gpu(gpu("A")), &at("3.0"));
    assert!(!skip_gpu(&mut r, &at("3.0"), &cache, &[], false));
    // 驱动没变时只记一条
    let (mut cache, _) = three([900.0, 10.0, 50.0]);
    cache.block(&a, at("1.0"), "设备丢失。");
    assert_eq!(cache.blacklist.len(), 1);
}

#[test]
fn blacklist_not_downgraded() {
    let key = key_with(&Candidate::Gpu(gpu("RTX")), Some("1.0".to_string()));
    let mut cache = CacheFile::default();
    cache.record(key.clone(), Record::Blacklisted, "设备丢失。");
    cache.record(key.clone(), Record::Failed, "余弦低于门槛。");
    let b = cache.blacklisted(&key).unwrap();
    assert_eq!(
        (b.kind, b.reason.as_str()),
        (Record::Blacklisted, "设备丢失。")
    );
    // 未通过可以升级为屏蔽
    let other = key_with(&Candidate::Gpu(gpu("GTX")), Some("1.0".to_string()));
    cache.record(other.clone(), Record::Failed, "余弦低于门槛。");
    cache.record(other.clone(), Record::Blacklisted, "设备丢失。");
    assert_eq!(cache.blacklisted(&other).unwrap().kind, Record::Blacklisted);
}

#[test]
fn same_device_and_pci() {
    let at = |pci: &str, driver: &str| {
        let mut g = gpu("RTX");
        g.pci_id = Some(pci.to_string());
        key_with(&Candidate::Gpu(g), Some(driver.to_string()))
    };
    let k = at("0000:01:00.0", "1.0");
    // 同一设备只看描述、PCI 号、运行库与模型，不看驱动版本
    assert!(k.same_device(&at("0000:01:00.0", "2.0")));
    assert!(!k.same_device(&at("0000:02:00.0", "1.0")));
    let mut runtime = k.clone();
    runtime.runtime = "llama.cpp b1".to_string();
    assert!(!k.same_device(&runtime));
    let mut model = k.clone();
    model.model = "00+00".to_string();
    assert!(!k.same_device(&model));
    // 同型号的两块卡（PCI 号不同）：解除一块只删它的记录（含运行库变化前记下的），另一块仍屏蔽、未通过记录不动
    let other = at("0000:02:00.0", "1.0");
    let mut cache = CacheFile::default();
    cache.record(k.clone(), Record::Blacklisted, "设备丢失。");
    cache.record(runtime.clone(), Record::Blacklisted, "驱动事件。");
    cache.record(at("0000:01:00.0", "2.0"), Record::Failed, "余弦低于门槛。");
    cache.record(other.clone(), Record::Blacklisted, "设备丢失。");
    assert_eq!(cache.records(&[]).blocked.len(), 2);
    assert!(cache.unblock(&k.device_id()));
    assert!(!cache.ever_blacklisted(&k));
    assert!(cache.ever_blacklisted(&other));
    let left: Vec<_> = cache
        .blacklist
        .iter()
        .map(|b| (b.key.clone(), b.kind))
        .collect();
    assert_eq!(
        left,
        [
            (at("0000:01:00.0", "2.0"), Record::Failed),
            (other.clone(), Record::Blacklisted)
        ]
    );
    // 只有未通过记录时不算屏蔽，解除返回否
    assert!(!cache.unblock(&k.device_id()));
}

#[test]
fn records_after_runtime_change() {
    let g = Candidate::Gpu(gpu("RTX"));
    let cur = key_with(&g, Some("1.0".to_string()));
    let mut old = cur.clone();
    old.runtime = "llama.cpp b1".to_string();
    // 运行库变化前屏蔽：仍列为已屏蔽（可以解除），自动检测仍跳过；手动检测按新键重测
    let mut cache = CacheFile::default();
    cache.record(old.clone(), Record::Blacklisted, "设备丢失。");
    assert_eq!(cache.records(&[]).blocked.len(), 1);
    let mut r = CandidateReport::new(&g, &cur);
    assert!(skip_gpu(&mut r, &cur, &cache, &[], true));
    assert_eq!(r.status, Status::Blacklisted);
    let mut r = CandidateReport::new(&g, &cur);
    assert!(!skip_gpu(&mut r, &cur, &cache, &[], false));
    // 运行库变化前未通过：自动与手动检测都重测；同一键的未通过记录则都不测，显示为未通过并写明原因
    let mut cache = CacheFile::default();
    cache.record(old, Record::Failed, "余弦低于门槛。");
    let mut r = CandidateReport::new(&g, &cur);
    assert!(!skip_gpu(&mut r, &cur, &cache, &[], true));
    cache.record(cur.clone(), Record::Failed, "余弦低于门槛。");
    for auto in [false, true] {
        let mut r = CandidateReport::new(&g, &cur);
        assert!(skip_gpu(&mut r, &cur, &cache, &[], auto));
        assert_eq!(r.status, Status::Failed);
        assert!(r.reason.unwrap().contains("不达标"));
    }
    // 本次运行停用优先于记录：显示为本次运行停用
    let mut r = CandidateReport::new(&g, &cur);
    assert!(skip_gpu(
        &mut r,
        &cur,
        &cache,
        std::slice::from_ref(&cur),
        false
    ));
    assert_eq!(r.reason.as_deref(), Some(SESSION_DISABLED_REASON));
}
