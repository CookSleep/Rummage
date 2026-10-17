//! 暂停建索引与睡眠唤醒在核心状态上的部分：
//! - 判定：手动暂停（到期时清除，含 `meta` 表）、后台额度（`State::quota`）、检测到的系统状态与参数交给 `resource::reason`，
//!   结果交给流水线（`Pipeline::set_paused`，有变化时立即广播）；同一次检测的结果也决定是否降温（`resource::slow`，
//!   `Pipeline::set_slow`）。判定与设置的全过程持有 `State::manual`，并发的判定不会用旧结果覆盖新结果。
//! - 检测线程（`start_monitor`）每 5 秒判定一次，随后检查是否要自动重新检测设备（`State::auto_selftest`）；手动暂停、取消，以及修改相关参数后立即判定一次。
//!   线程只持有 `State` 的 `Weak`，`State` 释放后退出。
//! - 进入任何暂停（判定出暂停原因，或系统即将睡眠）时中止进行中的自动设备检测（`State::stop_auto_selftest`）。
//! - 睡眠与唤醒：`suspend_indexing` 让流水线不再领取作业；`resume_indexing` 读取黄金文本样本，验证推理引擎后恢复
//!   （`pipeline::wake`），再判定一次暂停（睡眠期间可能拔了电源）。
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use rummage_embed::selftest;
use rummage_proto::{ApiError, PauseReason, Reply};

use super::{State, api_error, db, lock};
use crate::resource::{self, MANUAL_KEY};

/// 自动暂停条件的检测间隔
const MONITOR_EVERY: Duration = Duration::from_secs(5);

impl State {
    /// 当前的暂停原因与是否降温；`manual` 是持有中的 `State::manual`，手动暂停到期时清除它
    pub(super) fn pause_reason(
        &self,
        manual: &mut Option<Option<i64>>,
    ) -> (Option<PauseReason>, bool) {
        let Ok(core) = &self.db else {
            return (None, false);
        };
        if let Some(Some(until)) = *manual
            && until <= (self.sys.now)()
        {
            // 删除失败时内存中照样恢复；重启后读到旧记录也已到期，再删一次
            if let Err(err) = lock(&core.db).meta_delete(MANUAL_KEY) {
                log::warn!("无法清除到期的手动暂停记录：{err}");
            }
            *manual = None;
        }
        let sensed = (self.sys.sense)(&core.dir);
        let values = self.params();
        let quota = self.quota.load(Ordering::Relaxed);
        (
            resource::reason(*manual, quota, &sensed, &values),
            resource::slow(&sensed, &values),
        )
    }

    /// 重新判定暂停原因并交给流水线；暂停时中止进行中的自动设备检测
    pub fn refresh_pause(&self) {
        let paused = {
            let mut manual = lock(&self.manual);
            let (reason, slow) = self.pause_reason(&mut manual);
            let paused = reason.is_some();
            if let Some(p) = self.pipeline.get() {
                p.set_paused(reason);
                p.set_slow(slow);
            }
            paused
        };
        if paused {
            self.stop_auto_selftest();
        }
    }

    /// 启动检测线程；流水线没有启动时不启动
    pub fn start_monitor(self: &Arc<Self>) {
        if self.pipeline.get().is_none() {
            return;
        }
        let weak = Arc::downgrade(self);
        let spawned = std::thread::Builder::new()
            .name("pause-monitor".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(MONITOR_EVERY);
                    let Some(state) = weak.upgrade() else {
                        return;
                    };
                    state.refresh_pause();
                    // 设备检测结果过期时按暂停条件决定是否在后台重新检测（`setup`）
                    state.auto_selftest();
                }
            });
        if let Err(err) = spawned {
            log::error!("无法启动暂停检测线程，全屏、电池与磁盘空间不会自动暂停建索引：{err}");
        }
    }

    /// `PauseIndexing`（`Some(until)`）与 `ResumeIndexing`（`None`）：先写库再改内存，立即生效
    pub(super) fn set_manual_pause(&self, pause: Option<Option<i64>>) -> Result<Reply, ApiError> {
        {
            let mut manual = lock(&self.manual);
            let db = db(self)?;
            let res = match pause {
                Some(until) => db.meta_set(MANUAL_KEY, &resource::encode_manual(until)),
                None => db.meta_delete(MANUAL_KEY),
            };
            res.map_err(|err| {
                log::error!("无法保存手动暂停：{err}");
                api_error(
                    ApiError::UNAVAILABLE,
                    "暂停设置未能保存：索引数据库写入失败，请检查磁盘空间后重试。",
                )
            })?;
            *manual = pause;
        }
        self.refresh_pause();
        Ok(Reply::Done)
    }

    /// 系统即将睡眠：不再领取新作业，中止进行中的自动设备检测
    pub fn suspend_indexing(&self) {
        if let Some(p) = self.pipeline.get() {
            p.suspend();
        }
        self.stop_auto_selftest();
    }

    /// 系统已唤醒：用 `golden_dir` 中的黄金文本样本验证推理引擎后恢复领取作业。阻塞到验证结束，在单独的线程中调用
    pub fn resume_indexing(&self, golden_dir: &std::path::Path) {
        let Some(p) = self.pipeline.get() else {
            return;
        };
        let sample = selftest::wake_sample(golden_dir)
            .map_err(|err| log::warn!("无法读取黄金样本，唤醒后不验证推理引擎：{err}"))
            .ok();
        p.resume(sample.as_ref());
        self.refresh_pause();
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI64, Ordering};

    use rummage_embed::{EmbeddingBackend, Error as EmbedError};
    use rummage_index::Db;
    use rummage_proto::{IndexStatus, ParamValue, Request};

    use super::*;
    use crate::pipeline::{EngineSpec, Options};
    use crate::resource::{Sensed, System};
    use crate::server::respond;

    /// 时钟与检测结果都可在测试中修改
    fn fake() -> (System, Arc<AtomicI64>, Arc<Mutex<Sensed>>) {
        let clock = Arc::new(AtomicI64::new(1_000));
        let sensed = Arc::new(Mutex::new(Sensed {
            free_gb: Some(100.0),
            ..Sensed::default()
        }));
        let (c, s) = (clock.clone(), sensed.clone());
        let sys = System {
            sense: Box::new(move |_| *s.lock().unwrap()),
            now: Box::new(move || c.load(Ordering::SeqCst)),
        };
        (sys, clock, sensed)
    }

    /// 没有模型文件：流水线照常运行、报告状态，不创建推理后端
    fn opts(dir: &Path) -> Options {
        Options {
            model: dir.join("无.gguf"),
            mmproj: dir.join("无.gguf"),
            selftest_cache: dir.join("cache").join("selftest.json"),
            ffmpeg: dir.join("ffmpeg"),
            embed_tmp: dir.join("tmp"),
            available: |path, _| path.is_file(),
            factory: Box::new(
                |_: &EngineSpec| -> Result<Arc<dyn EmbeddingBackend>, EmbedError> {
                    unreachable!("没有模型时不创建推理后端")
                },
            ),
            driver_events: Box::new(|_| unreachable!("没有推理后端，不会失效")),
            file_timeout: None,
            gpus: Vec::new,
            litert: false,
        }
    }

    fn start(dir: &Path, sys: System) -> Arc<State> {
        let state = State::with_system("t".into(), Db::open(dir), sys);
        state.start_pipeline(opts(dir));
        state
    }

    fn status(state: &Arc<State>) -> IndexStatus {
        let Ok(Reply::Index(s)) = respond(Request::IndexStatus, state) else {
            panic!("应返回索引进度");
        };
        s
    }

    fn stored(state: &State) -> Option<String> {
        let Ok(core) = &state.db else {
            panic!("数据库应可用");
        };
        lock(&core.db).meta_get(MANUAL_KEY).unwrap()
    }

    #[test]
    fn manual_pause_persists_and_expires() {
        let dir = crate::test_dir();
        let (sys, _, _) = fake();
        let state = start(dir.path(), sys);
        assert_eq!(status(&state).paused, None);
        let body = Request::PauseIndexing { until: Some(5_000) };
        assert_eq!(respond(body, &state), Ok(Reply::Done));
        let manual = Some(PauseReason::Manual { until: Some(5_000) });
        assert_eq!(status(&state).paused, manual);
        state.close();

        // 重启核心后仍然有效，到期后自动恢复并清除记录
        let (sys, clock, _) = fake();
        clock.store(2_000, Ordering::SeqCst);
        let state = start(dir.path(), sys);
        assert_eq!(status(&state).paused, manual);
        clock.store(5_000, Ordering::SeqCst);
        state.refresh_pause();
        assert_eq!(status(&state).paused, None);
        assert_eq!(stored(&state), None);

        // 直到手动恢复：不会到期
        let body = Request::PauseIndexing { until: None };
        assert_eq!(respond(body, &state), Ok(Reply::Done));
        clock.store(i64::MAX, Ordering::SeqCst);
        state.refresh_pause();
        assert_eq!(
            status(&state).paused,
            Some(PauseReason::Manual { until: None })
        );
        assert!(stored(&state).is_some());
        assert_eq!(respond(Request::ResumeIndexing, &state), Ok(Reply::Done));
        assert_eq!(status(&state).paused, None);
        assert_eq!(stored(&state), None);
        state.close();
    }

    #[test]
    fn corrupt_manual_pause_removed() {
        let dir = crate::test_dir();
        Db::open(dir.path())
            .unwrap()
            .meta_set(MANUAL_KEY, "{oops")
            .unwrap();
        let (sys, _, _) = fake();
        let state = start(dir.path(), sys);
        assert_eq!(status(&state).paused, None);
        assert_eq!(stored(&state), None);
        state.close();
    }

    #[test]
    fn auto_pause_follows_conditions_and_params() {
        let dir = crate::test_dir();
        let (sys, _, sensed) = fake();
        let state = start(dir.path(), sys);
        let set = |key: &str, value| {
            let body = Request::SetParam {
                key: key.into(),
                value,
            };
            assert_eq!(respond(body, &state), Ok(Reply::Done));
        };

        sensed.lock().unwrap().fullscreen = true;
        state.refresh_pause();
        assert_eq!(status(&state).paused, Some(PauseReason::Fullscreen));
        sensed.lock().unwrap().battery = true;
        state.refresh_pause();
        assert_eq!(status(&state).paused, Some(PauseReason::Battery));
        // 修改参数后立即生效
        set("pause_on_battery", ParamValue::Bool(false));
        assert_eq!(status(&state).paused, Some(PauseReason::Fullscreen));
        set("pause_on_fullscreen", ParamValue::Bool(false));
        assert_eq!(status(&state).paused, None);

        sensed.lock().unwrap().free_gb = Some(1.5);
        state.refresh_pause();
        assert_eq!(
            status(&state).paused,
            Some(PauseReason::DiskLow { free_gb: 1.5 })
        );
        set("min_free_disk_gb", ParamValue::Int(1));
        assert_eq!(status(&state).paused, None);

        // 手动暂停优先；取消后回到自动判定的结果
        set("pause_on_battery", ParamValue::Bool(true));
        assert_eq!(status(&state).paused, Some(PauseReason::Battery));
        let body = Request::PauseIndexing { until: None };
        assert_eq!(respond(body, &state), Ok(Reply::Done));
        assert_eq!(
            status(&state).paused,
            Some(PauseReason::Manual { until: None })
        );
        assert_eq!(respond(Request::ResumeIndexing, &state), Ok(Reply::Done));
        assert_eq!(status(&state).paused, Some(PauseReason::Battery));
        state.close();
    }

    #[test]
    fn background_quota_pauses_until_cleared() {
        let dir = crate::test_dir();
        let (sys, _, sensed) = fake();
        let state = start(dir.path(), sys);
        let quota = |state: &Arc<State>, exhausted| {
            let body = Request::BackgroundQuota { exhausted };
            assert_eq!(respond(body, state), Ok(Reply::Done));
        };
        quota(&state, true);
        assert_eq!(status(&state).paused, Some(PauseReason::Quota));
        // 过热排在额度之后
        sensed.lock().unwrap().thermal = resource::THERMAL_SEVERE;
        state.refresh_pause();
        assert_eq!(status(&state).paused, Some(PauseReason::Quota));
        quota(&state, false);
        assert_eq!(status(&state).paused, Some(PauseReason::Hot));
        sensed.lock().unwrap().thermal = 0;
        state.refresh_pause();
        assert_eq!(status(&state).paused, None);
        state.close();

        // 不持久化：重启后不再暂停
        let (sys, _, _) = fake();
        let state = start(dir.path(), sys);
        quota(&state, true);
        state.close();
        let (sys, _, _) = fake();
        let state = start(dir.path(), sys);
        assert_eq!(status(&state).paused, None);
        state.close();
    }
}
