//! 暂停、降温与空闲卸载：暂停时不领取新作业、丢弃推理引擎但查询照常、暂停期间不显示剩余时间、降温时处理中的作业完成前不再领取、
//! 空闲卸载时限的修改立即生效。
use super::*;

fn txt_files(n: usize) -> Vec<(String, Vec<u8>)> {
    (0..n)
        .map(|i| (format!("{i}.txt"), format!("内容{i}").into_bytes()))
        .collect()
}

#[test]
fn paused_claims_nothing_but_queries_work() {
    let files = txt_files(8);
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    let mut e = env(&refs);
    e.threads = Some(1);
    *lock(&e.stub.hold) = true;
    let mut rx = e.events.subscribe();
    e.start(true, true);
    wait_entered(&e);
    // 等预处理、调度线程都压满，领取的作业数不再变化
    thread::sleep(Duration::from_millis(500));
    let p = e.pipeline.as_ref().unwrap();
    let reason = PauseReason::Manual { until: None };
    p.set_paused(Some(reason));
    // 状态变化时立即广播，不等写库线程
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match rx.try_recv() {
            Ok(Event::IndexProgress(s)) if s.paused == Some(reason) => break,
            Ok(_) => {}
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
        assert!(Instant::now() < deadline, "没有收到暂停时的进度事件");
    }
    let claimed = e.count("SELECT COUNT(*) FROM jobs WHERE stage <> 'pending'");
    // 暂停时丢弃推理引擎
    assert!(lock(&p.shared.engine).is_none());

    // 查询照常可用，需要时新建引擎
    let mut q = p.query_engine().unwrap();
    p.embed_query(&mut q, Input::Query("暂停时的查询".into()))
        .unwrap();
    assert_eq!(e.stub.queries.load(Ordering::Relaxed), 1);

    // 正在推理的文件算完，已领取、还没推理的放回 pending，之后不再领取
    release_hold(&e);
    let s = e.wait_until(|s| s.state == IndexState::Idle && s.done + s.pending == 8);
    assert_eq!(s.paused, Some(reason));
    assert!(s.done > 0 && (s.done as i64) < claimed, "{s:?} {claimed}");
    thread::sleep(Duration::from_millis(500));
    assert_eq!(e.status().done, s.done);
    assert_eq!(
        e.count("SELECT COUNT(*) FROM jobs WHERE stage <> 'pending'"),
        s.done as i64
    );

    // 解除后继续
    p.set_paused(None);
    let s = e.wait_idle(0);
    assert_eq!((s.done, s.paused), (8, None));
}

#[test]
fn eta_hidden_while_paused_and_reset_after() {
    let files = txt_files(8);
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
    // 填入暂停前的 20 条记录（跨度 19 秒），足够给出估计
    let now = Instant::now();
    for i in 0..20 {
        lock(&p.shared.recent)
            .0
            .push_back(now - Duration::from_secs(20 - i));
    }
    let s = e.status();
    assert_eq!((s.state, s.paused), (IndexState::Running, None));
    assert!(s.eta_s.is_some(), "{s:?}");

    // 暂停期间不给出估计
    p.set_paused(Some(PauseReason::Manual { until: None }));
    assert_eq!(e.status().eta_s, None);
    release_hold(&e);
    let s = e.wait_until(|s| s.state == IndexState::Idle && s.pending > 0);
    assert_eq!(s.eta_s, None);

    // 恢复后再领取作业时清空窗口：暂停前和暂停期间完成的记录都不留
    let resumed = Instant::now();
    p.set_paused(None);
    e.wait_idle(0);
    let recent = lock(&p.shared.recent);
    assert!(recent.0.len() < 8, "{:?}", recent.0.len());
    assert!(recent.0.iter().all(|t| *t >= resumed));
}

#[test]
fn slow_waits_for_inflight_jobs() {
    let files = txt_files(8);
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    let mut e = env(&refs);
    e.threads = Some(1);
    *lock(&e.stub.hold) = true;
    e.start(true, true);
    wait_entered(&e);
    let p = e.pipeline.as_ref().unwrap();
    p.set_slow(true);
    // 调度线程交出手头的作业后停在降温等待中，处理中的作业没完成就不再领取
    thread::sleep(Duration::from_millis(500));
    let claimed = e.count("SELECT COUNT(*) FROM jobs WHERE stage <> 'pending'");
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        e.count("SELECT COUNT(*) FROM jobs WHERE stage <> 'pending'"),
        claimed
    );
    release_hold(&e);
    // 关闭降温后不再等完休息时间
    p.set_slow(false);
    let s = e.wait_idle(0);
    assert_eq!(s.done, 8);
}

#[test]
fn engine_idle_applied() {
    #[derive(Default)]
    struct Idle(Mutex<Vec<Duration>>);
    impl EmbeddingBackend for Idle {
        fn embed(&self, _: Input, _: Priority) -> Result<Embedding, EmbedError> {
            unreachable!("没有作业")
        }
        fn count_tokens(&self, _: &Input, _: Priority) -> Result<usize, EmbedError> {
            unreachable!("没有作业")
        }
        fn set_idle_timeout(&self, idle: Duration) {
            lock(&self.0).push(idle);
        }
    }
    let mut e = env(&[]);
    e.start(true, true);
    e.wait_idle(0);
    let p = e.pipeline.as_ref().unwrap();
    let idle = Arc::new(Idle::default());
    let cpu = lock(&p.shared.engine).clone().unwrap();
    *lock(&p.shared.engine) = Some(Arc::new(Engine {
        backend: idle.clone(),
        gpu: None,
        device: "CPU".into(),
        images: true,
        audio: true,
        video: true,
        text_space: cpu.text_space.clone(),
        cfg: cpu.cfg.clone(),
        failed: AtomicBool::new(false),
        driver_failed: AtomicBool::new(false),
        retired: AtomicBool::new(false),
        released: Released(Arc::new(AtomicBool::new(false))),
    }));
    lock(&p.shared.params).insert("engine_idle_min".into(), ParamValue::Int(3));
    p.apply_engine_idle();
    assert_eq!(*lock(&idle.0), [Duration::from_secs(180)]);
}
