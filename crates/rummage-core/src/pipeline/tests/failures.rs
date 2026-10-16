//! 失败处理：失败计数与黑名单、超时与 panic、核心崩溃后的恢复、主模型缺失、根目录移除后不写入、停止时放回作业、
//! 批量写入失败时逐个重试、放弃等待的任务线程过多时暂停领取。
use super::*;

#[test]
fn failures_blacklist_and_recovery() {
    let mut e = env(&[("bad.txt", "这一段会 FAIL。".as_bytes())]);
    e.start(true, true);
    let s = e.wait_idle(0);
    assert_eq!(s.failed, 1);
    assert_eq!(
        e.job("bad.txt"),
        Some(("failed".into(), 3, Some("识别内容时出错。".into())))
    );
    assert_eq!(e.count("SELECT COUNT(*) FROM blacklist"), 1);

    // 只改修改时间：扫描重新排队，命中黑名单
    let path = e.root.path().join("bad.txt");
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(60))
        .unwrap();
    e.scan();
    e.wait_idle(0);
    // 显示黑名单中记下的原因，而不是笼统的“连续失败 3 次”
    assert_eq!(
        e.job("bad.txt"),
        Some(("failed".into(), 0, Some("识别内容时出错。".into())))
    );

    // 内容变了：quick_hash 不同，重新处理
    e.put("bad.txt", "已经修好了。".as_bytes());
    e.scan();
    let s = e.wait_idle(0);
    assert_eq!((s.done, s.failed), (1, 0));
    assert!(e.stored("bad.txt"));
}

#[test]
fn timeout_and_panic_fail() {
    let mut e = env(&[
        ("slow.txt", "慢".as_bytes()),
        ("panic.txt", "崩".as_bytes()),
        ("ok.txt", "好".as_bytes()),
    ]);
    e.timeout = Duration::from_millis(300);
    e.start(true, true);
    let s = e.wait_idle(0);
    assert_eq!((s.done, s.failed), (1, 2));
    assert_eq!(
        e.job("slow.txt"),
        Some((
            "failed".into(),
            3,
            Some("处理此文件超过 0.3 秒，已放弃。".into())
        ))
    );
    assert_eq!(
        e.job("panic.txt"),
        Some((
            "failed".into(),
            3,
            Some("处理此文件时程序出错，已跳过。".into())
        ))
    );
    // 已算出 quick_hash，进黑名单
    assert_eq!(e.count("SELECT COUNT(*) FROM blacklist"), 2);
}

#[test]
fn crash_recovery_and_missing_model() {
    let mut e = env(&[("a.txt", "内容".as_bytes())]);
    e.conn()
        .execute("UPDATE jobs SET stage = 'embedding'", [])
        .unwrap();
    e.start(false, false);
    let deadline = Instant::now() + Duration::from_secs(10);
    while e.status().state != IndexState::NoModel {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(e.status().backend, None);
    assert_eq!(
        e.job("a.txt"),
        Some((
            "pending".into(),
            1,
            Some("上次处理此文件时 Rummage 意外退出。".into())
        ))
    );
    assert_eq!(e.builds.load(Ordering::Relaxed), 0);

    // 模型下载后唤醒即开始
    fs::write(e.model("m.gguf"), b"x").unwrap();
    e.pipeline.as_ref().unwrap().wake();
    e.wait_idle(0);
    assert_eq!(e.job("a.txt"), Some(("stored".into(), 0, None)));
}

#[test]
fn removed_root_not_written() {
    let mut e = env(&[("a.txt", "内容".as_bytes())]);
    *lock(&e.stub.hold) = true;
    e.start(true, true);
    let deadline = Instant::now() + Duration::from_secs(10);
    while e.stub.entered.load(Ordering::Relaxed) == 0 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    lock(&e.db).remove_root(e.root_id).unwrap();
    *lock(&e.stub.hold) = false;
    e.stub.cv.notify_all();
    let s = e.wait_idle(0);
    assert_eq!(s.total, 0);
    assert_eq!(e.count("SELECT COUNT(*) FROM files"), 0);
    assert_eq!(e.count("SELECT COUNT(*) FROM segments"), 0);
    assert_eq!(e.count("SELECT COUNT(*) FROM spaces"), 0);
}

#[test]
fn stop_releases_and_claims_nothing() {
    let files: Vec<(String, Vec<u8>)> = (0..6)
        .map(|i| (format!("{i}.txt"), format!("内容{i}").into_bytes()))
        .collect();
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    let mut e = env(&refs);
    *lock(&e.stub.hold) = true;
    e.start(true, true);
    wait_entered(&e);
    let active = "SELECT COUNT(*) FROM jobs WHERE stage IN ('preprocessing', 'embedding')";
    assert!(e.count(active) > 0);
    e.pipeline.as_ref().unwrap().stop();
    assert_eq!(e.count(active), 0);
    // 推理继续完成：结果不再写入，也不再领出新作业
    release_hold(&e);
    thread::sleep(Duration::from_millis(1500));
    assert_eq!(e.count(active), 0);
    assert_eq!(
        e.count("SELECT COUNT(*) FROM jobs WHERE stage = 'pending' AND attempts = 0"),
        6
    );
    assert_eq!(e.count("SELECT COUNT(*) FROM segments"), 0);
}

#[test]
fn batch_write_failure_retried_per_file() {
    let mut e = env(&[
        ("good.txt", "正常的内容。".as_bytes()),
        ("bad.txt", "这里有坏写入的内容。".as_bytes()),
    ]);
    e.conn()
        .execute_batch(
            "CREATE TRIGGER fail_write BEFORE INSERT ON segments WHEN NEW.text LIKE '%坏写入%'
             BEGIN SELECT RAISE(ABORT, '模拟写入失败'); END;",
        )
        .unwrap();
    e.start(true, true);
    let s = e.wait_idle(0);
    assert_eq!((s.done, s.failed), (1, 1));
    assert!(e.stored("good.txt"));
    let (stage, attempts, error) = e.job("bad.txt").unwrap();
    assert_eq!((stage.as_str(), attempts), ("failed", 3));
    assert_eq!(error.as_deref(), Some(super::prep::reason::DB_REASON));
    assert_eq!(e.count("SELECT COUNT(*) FROM blacklist"), 1);
}

#[test]
fn stuck_threads_pause_claiming() {
    let mut e = env(&[
        ("stuck1.txt", "一".as_bytes()),
        ("stuck2.txt", "二".as_bytes()),
    ]);
    e.threads = Some(1);
    e.timeout = Duration::from_millis(150);
    e.start(true, true);
    let s = e.wait_idle(0);
    assert_eq!(s.failed, 2);
    // 一个预处理线程：超时的线程没退出前不领新作业，同时在跑的始终只有一个
    assert_eq!(STUCK_MAX.load(Ordering::SeqCst), 1);
    assert_eq!(
        e.job("stuck1.txt").unwrap().2.as_deref(),
        Some("处理此文件超过 0.15 秒，已放弃。")
    );
}
