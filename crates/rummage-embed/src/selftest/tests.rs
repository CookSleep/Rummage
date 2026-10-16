//! 自检的测试：设备标识与报告，以及各子模块共用的辅助函数；缓存、屏蔽记录、选择设备、单个候选与整个检测流程的测试在子模块中
use super::*;

mod cached;
mod candidate;
mod records;
mod run;
mod select;
mod speed;

fn gpu(desc: &str) -> GpuInfo {
    GpuInfo {
        ggml_index: 0,
        name: "Vulkan0".to_string(),
        description: desc.to_string(),
        pci_id: Some("0000:01:00.0".to_string()),
        memory_total: 16 << 30,
        integrated: false,
    }
}

#[test]
fn keys() {
    let cpu = key_with(&Candidate::Cpu, Some("ignored".to_string()));
    assert_eq!((cpu.description.as_str(), cpu.driver.as_str()), ("", ""));
    assert_eq!(cpu.runtime, space::RUNTIME);
    assert!(cpu.model.starts_with(space::MODEL.sha256));
    assert!(cpu.model.ends_with(space::MMPROJ.sha256));

    let c = Candidate::Gpu(gpu("NVIDIA GeForce RTX 4080 SUPER"));
    let k = key_with(&c, Some("32.0.15.6094".to_string()));
    assert_eq!(k.description, "NVIDIA GeForce RTX 4080 SUPER");
    assert_eq!(k.pci_id.as_deref(), Some("0000:01:00.0"));
    assert_eq!(k.driver, "32.0.15.6094");
    assert_eq!(key_with(&c, None).driver, UNKNOWN_DRIVER);
    assert_ne!(k, key_with(&c, Some("32.0.15.7000".to_string())));
    assert_eq!(k, key_with(&c, Some("32.0.15.6094".to_string())));
    // 枚举顺序变了（ggml 设备名称不同），仍是同一个键
    let mut moved = gpu("NVIDIA GeForce RTX 4080 SUPER");
    moved.name = "Vulkan1".to_string();
    assert_eq!(
        k,
        key_with(&Candidate::Gpu(moved), Some("32.0.15.6094".to_string()))
    );
    // CPU 报告的描述是推理进程报告的处理器型号，键仍与自检时相同
    let mut r = report(Status::Passed, None);
    r.description = "12th Gen Intel(R) Core(TM) i7-12700H".to_string();
    assert_eq!(report_key(&r), cpu);
}

fn report(status: Status, image_ms: Option<f64>) -> CandidateReport {
    CandidateReport {
        status,
        image_ms,
        ..CandidateReport::new(&Candidate::Cpu, &key_with(&Candidate::Cpu, None))
    }
}

#[test]
fn report_gpu() {
    let mut r = report(Status::Passed, Some(1.0));
    assert!(r.gpu().is_none());
    r.backend = Backend::Vulkan;
    r.description = "NVIDIA GeForce RTX 4080 SUPER".to_string();
    r.pci_id = Some("0000:06:00.0".to_string());
    let g = r.gpu().unwrap();
    assert_eq!(
        (g.description.as_str(), g.pci_id.as_deref()),
        ("NVIDIA GeForce RTX 4080 SUPER", Some("0000:06:00.0"))
    );
    r.fail("一".to_string());
    r.fail("二".to_string());
    assert_eq!(r.status, Status::Failed);
    assert_eq!(r.reason.as_deref(), Some("一\n二"));
}

/// 缓存中各候选现在的键（运行中停用用的键，`report_key`）
fn now_keys(cache: &CacheFile) -> Vec<DeviceKey> {
    let c = cache.cached.as_ref().unwrap();
    c.report.candidates.iter().map(report_key).collect()
}

/// CPU 与描述为 A、B 的两块显卡都通过，图像耗时为 `ms`（CPU、A、B），检测时系统中有 A、B
fn three(ms: [f64; 3]) -> (CacheFile, Vec<DeviceKey>) {
    let gpus = [Candidate::Gpu(gpu("A")), Candidate::Gpu(gpu("B"))];
    let keys = vec![
        key_with(&Candidate::Cpu, None),
        key_with(&gpus[0], Some("1.0".to_string())),
        key_with(&gpus[1], Some("1.0".to_string())),
    ];
    let mut candidates = vec![report(Status::Passed, Some(ms[0]))];
    for (g, (k, t)) in gpus.iter().zip(keys[1..].iter().zip(&ms[1..])) {
        let mut r = CandidateReport::new(g, k);
        r.status = Status::Passed;
        r.image_ms = Some(*t);
        candidates.push(r);
    }
    let selected = select(&candidates, true);
    let cache = CacheFile {
        cached: Some(Cached {
            keys: keys.clone(),
            report: SelftestReport {
                candidates,
                selected,
                from_cache: false,
            },
            mmproj: true,
            adapters: vec!["A".to_string(), "B".to_string()],
        }),
        blacklist: Vec::new(),
    };
    (cache, keys)
}
