//! 暂停领取作业与重建引擎：引导未完成、设备自检、模型下载或导入、清空索引期间 `hold`；自检开始前、模型任务开始前
//! `wait_released` 等旧引擎的推理进程退出（之后才能替换被它打开的模型文件）；选择或参数改变后 `reload`
//! （见 `pipeline` 模块说明）。丢弃引擎的地方（`hold`、`reload`、`drop_engine`）都经 `Shared::retire` 记下释放标志。
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::{Engine, Pipeline, Shared, lock};

/// 释放时置位，`Pipeline::wait_released` 据此确认旧引擎的推理进程已退出
pub(super) struct Released(pub(super) Arc<AtomicBool>);

impl Drop for Released {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// 暂停领取作业、不建引擎的原因
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// 引导未完成
    Setup,
    /// 设备自检进行中
    Selftest,
    /// 模型下载或导入进行中
    Models,
    /// 清空索引进行中：等处理中的作业结束再删除，避免旧结果写进复用了同一 ID 的新文件
    Clear,
}

impl Shared {
    /// 丢弃槽中的引擎，记下它的释放标志（`wait_released` 等它）；返回该标志。调用方持有引擎槽的锁
    pub(super) fn retire(&self, slot: &mut Option<Arc<Engine>>) -> Option<Arc<AtomicBool>> {
        let old = slot.take()?;
        old.retired.store(true, Ordering::Relaxed);
        let flag = old.released.0.clone();
        *lock(&self.last_released) = Some(flag.clone());
        Some(flag)
    }
}

impl Pipeline {
    /// 开始（`on`）或结束一种暂停（引导未完成、设备自检、模型任务）：开始时丢弃当前引擎（处理中的作业仍用它处理完），之后
    /// 不领取作业、不建新引擎，推理进程不与自检争用显卡；查询这段时间没有引擎，只有文件名结果。
    /// 返回被丢弃的引擎的释放标志，交给 `wait_released`
    pub fn hold(&self, kind: Hold, on: bool) -> Option<Arc<AtomicBool>> {
        let released = {
            let mut slot = lock(&self.shared.engine);
            let flag = match kind {
                Hold::Setup => &self.shared.held_setup,
                Hold::Selftest => &self.shared.held_selftest,
                Hold::Models => &self.shared.held_models,
                Hold::Clear => &self.shared.held_clear,
            };
            flag.store(on, Ordering::Relaxed);
            if on {
                self.shared.retire(&mut slot)
            } else {
                None
            }
        };
        self.shared.wake();
        // 模型任务结束后图片与音视频模型可能刚装好：已建好的文档补算内嵌媒体。删除模型时内嵌片段保留，不放回
        if kind == Hold::Models
            && !on
            && let Err(err) = self.requeue(&[])
        {
            log::warn!("无法检查哪些文件要按新装的模型补算：{err}");
        }
        released
    }

    /// 等处理中的作业都结束、`released` 与最近一次丢弃的引擎都已释放（推理进程退出），最多等 `limit`；返回是否等到了
    pub fn wait_released(&self, released: Option<Arc<AtomicBool>>, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        let done =
            |r: &Option<Arc<AtomicBool>>| r.as_ref().is_none_or(|r| r.load(Ordering::Relaxed));
        loop {
            let idle = self.shared.inflight.load(Ordering::Relaxed) == 0
                && done(&released)
                && done(&lock(&self.shared.last_released));
            if idle {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// 后台自动检测设备期间（`on`）暂时只用 CPU、照常建索引：正在用 GPU 的引擎丢弃，返回它的释放标志
    /// （用 CPU 的引擎照用，返回最近一次丢弃的引擎的标志），交给 `wait_engine_released` 等它的推理进程退出，
    /// 不与检测争用显卡。结束时（`!on`）按新的检测记录重建引擎
    pub fn cpu_only(&self, on: bool) -> Option<Arc<AtomicBool>> {
        let released = {
            let mut slot = lock(&self.shared.engine);
            self.shared.cpu_only.store(on, Ordering::Relaxed);
            if !on || slot.as_ref().is_some_and(|e| e.gpu.is_some()) {
                self.shared.retire(&mut slot)
            } else {
                // 之前丢弃的引擎（例如刚 `reload` 掉的 GPU 引擎）可能还在处理作业，同样要等它
                lock(&self.shared.last_released).clone()
            }
        };
        self.shared.wake();
        released
    }

    /// 只等 `released` 对应的引擎释放（用它的作业都已结束、推理进程退出），最多等 `limit`；返回是否等到了。
    /// 不等其他作业：自动检测期间 CPU 照常建索引
    pub fn wait_engine_released(&self, released: Option<Arc<AtomicBool>>, limit: Duration) -> bool {
        let Some(flag) = released else {
            return true;
        };
        let deadline = Instant::now() + limit;
        while !flag.load(Ordering::Relaxed) {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(50));
        }
        true
    }

    /// 用户选择设备（`auto`、`cpu` 或显卡的标识）后调用，随后 `reload`
    pub fn set_preferred_device(&self, device: &str) {
        *lock(&self.shared.preferred) = device.to_string();
    }

    /// 按当前的选择、自检记录与参数重建引擎（自检完成、用户选择的设备改变后，或 `index_images`、`index_audio`、`index_video`、推理线程数、推理进程的内存上限与启动时限修改后）。
    /// 不清除本次运行停用的显卡：本次运行中出过错的 GPU 不再使用
    pub fn reload(&self) {
        self.shared.retire(&mut lock(&self.shared.engine));
        // 换了设备可能换了模型文件（LiteRT 的 NPU），要重建的文件按新的选择判断
        if let Err(err) = self.requeue(&[]) {
            log::error!("无法检查哪些文件要按新的设备重建：{err}");
        }
        self.shared.wake();
    }

    /// 处于某种暂停中（引导、设备检测、模型任务、清空索引），没有引擎
    pub fn held(&self) -> bool {
        self.shared.held()
    }

    /// 本次运行中显卡出过错（`Shared::disabled`）：出错的显卡已停用，本次运行不再自动检测设备
    pub fn gpu_disabled(&self) -> bool {
        lock(&self.shared.disabled).failed.is_some()
    }

    /// 当前引擎实际使用的设备：`cpu` 或显卡的标识（`selftest::CandidateReport::device_id`）；还没有引擎时为空
    pub fn engine_device(&self) -> Option<String> {
        lock(&self.shared.engine).as_ref().map(|e| {
            e.gpu
                .as_ref()
                .map_or_else(|| rummage_proto::CPU_DEVICE.to_string(), |g| g.device_id())
        })
    }
}
