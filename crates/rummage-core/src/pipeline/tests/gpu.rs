//! 推理后端失效：设备丢失后重建后端、按证据屏蔽 GPU、找不到选中的显卡时改用 CPU 但不屏蔽、
//! 显卡出问题时改用剩下的设备中最快的、显卡算出 NaN 时只在本次运行停用这块显卡（不写记录）且不计文件失败。
use super::*;

#[test]
fn device_lost_rebuilds_backend() {
    let mut e = env(&[("a.txt", "内容".as_bytes())]);
    e.stub.lose.store(true, Ordering::Relaxed);
    e.start(true, true);
    e.wait_idle(0);
    assert_eq!(e.job("a.txt"), Some(("stored".into(), 0, None)));
    assert_eq!(e.builds.load(Ordering::Relaxed), 2);
}

#[test]
fn device_lost_blacklists_gpu() {
    let mut e = env(&[]);
    e.start(true, true);
    e.wait_idle(0);
    let shared = &e.pipeline.as_ref().unwrap().shared;
    let cache = e.data.path().join("cache").join("selftest.json");
    let cpu = lock(&shared.engine).clone().unwrap();
    let gpu_engine = |gpu| {
        Arc::new(Engine {
            backend: e.stub.clone(),
            gpu: Some(gpu),
            device: "测试显卡".into(),
            images: true,
            audio: true,
            video: true,
            text_space: cpu.text_space.clone(),
            cfg: cpu.cfg.clone(),
            failed: AtomicBool::new(false),
            driver_failed: AtomicBool::new(false),
            released: Released(Arc::new(AtomicBool::new(false))),
        })
    };
    let blacklist = || -> Vec<serde_json::Value> {
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&cache).unwrap()).unwrap();
        json["blacklist"].as_array().unwrap().clone()
    };

    // 推理出错（不是后端失效）：引擎照用
    let engine = gpu_engine(fake_gpu());
    *lock(&shared.engine) = Some(engine.clone());
    shared.engine_failed(&engine, &EmbedError::Inference("x".into()), shared.start());
    assert!(lock(&shared.engine).is_some());
    assert!(!engine.failed.load(Ordering::Relaxed));
    assert!(!cache.exists());

    // 后端失效（推理子进程崩溃、超时）、系统日志中没有驱动事件：只在本次运行内停用 GPU，不写记录
    shared.engine_failed(
        &engine,
        &EmbedError::Unavailable("显卡后端失效".into()),
        shared.start(),
    );
    assert!(lock(&shared.engine).is_none());
    assert!(engine.failed.load(Ordering::Relaxed));
    // 无法排除驱动原因：本次运行停用所有显卡，只用 CPU；记下出问题的设备
    assert!(lock(&shared.disabled).all);
    assert!(!cache.exists());
    // 记入本次运行停用的集合，重新自检时跳过它
    assert_eq!(
        lock(&shared.disabled).keys,
        [selftest::report_key(&fake_gpu())]
    );
    assert_eq!(lock(&shared.disabled).failed, Some(fake_gpu().device_id()));

    // 后端失效且有驱动事件：持久屏蔽
    e.driver_events.store(true, Ordering::Relaxed);
    let engine = gpu_engine(fake_gpu());
    *lock(&shared.engine) = Some(engine.clone());
    shared.engine_failed(
        &engine,
        &EmbedError::Unavailable("显卡后端失效".into()),
        shared.start(),
    );
    let list = blacklist();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["key"]["description"], "测试显卡");
    assert_eq!(list[0]["key"]["driver"], "1.0");
    let reason = list[0]["reason"].as_str().unwrap();
    assert!(
        reason.contains("显卡后端失效") && reason.contains("4101"),
        "{reason}"
    );
    e.driver_events.store(false, Ordering::Relaxed);

    // 设备丢失（没有驱动事件也一样）：同一个键，更新原因
    let engine = gpu_engine(fake_gpu());
    *lock(&shared.engine) = Some(engine.clone());
    shared.engine_failed(
        &engine,
        &EmbedError::DeviceLost("device lost".into()),
        shared.start(),
    );
    let list = blacklist();
    assert_eq!(list.len(), 1);
    assert!(list[0]["reason"].as_str().unwrap().contains("device lost"));
    // 重复出错不重复记录
    assert_eq!(lock(&shared.disabled).keys.len(), 1);

    // 同一个引擎先报显存不足、随后设备丢失：仍按驱动错误下结论；之后再报的错不再重复下结论
    let engine = gpu_engine(fake_gpu());
    *lock(&shared.engine) = Some(engine.clone());
    shared.engine_failed(
        &engine,
        &EmbedError::OutOfMemory("oom".into()),
        shared.start(),
    );
    assert!(
        blacklist()[0]["reason"]
            .as_str()
            .unwrap()
            .contains("device lost")
    );
    shared.engine_failed(
        &engine,
        &EmbedError::DeviceLost("lost after oom".into()),
        shared.start(),
    );
    assert!(
        blacklist()[0]["reason"]
            .as_str()
            .unwrap()
            .contains("lost after oom")
    );
    shared.engine_failed(
        &engine,
        &EmbedError::DeviceLost("lost again".into()),
        shared.start(),
    );
    assert!(
        blacklist()[0]["reason"]
            .as_str()
            .unwrap()
            .contains("lost after oom")
    );

    // CPU 引擎设备丢失（不会发生，但也不写记录）
    *lock(&shared.engine) = Some(cpu.clone());
    shared.engine_failed(&cpu, &EmbedError::DeviceLost("x".into()), shared.start());
    assert_eq!(blacklist().len(), 1);
}

/// 找不到选中显卡的 GPU 后端
struct GoneGpu;

impl EmbeddingBackend for GoneGpu {
    fn embed(&self, _: Input, _: Priority) -> Result<Embedding, EmbedError> {
        Err(EmbedError::GpuNotFound("测试显卡".into()))
    }

    fn count_tokens(&self, _: &Input, _: Priority) -> Result<usize, EmbedError> {
        Err(EmbedError::GpuNotFound("测试显卡".into()))
    }
}

#[test]
fn missing_gpu_falls_back_without_blacklist() {
    let mut e = env(&[]);
    e.start(true, true);
    e.wait_idle(0);
    let p = e.pipeline.as_ref().unwrap();
    let shared = &p.shared;
    let cache = e.data.path().join("cache").join("selftest.json");
    let cpu = lock(&shared.engine).clone().unwrap();
    let engine = Arc::new(Engine {
        backend: Arc::new(GoneGpu),
        gpu: Some(fake_gpu()),
        device: "测试显卡".into(),
        images: true,
        audio: true,
        video: true,
        text_space: cpu.text_space.clone(),
        cfg: cpu.cfg.clone(),
        failed: AtomicBool::new(false),
        driver_failed: AtomicBool::new(false),
        released: Released(Arc::new(AtomicBool::new(false))),
    });
    drop(cpu);
    *lock(&shared.engine) = Some(engine.clone());
    // 即使系统日志中有驱动事件，拔出显卡也不屏蔽
    e.driver_events.store(true, Ordering::Relaxed);
    // 查询换用新建的 CPU 引擎算出向量，不退回只匹配文件名
    let mut q = p.query_engine().unwrap();
    assert!(Arc::ptr_eq(&q.0, &engine));
    let v = p.embed_query(&mut q, Input::Query("猫".into())).unwrap();
    assert!(!v.is_empty());
    assert!(q.0.gpu.is_none() && !Arc::ptr_eq(&q.0, &engine));
    assert!(engine.failed.load(Ordering::Relaxed));
    // 拔出不是驱动问题：只停用这块显卡，不停用其他显卡
    assert!(!lock(&shared.disabled).all);
    assert_eq!(lock(&shared.disabled).keys.len(), 1);
    // 不查系统日志、不写屏蔽记录（留时间给可能误起的后台判定线程）
    thread::sleep(Duration::from_millis(300));
    assert!(!cache.exists());
}

#[test]
fn cpu_only_during_auto_selftest() {
    let mut e = env(&[]);
    e.start(true, true);
    e.wait_idle(0);
    let p = e.pipeline.as_ref().unwrap();
    let shared = &p.shared;
    let cpu = lock(&shared.engine).clone().unwrap();
    // 用 CPU 的引擎照用，不必等它
    let released = p.cpu_only(true);
    assert!(p.wait_engine_released(released, Duration::ZERO));
    assert!(Arc::ptr_eq(lock(&shared.engine).as_ref().unwrap(), &cpu));
    p.cpu_only(false);
    // 用显卡的引擎丢弃，等它释放后才开始检测；之后新建的引擎只用 CPU（测试的后端工厂要求不用显卡）
    let gpu = Arc::new(Engine {
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
        released: Released(Arc::new(AtomicBool::new(false))),
    });
    *lock(&shared.engine) = Some(gpu.clone());
    let released = p.cpu_only(true);
    // 调度线程可能随即新建引擎，只能是 CPU 的
    assert!(
        lock(&shared.engine)
            .as_ref()
            .is_none_or(|e| e.gpu.is_none())
    );
    assert!(!p.wait_engine_released(released.clone(), Duration::from_millis(20)));
    drop(gpu);
    assert!(p.wait_engine_released(released, Duration::from_secs(1)));
    let during = p.query_engine().unwrap().0;
    assert!(during.gpu.is_none());
    // 结束后按新记录重建引擎，不暂停建索引
    p.cpu_only(false);
    assert!(
        lock(&shared.engine)
            .as_ref()
            .is_none_or(|e| !Arc::ptr_eq(e, &during))
    );
    assert!(!p.held());
}

/// 写一份自检记录：CPU 与显卡 A、B 都通过，A 最快、B 次之、CPU 最慢
fn write_gpu_cache(e: &Env) {
    let model = format!("{}+{}", space::MODEL.sha256, space::MMPROJ.sha256);
    let key = |description: &str, driver: &str| {
        serde_json::json!({
            "description": description, "pci_id": null, "driver": driver,
            "runtime": space::RUNTIME, "model": model,
        })
    };
    let report = |backend: &str, description: &str, driver: Option<&str>, ms: f64| {
        serde_json::json!({
            "backend": backend, "name": "x", "description": description, "pci_id": null,
            "driver": driver, "status": "passed", "reason": null, "samples": [],
            "init_ms": null, "transient": false, "driver_error": false, "text_ms": ms, "image_ms": ms, "audio_ms": ms, "video_ms": ms,
        })
    };
    let json = serde_json::json!({
        "cached": {
            "keys": [key("", ""), key("A", "1.0"), key("B", "1.0")],
            "report": {
                "candidates": [
                    report("cpu", "", None, 900.0),
                    report("vulkan", "A", Some("1.0"), 10.0),
                    report("vulkan", "B", Some("1.0"), 50.0),
                ],
                "selected": 1,
                "from_cache": false,
            },
            "mmproj": true,
            "adapters": [],
        },
        "blacklist": [],
    });
    let path = e.data.path().join("cache").join("selftest.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, json.to_string()).unwrap();
}

#[test]
fn failed_gpu_switches_by_driver_evidence() {
    let mut e = env(&[("a.txt", "内容".as_bytes())]);
    write_gpu_cache(&e);
    e.gpu = true;
    e.start(true, true);
    e.wait_idle(0);
    assert!(e.stored("a.txt"));
    let p = e.pipeline.as_ref().unwrap();
    let shared = &p.shared;
    let a = lock(&shared.engine).clone().unwrap();
    assert_eq!(a.device, "A");
    // A 找不到了（拔出，明确不是驱动问题）：只停用它，换剩下的设备中最快的 B
    shared.engine_failed(&a, &EmbedError::GpuNotFound("A".into()), shared.start());
    let b = p.query_engine().unwrap().0;
    assert_eq!(b.device, "B");
    assert!(!lock(&shared.disabled).all);
    // B 崩溃或超时（无法排除驱动原因）：停用所有显卡，只用 CPU
    let unavailable = EmbedError::Unavailable("显卡后端失效".into());
    shared.engine_failed(&b, &unavailable, shared.start());
    assert!(p.query_engine().unwrap().0.gpu.is_none());
    let d = lock(&shared.disabled);
    assert!(d.all);
    assert_eq!(d.failed.as_deref(), Some("gpu:B|"));
}

#[test]
fn driver_error_goes_straight_to_cpu() {
    let mut e = env(&[]);
    write_gpu_cache(&e);
    e.gpu = true;
    e.start(true, true);
    e.wait_idle(0);
    let p = e.pipeline.as_ref().unwrap();
    let shared = &p.shared;
    let a = p.query_engine().unwrap().0;
    assert_eq!(a.device, "A");
    // 查询中 A 崩溃：不先初始化 B，直接用 CPU 重算
    e.stub.fail_query.store(true, Ordering::Relaxed);
    let mut q = p.query_engine().unwrap();
    p.embed_query(&mut q, Input::Query("猫".into())).unwrap();
    assert!(q.0.gpu.is_none());
    assert_eq!(e.builds.load(Ordering::Relaxed), 2);
    assert!(lock(&shared.disabled).all);
}

/// 推理进程结束不了的后端
struct StuckGpu;

impl EmbeddingBackend for StuckGpu {
    fn embed(&self, _: Input, _: Priority) -> Result<Embedding, EmbedError> {
        Err(EmbedError::Stuck("推理进程在 10 秒内没有结束。".into()))
    }

    fn count_tokens(&self, _: &Input, _: Priority) -> Result<usize, EmbedError> {
        Err(EmbedError::Stuck("推理进程在 10 秒内没有结束。".into()))
    }
}

#[test]
fn stuck_gpu_falls_back_to_cpu() {
    let mut e = env(&[]);
    e.start(true, true);
    e.wait_idle(0);
    let p = e.pipeline.as_ref().unwrap();
    let shared = &p.shared;
    let cache = e.data.path().join("cache").join("selftest.json");
    let cpu = lock(&shared.engine).clone().unwrap();
    let engine = Arc::new(Engine {
        backend: Arc::new(StuckGpu),
        ..Arc::try_unwrap(gpu_engine(&e.stub, &cpu)).ok().unwrap()
    });
    drop(cpu);
    *lock(&shared.engine) = Some(engine.clone());
    // 显卡的推理进程结束不了：按驱动问题停用所有显卡，查询改用新建的 CPU 引擎算出向量
    let mut q = p.query_engine().unwrap();
    let v = p.embed_query(&mut q, Input::Query("猫".into())).unwrap();
    assert!(!v.is_empty() && q.0.gpu.is_none());
    assert!(lock(&shared.disabled).all);
    assert_ne!(e.status().state, IndexState::Error);
    // 并持久屏蔽这块显卡
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cache.exists() {
        assert!(Instant::now() < deadline, "应写入屏蔽记录");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn stuck_cpu_stops_content_recognition() {
    let mut e = env(&[]);
    e.start(true, true);
    e.wait_idle(0);
    let p = e.pipeline.as_ref().unwrap();
    let shared = &p.shared;
    let cpu = lock(&shared.engine).clone().unwrap();
    let engine = Arc::new(Engine {
        backend: Arc::new(StuckGpu),
        gpu: None,
        ..Arc::try_unwrap(gpu_engine(&e.stub, &cpu)).ok().unwrap()
    });
    drop(cpu);
    *lock(&shared.engine) = Some(engine.clone());
    let builds = e.builds.load(Ordering::Relaxed);
    let mut q = p.query_engine().unwrap();
    let err = p
        .embed_query(&mut q, Input::Query("猫".into()))
        .unwrap_err();
    assert!(matches!(err, EmbedError::Stuck(_)), "{err}");
    // CPU 的推理进程结束不了：本次运行不再建任何引擎，状态为出错并说明原因
    assert!(p.query_engine().is_none());
    assert_eq!(e.builds.load(Ordering::Relaxed), builds);
    let s = e.status();
    assert_eq!(s.state, IndexState::Error);
    assert!(s.error.unwrap().contains("无法结束"));
}
#[test]
fn gpu_nan_disables_only_this_run_without_counting_file() {
    let mut e = env(&[("坏.txt", "这一段会 NAN。".as_bytes())]);
    write_gpu_cache(&e);
    e.gpu = true;
    e.start(true, true);
    e.wait_idle(0);
    // A、B 上算出 NaN：只在本次运行停用这块显卡、换下一个设备，这个文件放回重算、不计失败；
    // CPU 上算出 NaN 才按文件失败计，3 次后不再重试。共推理 5 次
    assert_eq!(e.stub.entered.load(Ordering::Relaxed), 5);
    assert_eq!(
        e.job("坏.txt"),
        Some(("failed".into(), 3, Some("识别结果异常，无法使用。".into())))
    );
    // 不写持久记录
    let cache = e.data.path().join("cache").join("selftest.json");
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(cache).unwrap()).unwrap();
    assert!(json["blacklist"].as_array().unwrap().is_empty());
    let p = e.pipeline.as_ref().unwrap();
    let d = lock(&p.shared.disabled);
    assert!(!d.all && d.keys.len() == 2);
}

#[test]
fn retired_engine_failure_still_disables_gpu() {
    let mut e = env(&[]);
    e.start(true, true);
    e.wait_idle(0);
    let p = e.pipeline.as_ref().unwrap();
    let shared = &p.shared;
    let cache = e.data.path().join("cache").join("selftest.json");
    let cpu = lock(&shared.engine).clone().unwrap();
    // 已被 reload 等丢弃、还在处理作业的显卡引擎上出了驱动类错误：照样停用所有显卡并持久屏蔽，不动当前引擎
    let retired = gpu_engine(&e.stub, &cpu);
    shared.engine_failed(
        &retired,
        &EmbedError::DeviceLost("device lost".into()),
        shared.start(),
    );
    assert!(retired.failed.load(Ordering::Relaxed));
    assert!(lock(&shared.disabled).all);
    assert!(Arc::ptr_eq(lock(&shared.engine).as_ref().unwrap(), &cpu));
    assert!(cache.exists());
}

#[test]
fn driver_error_after_oom_disables_all() {
    let mut e = env(&[]);
    e.start(true, true);
    e.wait_idle(0);
    let shared = &e.pipeline.as_ref().unwrap().shared;
    let cpu = lock(&shared.engine).clone().unwrap();
    let a = gpu_engine(&e.stub, &cpu);
    let mut other = fake_gpu();
    other.description = "另一块显卡".into();
    other.pci_id = Some("0000:02:00.0".into());
    let b = Arc::new(Engine {
        backend: e.stub.clone(),
        gpu: Some(other),
        device: "另一块显卡".into(),
        images: true,
        audio: true,
        video: true,
        text_space: cpu.text_space.clone(),
        cfg: cpu.cfg.clone(),
        failed: AtomicBool::new(false),
        driver_failed: AtomicBool::new(false),
        released: Released(Arc::new(AtomicBool::new(false))),
    });
    // 显存不足不是驱动问题：只停用这块显卡，换下一块
    *lock(&shared.engine) = Some(a.clone());
    shared.engine_failed(&a, &EmbedError::OutOfMemory("oom".into()), shared.start());
    assert!(!lock(&shared.disabled).all);
    *lock(&shared.engine) = Some(b.clone());
    // 同一引擎上随后的请求报设备丢失：升级为驱动错误，所有显卡停用，槽中已换上的另一块显卡的引擎也退役
    shared.engine_failed(&a, &EmbedError::DeviceLost("lost".into()), shared.start());
    assert!(lock(&shared.disabled).all);
    assert!(
        !lock(&shared.engine)
            .as_ref()
            .is_some_and(|x| Arc::ptr_eq(x, &b))
    );
}
