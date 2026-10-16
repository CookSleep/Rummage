//! 整个检测流程（`run`）的测试：只测一块时其余设备照缓存原样、保存时只替换这一项；自动检测沿用仍然有效的结果、
//! 跳过本次运行停用与有过屏蔽记录的显卡；手动检测在驱动变化后重测屏蔽过的显卡。
//! 都不初始化任何设备：显卡因记录或停用被跳过，或因没有显卡运行库（`vulkan_runtime` 为空）在启动推理进程前就失败；
//! 不测 CPU（只测一块时不测，自动检测时沿用缓存）。黄金样本用仓库中的 `assets/golden`
use std::fs;

use super::*;

/// 测试用的显卡描述，不会与本机 DXGI 中的显卡同名：现在的驱动版本查不到，`device_key` 的驱动版本为 `unknown`
const A: &str = "测试显卡甲";
const B: &str = "测试显卡乙";
const C: &str = "测试显卡丙";

fn cand(desc: &str) -> Candidate {
    Candidate::Gpu(gpu(desc))
}

/// 本次检测时的键（与 `device_key` 相同：驱动版本查不到）
fn now(desc: &str) -> DeviceKey {
    key_with(&cand(desc), None)
}

/// 上次检测（驱动 1.0）的键
fn then(desc: &str) -> DeviceKey {
    key_with(&cand(desc), Some("1.0".to_string()))
}

/// 缓存：CPU、甲、乙都通过，各有不同的耗时与原因；乙最快
fn cached_three() -> CacheFile {
    let mut cpu = report(Status::Passed, Some(900.0));
    (cpu.text_ms, cpu.reason) = (Some(90.0), Some("CPU 上次的原因。".to_string()));
    let gpu = |desc: &str, ms: f64| CandidateReport {
        status: Status::Passed,
        image_ms: Some(ms),
        text_ms: Some(ms / 10.0),
        audio_ms: Some(ms / 2.0),
        video_ms: Some(ms * 2.0),
        reason: Some(format!("{desc}上次的原因。")),
        ..CandidateReport::new(&cand(desc), &then(desc))
    };
    let candidates = vec![cpu, gpu(A, 50.0), gpu(B, 10.0)];
    let selected = select(&candidates, true);
    CacheFile {
        cached: Some(Cached {
            keys: vec![key_with(&Candidate::Cpu, None), then(A), then(B)],
            report: SelftestReport {
                candidates,
                selected,
                from_cache: false,
            },
            mmproj: true,
            adapters: vec![A.to_string(), B.to_string()],
        }),
        blacklist: Vec::new(),
    }
}

/// 临时目录中的 `selftest.json`，测完删除目录
struct TempCache(PathBuf);

impl TempCache {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("rummage-run-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        TempCache(dir)
    }

    fn path(&self) -> PathBuf {
        self.0.join("cache").join("selftest.json")
    }
}

impl Drop for TempCache {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn options(cache_file: PathBuf, auto: bool, only: Option<String>) -> Options {
    Options {
        exe: PathBuf::from("不存在的程序.exe"),
        golden_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/golden"),
        cpu_runtime: PathBuf::new(),
        // 没有显卡运行库：要测的显卡在启动推理进程前就失败
        vulkan_runtime: None,
        model: PathBuf::new(),
        mmproj: PathBuf::new(),
        cache_file,
        use_cache: false,
        save: true,
        images: true,
        auto,
        cancel: Arc::default(),
        only,
        video: VideoSpeed {
            window_s: 2,
            frames: 2,
            audio: false,
        },
    }
}

/// 运行检测，记下哪些候选显示过“检测中”
fn run_with(
    opts: &Options,
    candidates: &[Candidate],
    disabled: &[DeviceKey],
) -> (Result<SelftestReport, Error>, Vec<usize>) {
    let mut testing = Vec::new();
    let res = run(
        opts,
        candidates,
        &params::defaults(),
        disabled,
        &mut |i, done| {
            if done.is_none() {
                testing.push(i);
            }
        },
    );
    (res, testing)
}

/// 真正尝试测过（没有因记录或停用被跳过）：读不到系统日志时为已跳过，否则因缺少显卡运行库未通过
fn attempted(r: &CandidateReport) -> bool {
    match r.status {
        Status::Skipped => r.reason.as_deref() == Some(NO_EVENT_LOG_REASON),
        Status::Failed => r.reason.as_deref().is_some_and(|x| x.contains("缺少显卡")),
        _ => false,
    }
}

#[test]
fn only_keeps_others_and_replaces_target() {
    let tmp = TempCache::new("only");
    let path = tmp.path();
    let mut cache = cached_three();
    // 解除屏蔽后重测甲：它此前还有未通过记录（解除屏蔽不删），本次照记录判为未通过，不初始化
    cache.record(now(A), Record::Failed, "余弦低于门槛。");
    cache.save(&path).unwrap();
    let before = cache.cached.clone().unwrap();
    let candidates = [Candidate::Cpu, cand(A), cand(B)];
    let opts = options(path.clone(), false, Some(now(A).device_id()));
    let (res, testing) = run_with(&opts, &candidates, &[]);
    let report = res.unwrap();
    // 只有甲显示过“检测中”；其余设备（含 CPU）不测，结果（状态、耗时、原因）与缓存完全相同、顺序不变
    assert_eq!(testing, [1]);
    assert_eq!(report.candidates[0], before.report.candidates[0]);
    assert_eq!(report.candidates[2], before.report.candidates[2]);
    assert_eq!(report.candidates[1].status, Status::Failed);
    assert!(
        report.candidates[1]
            .reason
            .as_deref()
            .unwrap()
            .contains("不达标")
    );
    // 保存时只替换缓存中甲这一项：其余的键与结果照旧，甲的键改为本次的键；按新结果重新选最快的
    let after = CacheFile::load(&path).cached.unwrap();
    assert_eq!(
        after.keys,
        [before.keys[0].clone(), now(A), before.keys[2].clone()]
    );
    assert_eq!(after.report.candidates[0], before.report.candidates[0]);
    assert_eq!(after.report.candidates[2], before.report.candidates[2]);
    assert_eq!(after.report.candidates[1].status, Status::Failed);
    assert_eq!(after.report.selected, Some(2));
    assert_eq!((after.mmproj, &after.adapters), (true, &before.adapters));
    // 只测一块不算用户同意解除其他记录，未通过记录照旧
    let back = CacheFile::load(&path);
    assert_eq!(back.blacklisted(&now(A)).unwrap().kind, Record::Failed);
}

#[test]
fn only_missing_or_unsaved() {
    let tmp = TempCache::new("only-missing");
    let path = tmp.path();
    let mut cache = cached_three();
    cache.record(now(A), Record::Failed, "余弦低于门槛。");
    cache.save(&path).unwrap();
    let bytes = fs::read(&path).unwrap();
    // 要测的显卡不在系统中：报错，不改缓存
    let candidates = [Candidate::Cpu, cand(B)];
    let opts = options(path.clone(), false, Some(now(A).device_id()));
    let (res, testing) = run_with(&opts, &candidates, &[]);
    assert!(matches!(res, Err(Error::GpuNotFound(_))));
    assert!(testing.is_empty());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    // 不保存（列显卡卡住）：与手动检测一样整份缓存丢弃，记录照旧
    let candidates = [Candidate::Cpu, cand(A), cand(B)];
    let mut opts = options(path.clone(), false, Some(now(A).device_id()));
    opts.save = false;
    run_with(&opts, &candidates, &[]).0.unwrap();
    let back = CacheFile::load(&path);
    assert!(back.cached.is_none());
    assert_eq!(back.blacklisted(&now(A)).unwrap().kind, Record::Failed);
}

#[test]
fn auto_reuses_and_skips() {
    let tmp = TempCache::new("auto");
    let path = tmp.path();
    let mut cache = cached_three();
    // 乙在旧驱动（0.9）时被屏蔽过：驱动已更新，自动检测仍跳过（重测须征得用户同意）
    let old_b = key_with(&cand(B), Some("0.9".to_string()));
    cache.record(old_b.clone(), Record::Blacklisted, "设备丢失。");
    cache.save(&path).unwrap();
    let before = cache.cached.clone().unwrap();
    // 甲本次运行停用（拔出时记下的键，驱动版本未知；同一设备即算）；丙是新插上的
    let candidates = [Candidate::Cpu, cand(A), cand(B), cand(C)];
    let disabled = [now(A)];
    let opts = options(path.clone(), true, None);
    let (res, testing) = run_with(&opts, &candidates, &disabled);
    let report = res.unwrap();
    // 只有新显卡显示“检测中”；CPU 沿用上次的结果
    assert_eq!(testing, [3]);
    assert_eq!(report.candidates[0], before.report.candidates[0]);
    let a = &report.candidates[1];
    assert_eq!(
        (a.status, a.reason.as_deref()),
        (Status::Skipped, Some(SESSION_DISABLED_REASON))
    );
    assert_eq!(a.image_ms, None);
    let b = &report.candidates[2];
    assert_eq!((b.status, b.reason.as_deref()), (Status::Blacklisted, None));
    assert!(
        attempted(&report.candidates[3]),
        "{:?}",
        report.candidates[3]
    );
    // 整份写入；屏蔽记录不因自动检测删除；甲被跳过，下次启动算过期
    let back = CacheFile::load(&path);
    assert!(back.ever_blacklisted(&now(B)));
    assert_eq!(back.blacklisted(&old_b).unwrap().reason, "设备丢失。");
    let c = back.cached.as_ref().unwrap();
    assert_eq!(c.keys.len(), 4);
    assert_eq!(c.report.candidates[1].status, Status::Skipped);
    assert!(back.previous(&now(A), true).is_none());
}

#[test]
fn manual_retests_after_driver_change() {
    let tmp = TempCache::new("manual");
    let path = tmp.path();
    let mut cache = cached_three();
    // 甲按现在的键（驱动未变）屏蔽：手动检测也不测，显示为已屏蔽、不另写原因
    cache.record(now(A), Record::Blacklisted, "设备丢失。");
    // 乙只在旧驱动时屏蔽、丙在旧驱动时未通过：驱动已更新，手动检测重测
    cache.record(
        key_with(&cand(B), Some("0.9".to_string())),
        Record::Blacklisted,
        "设备丢失。",
    );
    cache.record(
        key_with(&cand(C), Some("0.9".to_string())),
        Record::Failed,
        "余弦低于门槛。",
    );
    cache.save(&path).unwrap();
    // 不测 CPU（会启动推理进程），只给显卡
    let candidates = [cand(A), cand(B), cand(C)];
    let opts = options(path.clone(), false, None);
    let (res, testing) = run_with(&opts, &candidates, &[]);
    let report = res.unwrap();
    assert_eq!(testing, [0, 1, 2]);
    let a = &report.candidates[0];
    assert_eq!((a.status, a.reason.as_deref()), (Status::Blacklisted, None));
    assert!(
        attempted(&report.candidates[1]),
        "{:?}",
        report.candidates[1]
    );
    assert!(
        attempted(&report.candidates[2]),
        "{:?}",
        report.candidates[2]
    );
    // 没有通过：记录都不动（没能真正测试，不新增记录）
    let back = CacheFile::load(&path);
    assert_eq!(back.blacklist.len(), 3);
    assert_eq!(back.records(&[]).blocked.len(), 2);
}
