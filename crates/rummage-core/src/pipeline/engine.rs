//! 推理引擎的创建与失效处理。
//! - 创建：按用户选择的设备（`preferred_device`）与自检记录选设备（`selftest::preferred_gpu`）：选了的显卡不能用时按自动选择，
//!   自动选择按性能顺序取第一个能用的设备，跳过本次运行中停用的显卡（`Shared::disabled`）；本次运行所有显卡都停用时、
//!   后台自动检测设备期间（`cpu_only`）只用 CPU。图片、音频、视频的开关变化时重建；视觉 token 档位不属于引擎（`Engine`），改了不重建。`hold` 期间不建引擎，
//!   检查与设置都在引擎槽的锁内。
//! - 失效（`engine_failed`）：设备丢失、后端失效、推理进程结束不了、显存不足，或显卡算出无效结果（NaN 等）时丢弃该引擎。
//!   换哪个设备按驱动证据门槛在丢弃时同步决定，新引擎建好之前不会初始化别的显卡：
//!   - 明确不是驱动问题的（找不到显卡即已拔出、算出 NaN、显存不足）：只停用这块显卡，换剩下的设备中最快的。
//!   - 其余（设备丢失、结束不了、崩溃、超时等无法排除驱动原因的，以及跨过睡眠的请求、唤醒验证失败）：本次运行停用所有显卡，
//!     只用 CPU，下次启动再按性能顺序选。
//!   - 推理进程结束不了（`Stuck`）：显卡上的按上一条处理（并持久屏蔽这块卡）；CPU 上的本次运行不再建任何引擎，
//!     内容识别不可用（状态为 `error`）。
//!
//!   是否持久屏蔽另按证据决定（`judge_gpu`）：设备丢失、结束不了，或后端失效且请求期间系统日志中有驱动事件时屏蔽
//!   （`selftest::blacklist_gpu`）；跨过睡眠的请求、没有驱动事件或读不到系统日志时只在本次运行内停用；
//!   找不到显卡、NaN、显存不足不查系统日志、不写记录。
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Instant;

use rummage_embed::{Backend, Error as EmbedError, selftest};
use rummage_proto::params;

use super::{Engine, EngineSpec, Msg, Released, Shared, Start, int, lock, space_row};

/// 推理后端失效、要换设备的错误：设备丢失、后端失效、结束不了、显存不足、找不到显卡，以及显卡算出无效结果
/// （CPU 上的按文件失败处理）
pub(super) fn backend_failed(engine: &Engine, err: &EmbedError) -> bool {
    match err {
        EmbedError::DeviceLost(_)
        | EmbedError::Unavailable(_)
        | EmbedError::GpuNotFound(_)
        | EmbedError::OutOfMemory(_)
        | EmbedError::Stuck(_) => true,
        EmbedError::BadVector(_) => engine.gpu.is_some(),
        _ => false,
    }
}

/// 设备丢失或推理进程结束不了：不必查系统日志就持久屏蔽
pub(super) fn lost(err: &EmbedError) -> bool {
    matches!(err, EmbedError::DeviceLost(_) | EmbedError::Stuck(_))
}

/// 明确不是驱动问题的显卡失效：已拔出、算出无效结果、显存不足。只有这些换下一块显卡，其余一律只用 CPU
fn not_driver(err: &EmbedError) -> bool {
    matches!(
        err,
        EmbedError::GpuNotFound(_) | EmbedError::BadVector(_) | EmbedError::OutOfMemory(_)
    )
}
impl Shared {
    /// 当前的推理引擎，需要时新建；主模型不可用或无法创建时为空
    pub(super) fn engine(&self, out: &Sender<Msg>) -> Option<Arc<Engine>> {
        let opts = &self.opts;
        let mut slot = lock(&self.engine);
        if self.held() {
            return None;
        }
        // CPU 的推理进程结束不了：本次运行不再建任何引擎
        if let Some(msg) = lock(&self.dead).clone() {
            *slot = None;
            self.set_error(Some(msg), out);
            return None;
        }
        let values = lock(&self.params).clone();
        // 已有引擎时按它的后端核对（换设备时引擎已被丢弃），不必每次都读自检记录、列出显卡
        if let Some(e) = &*slot
            && self.media_flags(&values, e.cfg.backend, out) == Some((e.images, e.audio, e.video))
        {
            return Some(e.clone());
        }
        // 先选设备再核对模型：LiteRT 的 NPU 与其他后端用不同的模型文件
        let gpu = self.chosen_gpu(&values, self.cpu_only.load(Ordering::Relaxed));
        let backend = gpu.as_ref().map_or(Backend::Cpu, |r| r.backend);
        let Some((images, audio, video)) = self.media_flags(&values, backend, out) else {
            *slot = None;
            return None;
        };
        let has_mmproj = !self.no_mmproj.load(Ordering::Relaxed);
        if !has_mmproj {
            log::warn!("多模态模型文件不可用，只为文本类文件建索引。");
        }
        let mmproj = images || audio || video;
        let spec = EngineSpec {
            backend,
            gpu: gpu.as_ref().and_then(|r| r.gpu()),
            mmproj,
            threads: int(&values, "index_threads").max(1) as u32,
            params: values,
        };
        let backend = match (opts.factory)(&spec) {
            Ok(b) => b,
            Err(err) => {
                log::error!("无法创建推理后端：{err}");
                *slot = None;
                self.set_error(
                    Some(format!(
                        "{}目前只能按文件名搜索。请重新启动 Rummage，或在「设置 → 设备」中选择使用 CPU。",
                        err.user_message()
                    )),
                    out,
                );
                return None;
            }
        };
        let cfg = opts.space(spec.backend);
        let engine = Arc::new(Engine {
            backend,
            device: gpu.as_ref().map_or_else(
                || rummage_embed::cpu_name().to_string(),
                |r| r.description.clone(),
            ),
            gpu: gpu.filter(|_| spec.gpu.is_some()),
            images,
            audio,
            video,
            text_space: space_row(&cfg, None),
            cfg,
            failed: AtomicBool::new(false),
            driver_failed: AtomicBool::new(false),
            retired: AtomicBool::new(false),
            released: Released(Arc::new(AtomicBool::new(false))),
        });
        *slot = Some(engine.clone());
        self.set_error(None, out);
        Some(engine)
    }

    /// 后端 `backend` 用的模型可用时，按参数与多模态模型是否可用返回要处理的图片、音频、视频；主模型（LiteRT 另要分词器）
    /// 不可用时为空。同时更新缺少模型的状态
    fn media_flags(
        &self,
        values: &std::collections::BTreeMap<String, rummage_proto::ParamValue>,
        backend: Backend,
        out: &Sender<Msg>,
    ) -> Option<(bool, bool, bool)> {
        let opts = &self.opts;
        let (model_file, mmproj_file) = opts.model_files(backend);
        let (model_path, mmproj_path) = opts.model_paths(backend);
        let tokenizer_ok = opts
            .litert
            .as_ref()
            .is_none_or(|f| (opts.available)(&f.tokenizer, &rummage_embed::space::TOKENIZER));
        if !tokenizer_ok || !(opts.available)(model_path, model_file) {
            self.set_flag(&self.no_model, true, out);
            return None;
        }
        self.set_flag(&self.no_model, false, out);
        let has_mmproj = (opts.available)(mmproj_path, mmproj_file);
        self.set_flag(&self.no_mmproj, !has_mmproj, out);
        let on = |k: &str| has_mmproj && params::flag(values, k);
        Some((on("index_images"), on("index_audio"), on("index_video")))
    }

    /// 按用户的选择与自检记录选用的显卡（LiteRT 的 GPU、NPU 也算）；用 CPU 时为空。本次运行所有显卡都停用、
    /// `cpu_only`（后台自动检测设备期间）时只用 CPU（见模块说明）
    pub(super) fn chosen_gpu(
        &self,
        values: &std::collections::BTreeMap<String, rummage_proto::ParamValue>,
        cpu_only: bool,
    ) -> Option<selftest::CandidateReport> {
        let (all, disabled) = {
            let d = lock(&self.disabled);
            (d.all, d.keys.clone())
        };
        if all || cpu_only {
            return None;
        }
        let any_media = ["index_images", "index_audio", "index_video"]
            .iter()
            .any(|k| params::flag(values, k));
        let preferred = lock(&self.preferred).clone();
        selftest::preferred_gpu(
            &self.opts.selftest_cache,
            &(self.opts.gpus)(),
            any_media && self.mmproj_available(Backend::Cpu),
            &preferred,
            &disabled,
        )
    }

    /// 当前时刻与挂起代数
    pub(super) fn start(&self) -> Start {
        Start {
            at: Instant::now(),
            generation: lock(&self.sleep).generation(),
        }
    }

    /// 丢弃失效的引擎并按驱动证据门槛停用显卡（见模块说明），再对 GPU 下结论（`judge_gpu`）。两步都在调用线程上同步执行；
    /// 查询系统日志要先等几秒让它写入，在释放引擎锁之后进行，不挡住其他线程取引擎
    pub(super) fn engine_failed(&self, engine: &Arc<Engine>, err: &EmbedError, start: Start) {
        if self.drop_engine(engine, err, start) {
            self.judge_gpu(engine, lost(err), &err.to_string(), start);
        }
    }

    /// `engine_failed` 的同步部分：标记失效、停用显卡（只停这一块还是全部，见模块说明），是当前引擎时清空引擎槽。
    /// 已退役的引擎（`reload` 等丢弃后还在处理作业）上出的错同样停用与下结论。同一个引擎只由第一次失效下结论，
    /// 其余并发请求随后报的错不再重复停用、等系统日志和写屏蔽记录；例外是先报显存不足等非驱动错误、随后才出现驱动类错误
    /// （如设备丢失），这时仍按驱动错误停用所有显卡并下结论。返回是否还要对 GPU 下结论（`judge_gpu`）
    pub(super) fn drop_engine(&self, engine: &Arc<Engine>, err: &EmbedError, start: Start) -> bool {
        if !backend_failed(engine, err) {
            return false;
        }
        let first = !engine.failed.swap(true, Ordering::Relaxed);
        let escalate = !not_driver(err) && !engine.driver_failed.swap(true, Ordering::Relaxed);
        if !first && !escalate {
            return false;
        }
        {
            let mut slot = lock(&self.engine);
            log::warn!("推理后端（{}）不可用：{err}", engine.device);
            // 只有 CPU 的推理进程卡死时停用内容识别；显卡卡死按驱动问题停用所有显卡、改用 CPU（CPU 后端不依赖显卡驱动）
            if let EmbedError::Stuck(_) = err
                && engine.gpu.is_none()
            {
                *lock(&self.dead) = Some(format!("{}目前只能按文件名搜索。", err.user_message()));
            }
            // 先于清空槽设置：否则调度线程可能在两步之间又按缓存在同一块 GPU 上建引擎
            let all = !not_driver(err) || lock(&self.sleep).overlaps(start.generation);
            self.disable_gpu(engine, all);
            // 槽中可能已换成别的引擎（另一块显卡，或 `reload` 后同一块显卡上新建的）：它的显卡也在停用之列时一并退役，
            // 否则它会继续使用，不受本次停用约束
            let stale = slot.as_ref().is_some_and(|e| {
                Arc::ptr_eq(e, engine)
                    || e.gpu.as_ref().is_some_and(|g| {
                        let d = lock(&self.disabled);
                        d.all || d.keys.contains(&selftest::report_key(g))
                    })
            });
            if stale {
                self.retire(&mut slot);
            }
        }
        engine.gpu.is_some() && !not_driver(err)
    }

    pub(super) fn held(&self) -> bool {
        self.held_setup.load(Ordering::Relaxed)
            || self.held_selftest.load(Ordering::Relaxed)
            || self.held_models.load(Ordering::Relaxed)
            || self.held_clear.load(Ordering::Relaxed)
    }

    /// 引擎用的是 GPU 时把它的键记入本次运行停用的集合（之后选设备、重新检测都跳过它），记下出问题的设备；
    /// `all` 时本次运行所有显卡都停用
    pub(super) fn disable_gpu(&self, engine: &Engine, all: bool) {
        let Some(g) = &engine.gpu else {
            return;
        };
        let key = selftest::report_key(g);
        let mut d = lock(&self.disabled);
        if !d.keys.contains(&key) {
            d.keys.push(key);
        }
        d.all |= all;
        d.failed = Some(g.device_id());
    }

    /// 决定 GPU 是否持久屏蔽（见模块说明）；`lost` 为设备丢失或推理进程结束不了
    pub(super) fn judge_gpu(&self, engine: &Engine, lost: bool, err: &str, start: Start) {
        let Some(gpu) = &engine.gpu else {
            return;
        };
        if lock(&self.sleep).overlaps(start.generation) {
            log::warn!("出错的请求跨过了系统睡眠，只在本次运行内停用显卡。");
            return;
        }
        let reason = if lost {
            err.to_string()
        } else {
            match (self.opts.driver_events)(start.at) {
                Ok(events) if !events.is_empty() => {
                    format!("{err}；期间系统日志中有显卡驱动事件：{}", events.join("；"))
                }
                Ok(_) => {
                    log::warn!("系统日志中没有显卡驱动事件，只在本次运行内停用显卡。");
                    return;
                }
                Err(e) => {
                    log::warn!("无法读取系统日志，只在本次运行内停用显卡：{e}");
                    return;
                }
            }
        };
        if let Err(e) = selftest::blacklist_gpu(&self.opts.selftest_cache, gpu, &reason) {
            log::warn!("无法写入显卡屏蔽记录：{e}");
        }
    }
}
