//! 引擎的挂起与重建：绑定旧引擎（失效或已丢弃）的作业放回、自检期间挂起与 `reload`、引导未完成时挂起、关闭图片索引。
use super::*;

#[test]
fn stale_engine_jobs_released() {
    let mut e = env(&[
        ("a.txt", "甲".as_bytes()),
        ("b.txt", "乙".as_bytes()),
        ("c.txt", "丙".as_bytes()),
    ]);
    // 第一次推理设备丢失；之后无法再创建后端，作业停在原处便于核对
    e.max_builds = 1;
    e.stub.lose.store(true, Ordering::Relaxed);
    *lock(&e.stub.hold) = true;
    e.start(true, true);
    wait_entered(&e);
    // 让其余文件预处理完、排在推理线程前
    thread::sleep(Duration::from_millis(500));
    release_hold(&e);
    let s = e.wait_until(|s| s.state == IndexState::Error && s.pending == 3);
    let deadline = Instant::now() + Duration::from_secs(10);
    while e.count("SELECT COUNT(*) FROM jobs WHERE stage = 'pending'") < 3 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    // 只有出错时正在推理的那个计一次失败，排队中的放回、不计失败
    assert_eq!(
        e.strings("SELECT stage || ' ' || attempts FROM jobs ORDER BY attempts"),
        ["pending 0", "pending 0", "pending 1"]
    );
    assert!(
        e.strings("SELECT last_error FROM jobs WHERE attempts = 1")[0].contains("显卡出现错误")
    );
    assert_eq!(
        s.error.as_deref(),
        Some(
            "内容识别无法启动。目前只能按文件名搜索。请重新启动 Rummage，或在「设置 → 设备」中选择使用 CPU。"
        )
    );
}

#[test]
fn selftest_hold_and_reload() {
    let mut e = env(&[("a.txt", b"a"), ("b.txt", b"b")]);
    e.start(true, false);
    e.wait_idle(0);
    let p = e.pipeline.as_ref().unwrap();
    let builds = e.builds.load(Ordering::Relaxed);
    // 自检期间：丢弃引擎、等它释放，不领取新作业
    let released = p.hold(Hold::Selftest, true);
    assert!(released.is_some());
    assert!(p.wait_released(released, Duration::from_secs(5)));
    e.put("c.txt", b"c");
    e.scan();
    thread::sleep(Duration::from_millis(300));
    let s = e.status();
    assert_eq!((s.pending, s.backend.clone()), (1, None), "{s:?}");
    assert_eq!(e.builds.load(Ordering::Relaxed), builds);
    assert!(p.query_engine().is_none());
    // 自检完成：按新的选择重建引擎并继续
    p.set_preferred_device(rummage_proto::CPU_DEVICE);
    p.reload();
    p.hold(Hold::Selftest, false);
    let s = e.wait_idle(0);
    assert_eq!(s.backend.as_deref(), Some(rummage_embed::cpu_name()));
    assert_eq!(e.builds.load(Ordering::Relaxed), builds + 1);
    assert!(e.stored("c.txt"));
}

#[test]
fn setup_pending_holds_until_finished() {
    let mut e = env(&[("a.txt", b"a")]);
    lock(&e.db).meta_set(ONBOARDED_KEY, "0").unwrap();
    e.start(true, false);
    thread::sleep(Duration::from_millis(300));
    assert_eq!(e.status().pending, 1);
    assert_eq!(e.builds.load(Ordering::Relaxed), 0);
    // 引导完成后才开始
    e.pipeline.as_ref().unwrap().hold(Hold::Setup, false);
    e.wait_idle(0);
    assert!(e.stored("a.txt"));
}

#[test]
fn images_off_skips_image_jobs() {
    let corgi = fs::read(CORGI).unwrap();
    let mut e = env(&[("a.txt", "内容".as_bytes()), ("b.jpg", &corgi)]);
    e.images_param = Some(false);
    e.start(true, true);
    // mmproj 可用但关闭了图片：不领取图片作业，也不报缺 mmproj
    let s = e.wait_until(|s| s.state == IndexState::Idle && s.done == 1);
    assert_eq!(s.pending, 1);
    assert!(e.stored("a.txt"));
    assert_eq!(e.job("b.jpg").unwrap().0, "pending");
}

#[test]
fn reload_records_released_engine() {
    let mut e = env(&[("a.txt", b"a")]);
    e.start(true, false);
    e.wait_idle(0);
    let p = e.pipeline.as_ref().unwrap();
    // 有人还拿着引擎时，reload 丢弃的引擎没有释放，wait_released 等不到
    let engine = p.query_engine().unwrap();
    p.reload();
    assert!(!p.wait_released(None, Duration::from_millis(200)));
    drop(engine);
    assert!(p.wait_released(None, Duration::from_secs(5)));
    // 模型任务期间同样暂停
    p.hold(Hold::Models, true);
    assert!(p.held());
    p.hold(Hold::Models, false);
    assert!(!p.held());
}

#[test]
fn retired_engine_jobs_released() {
    let files: Vec<(String, Vec<u8>)> = (0..8)
        .map(|i| (format!("{i}.txt"), format!("内容{i}").into_bytes()))
        .collect();
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    let mut e = env(&refs);
    e.threads = Some(1);
    *lock(&e.stub.hold) = true;
    e.start(true, true);
    wait_entered(&e);
    thread::sleep(Duration::from_millis(500));
    let p = e.pipeline.as_ref().unwrap();
    let old = lock(&p.shared.engine).clone().unwrap();
    let claimed = e.count("SELECT COUNT(*) FROM jobs WHERE stage <> 'pending'");
    // 设备检测开始时丢弃引擎：正在推理的文件算完，已领取、还没推理的放回 pending，不在旧引擎上算
    p.hold(Hold::Selftest, true);
    release_hold(&e);
    let s = e.wait_until(|s| s.state == IndexState::Idle && s.done + s.pending == 8);
    assert!(s.done > 0 && (s.done as i64) < claimed, "{s:?} {claimed}");
    assert_eq!(s.failed, 0);
    // 检测结束后由新引擎接着算
    p.hold(Hold::Selftest, false);
    let s = e.wait_idle(0);
    assert_eq!(s.done, 8);
    assert!(!Arc::ptr_eq(&lock(&p.shared.engine).clone().unwrap(), &old));
}
