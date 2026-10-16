//! 单个候选的测试：驱动错误后跳过其余显卡、超时与崩溃按驱动错误处理、按证据分级、显存不足与结束不了、中止、中位数
use std::sync::mpsc;

use super::super::candidate::{median, wait_limited};
use super::*;

#[test]
fn stops_gpus_after_driver_error() {
    let gpu_report = |desc: &str| {
        let c = Candidate::Gpu(gpu(desc));
        CandidateReport::new(&c, &key_with(&c, None))
    };
    let mut reports = vec![
        report(Status::Failed, None),
        gpu_report("A"),
        gpu_report("B"),
        gpu_report("C"),
    ];
    let mut tested = Vec::new();
    test_in_order(&mut reports, &[], |i, r| {
        tested.push(i);
        if i == 1 {
            r.backend_error(String::new(), &Error::DeviceLost("Vulkan0".to_string()));
        } else {
            r.status = Status::Passed;
        }
    });
    assert_eq!(tested, [0, 1]);
    assert_eq!(reports[0].status, Status::Passed);
    assert_eq!(reports[1].status, Status::Failed);
    assert!(reports[1].driver_error);
    for r in &reports[2..] {
        assert_eq!(r.status, Status::Skipped);
        assert_eq!(r.reason.as_deref(), Some(SKIPPED_REASON));
        assert!(!r.driver_error);
    }
    // 沿用上次结果的显卡不初始化，照常沿用
    let mut reports = vec![gpu_report("A"), gpu_report("B"), gpu_report("C")];
    let mut tested = Vec::new();
    test_in_order(&mut reports, &[false, false, true], |i, r| {
        tested.push(i);
        if i == 0 {
            r.backend_error(String::new(), &Error::DeviceLost("Vulkan0".to_string()));
        }
    });
    assert_eq!(tested, [0, 2]);
    assert_eq!(reports[1].status, Status::Skipped);

    // 普通失败（如余弦不达标）不影响后面的显卡。
    let mut reports = vec![gpu_report("A"), gpu_report("B")];
    let mut tested = Vec::new();
    test_in_order(&mut reports, &[], |i, r| {
        tested.push(i);
        r.fail("余弦低于门槛。".to_string());
    });
    assert_eq!(tested, [0, 1]);
}

fn gpu_report() -> CandidateReport {
    let c = Candidate::Gpu(gpu("GPU"));
    CandidateReport::new(&c, &key_with(&c, None))
}

#[test]
fn timeout_is_driver_error() {
    let ms = Duration::from_millis;
    let no = AtomicBool::new(false);
    // 结束推理进程后测试线程返回：保留已算出的余弦。
    let (tx, rx) = mpsc::channel();
    let mut r = gpu_report();
    let mut partial = gpu_report();
    partial.samples.push(SampleResult {
        id: "q01".to_string(),
        modality: Modality::Text,
        cosine: 1.0,
    });
    partial.reason = Some("推理进程异常退出。".to_string());
    let mut killed = false;
    wait_limited(&mut r, &rx, Some(ms(10)), ms(1000), &no, || {
        killed = true;
        tx.send(partial).unwrap();
    });
    assert!(killed);
    assert!(r.driver_error && !r.stuck);
    assert_eq!(r.status, Status::Failed);
    assert_eq!(r.samples.len(), 1);
    assert!(r.reason.as_deref().unwrap().starts_with("检测超过"));

    // 结束后仍不返回：推理进程结束不了，屏蔽
    let (_tx, rx) = mpsc::channel();
    let mut r = gpu_report();
    wait_limited(&mut r, &rx, Some(ms(10)), ms(10), &no, || {});
    assert!(r.driver_error && r.stuck);
    assert!(r.reason.as_deref().unwrap().contains("仍未结束"));
    assert_eq!(verdict(&r, Events::None), Verdict::Blacklist);

    // 按时返回不受影响。
    let (tx, rx) = mpsc::channel();
    let mut done = gpu_report();
    done.status = Status::Passed;
    tx.send(done).unwrap();
    let mut r = gpu_report();
    wait_limited(&mut r, &rx, Some(ms(1000)), ms(10), &no, || {
        panic!("不应结束推理进程")
    });
    assert_eq!(r.status, Status::Passed);
    assert!(!r.driver_error);

    // 中止：结束推理进程，结果不变、不算驱动错误；没有时限（CPU）时同样
    let (tx, rx) = mpsc::channel();
    let mut r = gpu_report();
    let mut killed = false;
    wait_limited(&mut r, &rx, None, ms(1000), &AtomicBool::new(true), || {
        killed = true;
        tx.send(gpu_report()).unwrap();
    });
    assert!(killed);
    assert_eq!(r.status, gpu_report().status);
    assert!(!r.driver_error && !r.stuck && r.reason.is_none());

    // 中止后推理进程结束不了：与超时一样按驱动错误处理，并记下结束不了（屏蔽）
    let (_tx, rx) = mpsc::channel();
    let mut r = gpu_report();
    wait_limited(&mut r, &rx, None, ms(10), &AtomicBool::new(true), || {});
    assert!(r.driver_error && r.stuck);
    assert_eq!(r.status, Status::Failed);
    assert!(r.reason.as_deref().unwrap().contains("仍未结束"));
}

#[test]
fn gpu_crash_is_driver_error() {
    let cases = [
        (Error::Unavailable("显卡后端失效".to_string()), true),
        (Error::Crashed("退出".to_string()), true),
        (Error::InferTimeout(Duration::from_secs(60)), true),
        (Error::DeviceLost("Vulkan0".to_string()), true),
        (Error::InvalidInput("过长".to_string()), false),
    ];
    for (err, want) in cases {
        let mut r = gpu_report();
        r.backend_error(String::new(), &err);
        assert_eq!(r.driver_error, want, "{err}");
    }
    // CPU 的崩溃与失效不算驱动错误，设备丢失仍算。
    let mut r = report(Status::Failed, None);
    r.backend_error(String::new(), &Error::Unavailable("连续失败".to_string()));
    assert!(!r.driver_error);
}

#[test]
fn verdict_by_evidence() {
    let failed = |f: fn(&mut CandidateReport)| {
        let mut r = gpu_report();
        f(&mut r);
        r
    };
    // 超时、崩溃、后端失效：没有驱动事件时只判本次失败，不写记录
    let timeout =
        failed(|r| r.backend_error(String::new(), &Error::InferTimeout(Duration::from_secs(60))));
    assert!(timeout.driver_error);
    assert_eq!(verdict(&timeout, Events::None), Verdict::Transient);
    assert_eq!(verdict(&timeout, Events::Unreadable), Verdict::Transient);
    // 有驱动事件，或设备丢失：持久屏蔽
    assert_eq!(verdict(&timeout, Events::Found), Verdict::Blacklist);
    let lost = failed(|r| r.backend_error(String::new(), &Error::DeviceLost("x".into())));
    assert_eq!(verdict(&lost, Events::None), Verdict::Blacklist);
    // 余弦不达标、NaN：未通过
    let low = failed(|r| r.fail("以下样本与参考向量的余弦低于 0.99：q01。".into()));
    assert_eq!(verdict(&low, Events::None), Verdict::Failed);
    let nan = failed(|r| r.backend_error(String::new(), &Error::BadVector("NaN".into())));
    assert!(!nan.driver_error);
    assert_eq!(verdict(&nan, Events::None), Verdict::Failed);
    // 无法读取系统日志而判的失败不写记录
    assert_eq!(verdict(&low, Events::Unreadable), Verdict::Transient);
    // 列出之后显卡不见了：不算驱动错误，也不写记录
    let gone = failed(|r| r.backend_error(String::new(), &Error::GpuNotFound("A".into())));
    assert!(!gone.driver_error);
    assert_eq!(verdict(&gone, Events::None), Verdict::Transient);
    // 显存不足（显存一时被别的程序占满）：只在本次运行停用，不记为未通过；其他推理错误照旧记为未通过
    let oom = failed(|r| {
        r.backend_error(
            String::new(),
            &Error::Inference(crate::OUT_OF_MEMORY.to_string()),
        )
    });
    assert!(oom.transient && !oom.driver_error);
    assert_eq!(verdict(&oom, Events::None), Verdict::Transient);
    let other = failed(|r| r.backend_error(String::new(), &Error::Inference("x".into())));
    assert_eq!(verdict(&other, Events::None), Verdict::Failed);
    // 推理进程结束不了：屏蔽
    let stuck = failed(|r| r.stuck = true);
    assert_eq!(verdict(&stuck, Events::None), Verdict::Blacklist);
    let mut ok = gpu_report();
    ok.status = Status::Passed;
    assert_eq!(verdict(&ok, Events::None), Verdict::Passed);
}

#[test]
fn medians() {
    assert_eq!(median(vec![]), None);
    assert_eq!(median(vec![3.0, 1.0, 2.0]), Some(2.0));
    assert_eq!(median(vec![4.0, 1.0, 3.0, 2.0]), Some(2.5));
}

#[test]
fn oom_and_stuck_in_selftest() {
    // 显存不足：显卡只判本次失败，CPU 不算暂时性失败；原因按设备说，不出现“此文件”
    let oom = Error::Inference(crate::OUT_OF_MEMORY.to_string());
    let mut g = gpu_report();
    g.backend_error(String::new(), &oom);
    assert!(g.transient);
    let mut c = report(Status::Failed, None);
    c.backend_error(String::new(), &oom);
    assert!(!c.transient);
    for r in [&g, &c] {
        assert!(
            !r.reason.as_deref().unwrap().contains("此文件"),
            "{:?}",
            r.reason
        );
    }
    let mut g = gpu_report();
    g.backend_error(String::new(), &Error::OutOfMemory("x".into()));
    assert!(g.transient);
    assert_eq!(verdict(&g, Events::None), Verdict::Transient);
    // 推理进程结束不了：驱动错误，屏蔽
    let mut g = gpu_report();
    g.backend_error(String::new(), &Error::Stuck("x".into()));
    assert!(g.stuck && g.driver_error);
    assert_eq!(verdict(&g, Events::None), Verdict::Blacklist);
}

#[test]
fn cancel_keeps_device_lost() {
    // 中止时测试线程按时返回、带回设备丢失或结束不了：保留这两项，作为屏蔽的依据
    let (tx, rx) = mpsc::channel();
    let mut r = gpu_report();
    wait_limited(
        &mut r,
        &rx,
        None,
        Duration::from_millis(1000),
        &AtomicBool::new(true),
        || {
            let mut out = gpu_report();
            out.backend_error(String::new(), &Error::DeviceLost("x".into()));
            tx.send(out).unwrap();
        },
    );
    assert!(r.device_lost && !r.stuck);
    assert!(WAIT_GRACE > KILL_GRACE);
}
