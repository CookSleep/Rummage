//! 睡眠与唤醒：
//! - 系统即将睡眠（`suspend`）：不再领取新作业，暂停原因报为 `sleeping`（优先级仅次于手动暂停），立即广播。
//!   在窗口线程上调用，只改睡眠状态，不等 `paused` 与数据库锁；广播放到单独的线程，仍在 `paused` 锁内读取并发出。
//!   每次挂起让代数加 1；唤醒验证结束时只有期间没有新的挂起才恢复领取，连续的睡眠、唤醒交错时不会提前恢复。
//! - 唤醒（`resume`）：先用一条黄金文本样本验证当前的推理引擎，余弦不低于 `selftest_min_cosine` 才算通过。
//!   - 引擎已因设备丢失或后端失效被标记为失效时不验证：调度线程会新建引擎。
//!   - 余弦不达标：重启推理进程（`EmbeddingBackend::restart`）再验证一次，仍不达标就丢弃该引擎；是显卡时本次运行停用所有显卡，
//!     下一个作业起只用 CPU（无法排除驱动原因）。
//!   - 验证报错（设备丢失、超时、崩溃、后端不可用）：GPU 引擎不重启，按 `Shared::engine_failed` 处理（验证中总算跨过睡眠，
//!     本次运行停用所有显卡）；
//!     重启会去重新初始化可能已丢失的显卡（`LocalBackend::restart` 也保留设备丢失的状态）。CPU 引擎除设备丢失外
//!     仍重启再验证一次。
//!   - 验证串行执行；从唤醒到验证结束，查询先等验证完成（最多 `infer_timeout_s`），不会用到正在验证的引擎。
//!   - 睡眠前已领取的作业不等验证，醒来后照常推理，仍可能落在已损坏的推理进程上；它们按普通的推理失败处理
//!     （计一次失败，设备丢失或后端失效时换用 CPU）。
//!
//! 跨过睡眠的请求不写显卡的持久屏蔽记录：领取（查询为开始）之后有过挂起，或出错时正在睡眠、唤醒验证中
//! （`Sleep::overlaps`），`Shared::engine_failed` 只在本次运行内停用所有显卡、改用 CPU。唤醒验证总在验证中，因此也不写。
//! 唤醒后外接显卡可能已经拔掉，或驱动还没恢复，不能说明显卡或驱动本身有问题；下次启动核心时仍按自检记录选用它。
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use rummage_embed::selftest::Sample;
use rummage_embed::{Error as EmbedError, Priority, vector};

use super::{Engine, Pipeline, float, int, lock, spawn};

#[derive(Default)]
pub(super) struct Sleep {
    /// 挂起的代数，每次 `suspend` 加 1
    generation: u64,
    /// 睡眠中或验证中，不领取新作业
    held: bool,
    /// 正在进行（含排队等待）的唤醒验证数，大于 0 时查询先等
    verifying: u32,
}

impl Sleep {
    pub(super) fn held(&self) -> bool {
        self.held
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    /// 代数为 `generation` 时开始的请求跨过了睡眠：之后有过挂起，或现在正在睡眠、唤醒验证中
    pub(super) fn overlaps(&self, generation: u64) -> bool {
        self.generation != generation || self.held || self.verifying > 0
    }
}

enum Check {
    Passed,
    /// 余弦不达标
    Low,
    Error(EmbedError),
}

/// 验证一次；报错与不达标都写日志
fn check(engine: &Engine, sample: &Sample, min_cos: f64) -> Check {
    match engine.backend.embed(sample.input.clone(), Priority::Query) {
        Ok(e) => {
            let cos = vector::cosine(&e.vector, &sample.want);
            // NaN 也算不达标
            if cos >= min_cos {
                return Check::Passed;
            }
            log::warn!(
                "唤醒后推理引擎（{}）的样本 {} 余弦为 {cos:.6}，低于 {min_cos}。",
                engine.device,
                sample.id
            );
            Check::Low
        }
        Err(err) => {
            log::warn!("唤醒后推理引擎（{}）验证出错：{err}", engine.device);
            Check::Error(err)
        }
    }
}

impl Pipeline {
    /// 系统即将睡眠：不再领取新作业，处理中的照常完成
    pub fn suspend(&self) {
        {
            let mut s = lock(&self.shared.sleep);
            s.generation += 1;
            s.held = true;
        }
        // 统计进度要读数据库，不占用窗口线程
        let shared = self.shared.clone();
        spawn("sleep-status".into(), move || shared.broadcast());
    }

    /// 系统唤醒：验证推理引擎后恢复领取作业（见模块说明）。`sample` 为空（黄金样本读取失败）或还没有引擎时不验证。
    /// 会阻塞到验证结束，在单独的线程中调用
    pub fn resume(&self, sample: Option<&Sample>) {
        let shared = &self.shared;
        lock(&shared.sleep).verifying += 1;
        let serial = lock(&shared.verify);
        let generation = lock(&shared.sleep).generation;
        let engine = lock(&shared.engine).clone();
        if let (Some(e), Some(s)) = (engine, sample)
            && !e.failed.load(Ordering::Relaxed)
        {
            self.verify(&e, s);
        }
        drop(serial);
        let paused = lock(&shared.paused);
        let held = {
            let mut s = lock(&shared.sleep);
            s.verifying -= 1;
            // 验证期间又挂起过：保持不领取，等下一次唤醒
            if s.generation == generation {
                s.held = false;
            }
            s.held
        };
        shared.sleep_cv.notify_all();
        if !held {
            shared.wake();
        }
        shared.send_status(*paused);
    }

    fn verify(&self, e: &Arc<Engine>, s: &Sample) {
        let shared = &self.shared;
        let min_cos = float(&lock(&shared.params), "selftest_min_cosine");
        let start = shared.start();
        let retry = || {
            log::warn!("唤醒后推理引擎验证未通过，重启推理进程后再验证一次。");
            if let Err(err) = e.backend.restart() {
                log::warn!("无法重启推理进程：{err}");
            }
            matches!(check(e, s, min_cos), Check::Passed)
        };
        let passed = match check(e, s, min_cos) {
            Check::Passed => true,
            Check::Low => retry(),
            // GPU 报错不重启，见模块说明；验证中总是只在本次运行内停用
            Check::Error(err) if e.gpu.is_some() => {
                shared.engine_failed(e, &err, start);
                false
            }
            Check::Error(EmbedError::DeviceLost(_)) => false,
            Check::Error(_) => retry(),
        };
        if passed {
            return;
        }
        log::warn!(
            "唤醒后推理引擎（{}）未通过验证，本次运行改用 CPU。",
            e.device
        );
        // 不写屏蔽记录，原因见模块说明；唤醒后的失败无法排除驱动原因，本次运行停用所有显卡
        shared.disable_gpu(e, true);
        e.failed.store(true, Ordering::Relaxed);
        let mut slot = lock(&shared.engine);
        if slot.as_ref().is_some_and(|cur| Arc::ptr_eq(cur, e)) {
            shared.retire(&mut slot);
        }
    }

    /// 唤醒验证进行中时等它结束，最多等 `infer_timeout_s`
    pub(super) fn wait_verified(&self) {
        let shared = &self.shared;
        let secs = int(&lock(&shared.params), "infer_timeout_s").max(0) as u64;
        let s = lock(&shared.sleep);
        let _ = shared
            .sleep_cv
            .wait_timeout_while(s, Duration::from_secs(secs), |s| s.verifying > 0);
    }
}
