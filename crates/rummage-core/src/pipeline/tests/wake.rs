//! 睡眠与唤醒验证：验证通过、睡眠中手动暂停优先显示、跳过已失效的引擎、GPU 出错时改用 CPU、跨过睡眠的失败不屏蔽、验证期间再次挂起、
//! 余弦不达标时重启后改用 CPU。
use super::*;

/// 唤醒验证用的样本：与替身对同一查询给出的向量一致
fn wake_sample() -> selftest::Sample {
    let text = "唤醒样本";
    selftest::Sample {
        id: "q01".into(),
        modality: selftest::Modality::Text,
        input: Input::Query(text.into()),
        want: stub_vector(text.len()),
    }
}

#[test]
fn wake_verification_passes() {
    let mut e = env(&[("a.txt", "甲".as_bytes())]);
    e.start(true, true);
    e.wait_idle(0);
    let p = e.pipeline.as_ref().unwrap();
    let engine = lock(&p.shared.engine).clone().unwrap();
    let mut rx = e.events.subscribe();
    p.suspend();
    // 睡眠时报为 sleeping 并立即广播（不等写库线程）
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match rx.try_recv() {
            Ok(Event::IndexProgress(s)) if s.paused == Some(PauseReason::Sleeping) => break,
            Ok(_) => {}
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
        assert!(Instant::now() < deadline, "挂起时应立即广播");
    }
    // 睡眠期间新出现的作业不领取
    e.put("b.txt", "乙".as_bytes());
    e.scan();
    thread::sleep(Duration::from_millis(300));
    assert!(e.job("b.txt").is_some_and(|j| j.0 == "pending"));
    p.resume(Some(&wake_sample()));
    assert_eq!(e.stub.restarts.load(Ordering::Relaxed), 0);
    assert!(Arc::ptr_eq(
        &lock(&p.shared.engine).clone().unwrap(),
        &engine
    ));
    // 恢复时同样立即广播，最后一条不再是 sleeping
    let mut last = None;
    while let Ok(Event::IndexProgress(s)) = rx.try_recv() {
        last = Some(s.paused);
    }
    assert_eq!(last, Some(None));
    e.wait_idle(0);
    assert!(e.stored("b.txt"));
}

#[test]
fn manual_pause_shown_over_sleep() {
    let mut e = env(&[]);
    e.start(true, true);
    e.wait_idle(0);
    let p = e.pipeline.as_ref().unwrap();
    p.suspend();
    let manual = PauseReason::Manual { until: None };
    p.set_paused(Some(manual));
    assert_eq!(e.status().paused, Some(manual));
    p.set_paused(None);
    assert_eq!(e.status().paused, Some(PauseReason::Sleeping));
    p.resume(Some(&wake_sample()));
    assert_eq!(e.status().paused, None);
}

#[test]
fn wake_skips_failed_engine() {
    let mut e = env(&[]);
    e.start(true, true);
    e.wait_idle(0);
    let p = e.pipeline.as_ref().unwrap();
    let cpu = lock(&p.shared.engine).clone().unwrap();
    let engine = gpu_engine(&e.stub, &cpu);
    engine.failed.store(true, Ordering::Relaxed);
    *lock(&p.shared.engine) = Some(engine);
    p.suspend();
    p.resume(Some(&wake_sample()));
    // 已失效的引擎不验证、不重启
    assert_eq!(e.stub.queries.load(Ordering::Relaxed), 0);
    assert_eq!(e.stub.restarts.load(Ordering::Relaxed), 0);
    assert!(!lock(&p.shared.sleep).held());
}

#[test]
fn wake_device_lost_falls_back_without_restart() {
    let mut e = env(&[]);
    e.start(true, true);
    e.wait_idle(0);
    let p = e.pipeline.as_ref().unwrap();
    let cache = e.data.path().join("cache").join("selftest.json");
    let cpu = lock(&p.shared.engine).clone().unwrap();
    let engine = gpu_engine(&e.stub, &cpu);
    *lock(&p.shared.engine) = Some(engine.clone());
    e.stub.lose.store(true, Ordering::Relaxed);
    p.suspend();
    p.resume(Some(&wake_sample()));
    // 设备丢失不重启（不重新初始化已丢失的显卡），直接本次改用 CPU，不写屏蔽记录
    assert_eq!(e.stub.queries.load(Ordering::Relaxed), 1);
    assert_eq!(e.stub.restarts.load(Ordering::Relaxed), 0);
    assert!(engine.failed.load(Ordering::Relaxed));
    assert!(lock(&p.shared.disabled).all);
    assert!(!cache.exists());
}

#[test]
fn wake_gpu_error_falls_back_without_restart() {
    let mut e = env(&[]);
    e.start(true, true);
    e.wait_idle(0);
    let p = e.pipeline.as_ref().unwrap();
    let cache = e.data.path().join("cache").join("selftest.json");
    let cpu = lock(&p.shared.engine).clone().unwrap();
    let engine = gpu_engine(&e.stub, &cpu);
    *lock(&p.shared.engine) = Some(engine.clone());
    // 有驱动事件也不屏蔽：验证中的错误都只在本次运行内停用
    e.driver_events.store(true, Ordering::Relaxed);
    e.stub.fail_query.store(true, Ordering::Relaxed);
    p.suspend();
    p.resume(Some(&wake_sample()));
    // 超时、崩溃等报错：GPU 不重启，本次改用 CPU
    assert_eq!(e.stub.queries.load(Ordering::Relaxed), 1);
    assert_eq!(e.stub.restarts.load(Ordering::Relaxed), 0);
    assert!(engine.failed.load(Ordering::Relaxed));
    assert!(lock(&p.shared.disabled).all);
    assert!(!cache.exists());
    assert!(!lock(&p.shared.sleep).held());
}

#[test]
fn failure_across_sleep_not_blacklisted() {
    let mut e = env(&[]);
    e.start(true, true);
    e.wait_idle(0);
    let p = e.pipeline.as_ref().unwrap();
    let shared = &p.shared;
    let cache = e.data.path().join("cache").join("selftest.json");
    let cpu = lock(&shared.engine).clone().unwrap();
    let lost = || EmbedError::DeviceLost("device lost".into());

    // 睡眠前开始的请求，醒来后才报设备丢失：只在本次运行内停用
    let start = shared.start();
    p.suspend();
    p.resume(None);
    let engine = gpu_engine(&e.stub, &cpu);
    *lock(&shared.engine) = Some(engine.clone());
    shared.engine_failed(&engine, &lost(), start);
    assert!(engine.failed.load(Ordering::Relaxed));
    assert!(lock(&shared.disabled).all);
    assert!(!cache.exists());

    // 睡眠中出错：同样不写记录
    p.suspend();
    let engine = gpu_engine(&e.stub, &cpu);
    *lock(&shared.engine) = Some(engine.clone());
    shared.engine_failed(&engine, &lost(), shared.start());
    assert!(engine.failed.load(Ordering::Relaxed));
    assert!(!cache.exists());

    // 醒来后开始的请求：设备丢失照常持久屏蔽
    p.resume(None);
    let engine = gpu_engine(&e.stub, &cpu);
    *lock(&shared.engine) = Some(engine.clone());
    shared.engine_failed(&engine, &lost(), shared.start());
    assert!(cache.exists());
}

#[test]
fn resuspend_during_verification_keeps_hold() {
    let mut e = env(&[]);
    e.start(true, true);
    e.wait_idle(0);
    let p = e.pipeline.as_ref().unwrap();
    *lock(&e.stub.hold_queries) = true;
    p.suspend();
    thread::scope(|scope| {
        let verifying = scope.spawn(|| p.resume(Some(&wake_sample())));
        let deadline = Instant::now() + Duration::from_secs(10);
        while e.stub.queries.load(Ordering::Relaxed) == 0 {
            assert!(Instant::now() < deadline, "没有开始验证");
            thread::sleep(Duration::from_millis(10));
        }
        // 验证期间的查询先等验证结束
        let query = scope.spawn(|| p.query_engine().is_some());
        thread::sleep(Duration::from_millis(300));
        assert!(!query.is_finished());
        // 验证期间再次挂起
        p.suspend();
        *lock(&e.stub.hold_queries) = false;
        e.stub.cv.notify_all();
        verifying.join().unwrap();
        assert!(query.join().unwrap());
    });
    // 期间有新的挂起：仍不领取作业，报为 sleeping
    assert!(lock(&p.shared.sleep).held());
    assert_eq!(e.status().paused, Some(PauseReason::Sleeping));
    p.resume(Some(&wake_sample()));
    assert!(!lock(&p.shared.sleep).held());
    assert_eq!(e.status().paused, None);
}

#[test]
fn wake_low_cosine_restarts_then_falls_back_to_cpu() {
    let mut e = env(&[]);
    e.start(true, true);
    e.wait_idle(0);
    let p = e.pipeline.as_ref().unwrap();
    let cache = e.data.path().join("cache").join("selftest.json");
    let cpu = lock(&p.shared.engine).clone().unwrap();
    let gpu_engine = || {
        Arc::new(Engine {
            backend: e.stub.clone(),
            gpu: Some(fake_gpu()),
            device: "测试显卡".into(),
            images: true,
            audio: true,
            video: true,
            text_space: cpu.text_space.clone(),
            cfg: cpu.cfg.clone(),
            failed: AtomicBool::new(false),
            driver_failed: AtomicBool::new(false),
            retired: AtomicBool::new(false),
            released: Released(Arc::new(AtomicBool::new(false))),
        })
    };

    // 第一次余弦不达标、重启后通过：引擎照用
    let engine = gpu_engine();
    *lock(&p.shared.engine) = Some(engine.clone());
    e.stub.bad_queries.store(1, Ordering::Relaxed);
    p.suspend();
    p.resume(Some(&wake_sample()));
    assert_eq!(e.stub.restarts.load(Ordering::Relaxed), 1);
    assert!(lock(&p.shared.engine).is_some());
    assert!(!engine.failed.load(Ordering::Relaxed));
    assert!(!lock(&p.shared.disabled).all);

    // 重启后仍不达标：本次运行改用 CPU，不写屏蔽记录
    e.stub.bad_queries.store(2, Ordering::Relaxed);
    p.suspend();
    p.resume(Some(&wake_sample()));
    assert_eq!(e.stub.restarts.load(Ordering::Relaxed), 2);
    assert!(engine.failed.load(Ordering::Relaxed));
    assert!(lock(&p.shared.disabled).all);
    assert!(!cache.exists());
    // 调度线程随即新建 CPU 后端
    let deadline = Instant::now() + Duration::from_secs(5);
    while lock(&p.shared.engine)
        .as_ref()
        .is_none_or(|e| e.gpu.is_some())
    {
        assert!(Instant::now() < deadline, "没有改用 CPU");
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        e.status().backend.as_deref(),
        Some(rummage_embed::cpu_name())
    );
}
