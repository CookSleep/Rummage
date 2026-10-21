//! 自检中单个候选的测试：按顺序测试并在驱动错误后跳过其余 GPU（`test_in_order`），
//! 在单独的线程中启动推理进程、算黄金样本（`measure`）再测速（`speed`），限时等结果（`wait_limited`），
//! 测完查系统日志中的显卡驱动事件（`check_driver_events`）。判定规则见上级模块说明。
//! 黄金样本在 GPU 上算两遍：第一遍比较余弦兼作预热（首次遇到的输入形状要编译着色器），第二遍计时；
//! CPU 没有这类预热成本，只算一遍，比较余弦的同时计时。
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use super::golden::Golden;
use super::speed;
use super::{
    CANCEL_POLL, CandidateReport, Events, Modality, SKIPPED_REASON, SampleResult, Status,
    TDR_LOG_WAIT, TDR_WINDOW_MARGIN, WAIT_GRACE,
};
use crate::local::{Limits, LocalBackend, WorkerSpec};
use crate::space::Backend;
use crate::{EmbeddingBackend, Priority, platform, vector};

/// 按顺序测试每个候选；某块 GPU 出现驱动错误后，其余 GPU 记为“已跳过”，不再调用 `test_one`。
/// 沿用上次结果的（`reused`）不初始化显卡，照常调用
pub(super) fn test_in_order(
    reports: &mut [CandidateReport],
    reused: &[bool],
    mut test_one: impl FnMut(usize, &mut CandidateReport),
) {
    let mut stopped = false;
    for (i, r) in reports.iter_mut().enumerate() {
        if stopped && r.backend.accel() && !reused.get(i).is_some_and(|x| *x) {
            r.status = Status::Skipped;
            r.reason = Some(SKIPPED_REASON.to_string());
            continue;
        }
        test_one(i, r);
        stopped |= r.backend.accel() && r.driver_error;
    }
}

/// 等系统日志写入后查询本候选开始以来的显卡驱动事件，有就判失败；读不到同样判失败（不能确认安全）。
pub(super) fn check_driver_events(r: &mut CandidateReport, start: Instant) -> Events {
    thread::sleep(TDR_LOG_WAIT);
    match platform::driver_events(start.elapsed() + TDR_WINDOW_MARGIN) {
        Ok(events) if events.is_empty() => Events::None,
        Ok(events) => {
            r.driver_error = true;
            log::warn!(
                "{} 自检期间系统日志中出现 {} 条显卡驱动事件，可能发生过驱动重置：{}。",
                r.name,
                events.len(),
                events.join("；")
            );
            r.fail("检测期间显卡驱动出现错误，为避免影响系统，已屏蔽这块显卡，更新显卡驱动后可以重新检测。".to_string());
            Events::Found
        }
        Err(err) => {
            log::warn!("无法读取系统日志，不能确认自检期间显卡驱动是否重置：{err}");
            r.fail(
                "无法读取系统日志，不能确认检测期间显卡驱动是否出错，本次不使用这块显卡。"
                    .to_string(),
            );
            Events::Unreadable
        }
    }
}

/// 在单独的线程中测试一个候选；`total` 为总时限，到期就结束推理进程并判失败。
/// `cancel` 置位时结束推理进程，`r` 不再使用
pub(super) fn test(
    r: &mut CandidateReport,
    spec: WorkerSpec,
    limits: &Limits,
    golden: &Arc<Golden>,
    min_cos: f64,
    total: Option<Duration>,
    cancel: &AtomicBool,
) {
    let backend = match LocalBackend::new(spec, limits.clone()) {
        Ok(b) => Arc::new(b),
        Err(err) => return r.backend_error(String::new(), &err),
    };
    let (tx, rx) = mpsc::channel();
    let (b, g, mut out) = (backend.clone(), golden.clone(), r.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    thread::spawn(move || {
        measure(&b, &g, min_cos, &stopped, &mut out);
        let _ = tx.send(out);
    });
    wait_limited(r, &rx, total, WAIT_GRACE, cancel, || {
        // 推理进程可能还在启动、结束不到，测试线程在下一个样本前看到 `stop` 也会退出
        stop.store(true, Ordering::Relaxed);
        backend.kill_worker();
    });
}

/// 等测试线程的结果。`limit` 到期就调用 `kill` 结束推理进程，再最多等 `grace`；
/// 到期按驱动错误处理（GPU 超时很可能就是 TDR），进程结束不了时同样如此，另记 `stuck`（屏蔽）。
/// `cancel` 置位时同样调用 `kill` 并最多等 `grace`：按时结束时 `r` 只带回设备丢失与结束不了两项（屏蔽的依据），
/// 结束不了时记 `stuck`
pub(super) fn wait_limited(
    r: &mut CandidateReport,
    rx: &Receiver<CandidateReport>,
    limit: Option<Duration>,
    grace: Duration,
    cancel: &AtomicBool,
    kill: impl FnOnce(),
) {
    let start = Instant::now();
    let res = loop {
        if cancel.load(Ordering::Relaxed) {
            kill();
            match rx.recv_timeout(grace) {
                Ok(out) => {
                    r.device_lost = out.device_lost;
                    r.stuck = out.stuck;
                }
                Err(RecvTimeoutError::Timeout) => {
                    r.driver_error = true;
                    r.stuck = true;
                    r.fail(format!(
                        "检测被中止后 {} 秒内仍未结束，显卡驱动可能已停止响应，建议重新启动电脑。",
                        grace.as_secs()
                    ));
                }
                Err(RecvTimeoutError::Disconnected) => {}
            }
            return;
        }
        let left = limit.map(|l| l.saturating_sub(start.elapsed()));
        match rx.recv_timeout(left.map_or(CANCEL_POLL, |l| l.min(CANCEL_POLL))) {
            Err(RecvTimeoutError::Timeout) if left != Some(Duration::ZERO) => {}
            res => break res,
        }
    };
    match res {
        Ok(out) => *r = out,
        Err(RecvTimeoutError::Timeout) => {
            let limit = limit.unwrap_or_default();
            kill();
            // 保留已经算出的余弦；测试线程自己的失败原因只是进程被结束的结果，不再列出。
            let returned = match rx.recv_timeout(grace) {
                Ok(out) => {
                    *r = CandidateReport {
                        reason: None,
                        ..out
                    };
                    true
                }
                Err(_) => false,
            };
            r.driver_error = true;
            r.fail(format!(
                "检测超过 {} 秒仍未完成，已停止检测这块显卡。",
                limit.as_secs()
            ));
            if !returned {
                r.stuck = true;
                r.fail(format!(
                    "停止后 {} 秒内仍未结束，显卡驱动可能已停止响应，建议重新启动电脑。",
                    grace.as_secs()
                ));
            }
        }
        Err(RecvTimeoutError::Disconnected) => r.fail("检测意外中断。".to_string()),
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// `stop` 置位（检测被中止）时在下一个样本前返回
fn measure(
    backend: &LocalBackend,
    golden: &Golden,
    min_cos: f64,
    stop: &AtomicBool,
    r: &mut CandidateReport,
) {
    let t = Instant::now();
    if let Err(err) = backend.start() {
        return r.backend_error(String::new(), &err);
    }
    r.init_ms = Some(ms(t.elapsed()));
    if r.backend == Backend::Cpu
        && let Some(d) = backend.device()
    {
        r.name = d.name;
        r.description = d.description;
    }

    let gpu = r.backend.accel();
    let mut low = Vec::new();
    let mut times = Vec::new();
    for s in &golden.samples {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let t = Instant::now();
        let v = match backend.embed(s.input.clone(), Priority::Index) {
            Ok(e) => e.vector,
            Err(err) => return r.backend_error(format!("样本 {}：", s.id), &err),
        };
        times.push((s.id.clone(), ms(t.elapsed())));
        if v.len() != s.want.len() {
            log::warn!(
                "{} 样本 {} 的向量为 {} 维，参考向量为 {} 维。",
                r.name,
                s.id,
                v.len(),
                s.want.len()
            );
            return r.fail("计算结果格式不正确，不能使用这个设备。".to_string());
        }
        let cos = vector::cosine(&v, &s.want);
        // NaN 也算不达标。
        if cos.is_nan() || cos < min_cos {
            low.push(format!("{}（{cos:.6}）", s.id));
        }
        r.samples.push(SampleResult {
            id: s.id.clone(),
            modality: s.modality,
            cosine: cos,
        });
    }
    if !low.is_empty() {
        log::warn!(
            "{} 以下样本与参考向量的余弦低于 {min_cos}：{}。",
            r.name,
            low.join("、")
        );
        return r.fail("这个设备的计算结果与标准结果相差过大，不能使用。".to_string());
    }

    // GPU 第二遍计时，第一遍兼作预热；CPU 用第一遍的耗时
    if gpu {
        times.clear();
        for s in &golden.samples {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let t = Instant::now();
            if let Err(err) = backend.embed(s.input.clone(), Priority::Index) {
                return r.backend_error(format!("样本 {}：", s.id), &err);
            }
            times.push((s.id.clone(), ms(t.elapsed())));
        }
    }
    let by = |m: Modality| {
        let v = golden
            .samples
            .iter()
            .zip(&times)
            .filter(|(s, _)| s.modality == m)
            .map(|(_, t)| t.1)
            .collect();
        median(v)
    };
    r.text_ms = by(Modality::Text);
    r.image_ms = by(Modality::Image);
    r.audio_ms = by(Modality::Audio);
    r.video_ms = by(Modality::Video);
    if let Some(plan) = &golden.speed {
        match speed::measure(backend, plan, &times, gpu, stop) {
            Ok(Some(s)) => r.speed = Some(s),
            Ok(None) => return,
            Err((context, err)) => return r.backend_error(context, &err),
        }
    }
    r.status = Status::Passed;
}

pub(super) fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let mid = v.len() / 2;
    Some(if v.len().is_multiple_of(2) {
        (v[mid - 1] + v[mid]) / 2.0
    } else {
        v[mid]
    })
}
