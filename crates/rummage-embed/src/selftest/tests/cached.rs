//! 检测结果缓存的测试：结果是否过期、检测时系统中的显卡、复用上次的结果
use super::*;

#[test]
fn cached_result_current() {
    let g = Candidate::Gpu(gpu("GPU"));
    let keys = vec![
        key_with(&Candidate::Cpu, None),
        key_with(&g, Some("1.0".to_string())),
    ];
    let mut passed = CandidateReport::new(&g, &keys[1]);
    passed.status = Status::Passed;
    let cache = |images: bool, selected| CacheFile {
        cached: Some(Cached {
            keys: keys.clone(),
            report: SelftestReport {
                candidates: vec![report(Status::Passed, Some(9.0)), passed.clone()],
                selected,
                from_cache: false,
            },
            mmproj: images,
            adapters: vec!["GPU".to_string()],
        }),
        blacklist: Vec::new(),
    };
    let here = vec!["GPU".to_string()];
    let same = here.as_slice();
    assert!(cache(true, Some(1)).current(same, true, false, true));
    assert!(cache(false, Some(1)).current(same, false, false, true));
    // 只更新驱动不算过期：不核对键中的驱动版本
    let mut newer = cache(true, Some(1));
    newer.cached.as_mut().unwrap().keys[1].driver = "0.9".to_string();
    assert!(newer.current(same, true, false, true));
    // 只测了文本却要处理图片、换了运行库、选中的卡被屏蔽、没有记录：都要重新检测
    assert!(!cache(false, Some(1)).current(same, true, false, true));
    let mut runtime = cache(true, Some(1));
    runtime.cached.as_mut().unwrap().keys[1].runtime = "llama.cpp b1".to_string();
    assert!(!runtime.current(same, true, false, true));
    let mut black = cache(true, Some(1));
    black.record(keys[1].clone(), Record::Blacklisted, "设备丢失。");
    assert!(!black.current(same, true, false, true));
    // 显卡有增减或更换都算过期（选中的是 CPU 也一样：新插上的显卡可能更快）
    let mut cpu = cache(true, Some(0));
    cpu.record(keys[1].clone(), Record::Blacklisted, "设备丢失。");
    assert!(cpu.current(same, true, false, true));
    for now in [
        vec![],
        vec!["GPU".to_string(), "GPU2".to_string()],
        vec!["GPU2".to_string()],
    ] {
        assert!(
            !cache(true, Some(0)).current(&now, true, false, true),
            "{now:?}"
        );
        assert!(
            !cache(true, Some(1)).current(&now, true, false, true),
            "{now:?}"
        );
    }
    // 用户选了只用 CPU：只核对 CPU 的运行库与模型，显卡有变化也不算
    assert!(cache(true, Some(1)).current(&[], true, true, true));
    assert!(black.current(same, true, true, true));
    assert!(!cache(false, Some(1)).current(same, true, true, true));
    assert!(!CacheFile::default().current(same, false, false, true));
}

#[test]
fn adapter_list() {
    let a = |description: &str, vendor_id| crate::Adapter {
        description: description.to_string(),
        vendor_id,
        device_id: 0,
        vram_mb: 0,
        driver: None,
    };
    // 微软的软件与远程桌面适配器不算显卡；按名称排序，同型号的保留重复项
    let got = cache::adapter_names(vec![
        a("NVIDIA GeForce RTX 4080 SUPER", 0x10de),
        a("Microsoft Basic Render Driver", 0x1414),
        a("Intel(R) Iris(R) Xe Graphics", 0x8086),
        a("NVIDIA GeForce RTX 4080 SUPER", 0x10de),
    ]);
    assert_eq!(
        got,
        [
            "Intel(R) Iris(R) Xe Graphics",
            "NVIDIA GeForce RTX 4080 SUPER",
            "NVIDIA GeForce RTX 4080 SUPER"
        ]
    );
    // 再加一块同型号的显卡：检测结果过期
    let one = cache::adapter_names(vec![a("NVIDIA GeForce RTX 4080 SUPER", 0x10de)]);
    let two = cache::adapter_names(vec![
        a("NVIDIA GeForce RTX 4080 SUPER", 0x10de),
        a("NVIDIA GeForce RTX 4080 SUPER", 0x10de),
    ]);
    let cache = CacheFile {
        cached: Some(Cached {
            keys: vec![key_with(&Candidate::Cpu, None)],
            report: SelftestReport {
                candidates: vec![report(Status::Passed, Some(1.0))],
                selected: Some(0),
                from_cache: false,
            },
            mmproj: true,
            adapters: one.clone(),
        }),
        blacklist: Vec::new(),
    };
    assert!(cache.current(&one, true, false, true));
    assert!(!cache.current(&two, true, false, true));
}

#[test]
fn reuses_previous_results() {
    let (mut cache, keys) = three([900.0, 10.0, 50.0]);
    // 只更新驱动：仍是同一设备，沿用
    let newer = key_with(&Candidate::Gpu(gpu("A")), Some("2.0".to_string()));
    assert_eq!(cache.previous(&newer, true).unwrap().image_ms, Some(10.0));
    assert!(cache.previous(&keys[0], true).is_some());
    // 新出现的显卡没有上次的结果
    let c = key_with(&Candidate::Gpu(gpu("C")), Some("1.0".to_string()));
    assert!(cache.previous(&c, true).is_none());
    // 运行库或模型变化：都要重测（CPU 也是）
    let mut old = keys.clone();
    for k in &mut old {
        k.runtime = "llama.cpp b1".to_string();
    }
    cache.cached.as_mut().unwrap().keys = old;
    assert!(keys.iter().all(|k| cache.previous(k, true).is_none()));
    // 上次只测了文字、现在要处理图片（模型变化）：显卡与 CPU 都重测，CPU 的图片结果也要与黄金向量比较过
    let (mut cache, keys) = three([900.0, 10.0, 50.0]);
    cache.cached.as_mut().unwrap().mmproj = false;
    assert!(cache.previous(&keys[0], true).is_none());
    assert!(cache.previous(&keys[1], true).is_none());
    assert!(cache.previous(&keys[0], false).is_some());
    assert!(cache.previous(&keys[1], false).is_some());
    // 暂时性失败、被跳过的重测；结果中带着它们时算过期（本次运行已检测过时不算）
    let (mut cache, keys) = three([900.0, 10.0, 50.0]);
    let here = ["A".to_string(), "B".to_string()];
    assert!(cache.current(&here, true, false, true));
    let c = cache.cached.as_mut().unwrap();
    c.report.candidates[1].status = Status::Failed;
    c.report.candidates[1].transient = true;
    c.report.candidates[2].status = Status::Skipped;
    assert!(cache.previous(&keys[1], true).is_none());
    assert!(cache.previous(&keys[2], true).is_none());
    assert!(!cache.current(&here, true, false, true));
    assert!(cache.current(&here, true, false, false));
    assert!(cache.current(&here, true, true, true));
    // 暂时性失败的标记写进缓存文件
    let json = serde_json::to_value(&cache).unwrap();
    assert_eq!(json["cached"]["report"]["candidates"][1]["transient"], true);
}
