//! 设备检测在核心状态上的部分：手动与自动检测、检测状态、用户选择的设备、解除屏蔽。
//! Android 上的候选按已有的模型给出：通用版可用时测 CPU 与 GPU，本机芯片的 NPU 模型（含 QNN 运行库）可用时测 NPU；
//! 引导中只下了 NPU 版时只测 NPU，未通过时界面请用户下载通用版再检测。
//! 规则见上级模块（`setup`）说明中设备自检、用户选择的设备、本次运行中显卡出的问题、自动重新检测与解除屏蔽各条。
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use rummage_embed::selftest::{self, Candidate, CandidateReport, SelftestReport, Status};
use rummage_embed::{Backend, Limits};
use rummage_proto::{
    AUTO_DEVICE, ApiError, BlockedGpu, CPU_DEVICE, DeviceResult, DeviceStatus, Event, GpuState,
    ModelTaskKind, ModelTaskState, Reply, SelftestStatus,
};

use super::super::{State, api_error, db, lock};
use super::{Slot, bad_request, config, files, mmproj_on, release_limit, unavailable};
use crate::pipeline::{Config, Disabled, Hold, PREFERRED_DEVICE_KEY};

/// 检测的发起方式
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Mode {
    /// 用户在引导或设置页发起：全部重测，期间暂停建立索引
    Manual,
    /// 结果过期时核心在后台自动发起（见上级模块说明）
    Auto,
    /// 解除屏蔽后立即发起：只测解除的这块显卡（`selftest::Options::only`），其余设备照缓存原样显示；
    /// 其余按手动检测对待（暂停建立索引、不受暂停条件限制、出错时说明原因）
    Retest(String),
}

/// 用户选了某块显卡、它却不能用（不在最近一次检测中、没有通过、被屏蔽或已拔出）时的原因，流水线按自动选择；
/// 能用时为空。本次运行中出了问题而停用的不算在这里，见 `SelftestStatus::failed`
fn preferred_error(
    st: &SelftestStatus,
    cache: &Path,
    gpus: &[String],
    images: bool,
) -> Option<String> {
    if let Some(b) = st.blocked.iter().find(|b| b.id == st.preferred) {
        return Some(format!("所选的显卡「{}」已屏蔽，已改为自动选择。", b.name));
    }
    let Some(d) = st.devices.iter().find(|d| d.id == st.preferred) else {
        return Some("所选的显卡在最近一次检测中没有找到，已改为自动选择。".to_string());
    };
    if selftest::cached_gpu(cache, gpus, images, Some(&st.preferred), &[]).is_some() {
        return None;
    }
    Some(if d.status == DeviceStatus::Passed {
        format!(
            "所选的显卡「{}」目前不能使用，可能已拔出或出现过错误，已改为自动选择。",
            d.name
        )
    } else {
        format!(
            "所选的显卡「{}」没有通过最近一次检测，已改为自动选择。",
            d.name
        )
    })
}

impl State {
    /// 自检状态：本次运行还没有检测过时用缓存中最近一次的结果；填入用户选择的设备、实际使用的设备与结果是否需要重新检测
    pub(super) fn selftest_status(&self) -> SelftestStatus {
        let mut st = lock(&self.setup.selftest).clone();
        st.preferred = self.preferred_device();
        let Some(cfg) = self.config.get() else {
            return st;
        };
        let cache = &cfg.selftest_cache;
        let gpus = (cfg.gpus)();
        let records = selftest::gpu_records(cache, &gpus);
        st.blocked = records
            .blocked
            .iter()
            .map(|b| BlockedGpu {
                id: b.device_id(),
                name: b.description.clone(),
                pci_id: b.pci_id.clone(),
                reason: b.reason.clone(),
            })
            .collect();
        // 已屏蔽的显卡始终列在 `blocked` 中（可以解除），一般不出现在检测结果中（也就不能被选用）。例外是本次真正测它的
        // 检测（只测一块时只有那一块）：检测中正在测试的（不列等待中的，免得先显示“等待检测”又消失），以及测完未通过的（看得到原因）
        let blocked: Vec<String> = st.blocked.iter().map(|b| b.id.clone()).collect();
        let from_run = !st.devices.is_empty();
        let only = lock(&self.setup.only).clone();
        let tested = |id: &str| only.as_deref().is_none_or(|o| o == id);
        if st.running {
            st.devices.retain(|d| {
                !blocked.contains(&d.id) || (tested(&d.id) && d.status == DeviceStatus::Testing)
            });
            return st;
        }
        // 与流水线判断是否加载 mmproj 的条件相同
        let images = mmproj_on(&self.params()) && cfg.media_model_ok();
        if st.devices.is_empty()
            && st.error.is_none()
            && let Some(r) = selftest::last_report(&cfg.selftest_cache)
        {
            (st.devices, st.selected) = devices(&r);
        }
        if st.devices.is_empty() {
            // 已完成引导却没有任何检测结果（如上次列显卡卡住，结果已丢弃）：算过期，由后台自动检测
            st.stale = self.onboarded();
            return st;
        }
        st.devices.retain(|d| {
            !blocked.contains(&d.id)
                || (from_run && tested(&d.id) && d.status == DeviceStatus::Failed)
        });
        // 显卡现在的情况：不在系统中优先于已屏蔽，已屏蔽优先于本次运行停用
        {
            let d = lock(&self.disabled);
            for dev in st.devices.iter_mut().filter(|x| x.gpu) {
                dev.state = Some(if records.absent.contains(&dev.id) {
                    GpuState::Absent
                } else if blocked.contains(&dev.id) {
                    GpuState::Blocked
                } else if d.all || d.keys.iter().any(|k| k.device_id() == dev.id) {
                    GpuState::Disabled
                } else {
                    GpuState::Usable
                });
            }
        }
        let cpu_only = st.preferred == CPU_DEVICE;
        // 失效：记录中任何一个设备有变化都建议重新检测；实际使用的设备另按流水线的规则计算，两者分开。
        // 本次运行中检测过时，其中暂时性失败、被跳过的显卡下次启动才重测，不算过期
        let retry = !self.setup.tested.load(Ordering::Relaxed);
        st.stale = !selftest::cache_current(cache, &gpus, images, cpu_only, retry);
        if cpu_only {
            st.selected = st.devices.iter().position(|d| !d.gpu);
        } else {
            if st.preferred != AUTO_DEVICE {
                st.preferred_error = preferred_error(&st, cache, &gpus, images);
            }
            // 与流水线选设备的规则相同（`selftest::preferred_gpu`，这里不写日志）
            let d = lock(&self.disabled);
            st.gpus_off = d.all;
            let chosen = (st.preferred != AUTO_DEVICE && !d.all).then_some(st.preferred.as_str());
            let gpu = chosen
                .and_then(|p| selftest::cached_gpu(cache, &gpus, images, Some(p), &d.keys))
                .or_else(|| {
                    (!d.all)
                        .then(|| selftest::cached_gpu(cache, &gpus, images, None, &d.keys))
                        .flatten()
                });
            let id = gpu.map_or_else(|| CPU_DEVICE.to_string(), |r| r.device_id());
            st.selected = st.devices.iter().position(|x| x.id == id);
            // 运行中出了问题的设备（流水线直接记下），已改用别的设备时说明；它可能已屏蔽、只在 `blocked` 中
            st.failed = d.failed.clone().filter(|f| *f != id);
        }
        // 已有引擎时以它实际使用的设备为准，上面按缓存推算的只用于还没有引擎时
        if let Some(id) = self.pipeline.get().and_then(|p| p.engine_device())
            && let Some(i) = st.devices.iter().position(|d| d.id == id)
        {
            st.selected = Some(i);
        }
        st
    }

    /// 结果过期时在后台自动重新检测一次（条件见模块说明）；由暂停检测线程定时调用
    pub(in crate::server) fn auto_selftest(self: &Arc<Self>) {
        if self.setup.started.load(Ordering::Relaxed) {
            return;
        }
        let Some(p) = self.pipeline.get() else {
            return;
        };
        // 引导、模型任务、清空索引、手动检测中，或本次运行显卡出过错（不自动重试）
        if p.held() || p.gpu_disabled() {
            return;
        }
        let Ok(s) = p.status() else {
            return;
        };
        if s.paused.is_some() || (s.pending == 0 && s.rebuild.is_none()) {
            return;
        }
        let st = self.selftest_status();
        if st.running || !st.stale || st.preferred == CPU_DEVICE {
            return;
        }
        // 模型下载、导入或删除进行中时被拒绝，下次再试
        if start_selftest(self, false, Mode::Auto).is_ok() {
            log::info!("设备检测结果已过期，在后台自动重新检测。");
        }
    }

    /// 自动检测进行中进入任何暂停（含即将睡眠）时中止：结束检测子进程，结果不用，本次运行不再自动发起。手动检测不受影响
    pub(in crate::server) fn stop_auto_selftest(&self) {
        let st = lock(&self.setup.selftest);
        if st.running && st.auto && !self.setup.auto_cancel.swap(true, Ordering::Relaxed) {
            log::info!("建立索引已暂停，中止自动设备检测。");
        }
    }

    pub(super) fn broadcast_selftest(&self) {
        let _ = self.events.send(Event::Selftest(self.selftest_status()));
    }
}

fn device(r: &CandidateReport) -> DeviceResult {
    DeviceResult {
        id: r.device_id(),
        pci_id: r.pci_id.clone(),
        gpu: r.backend.accel(),
        // CPU 按本机当前的处理器型号显示，不用检测记录中保存的名称
        name: if r.backend == Backend::Cpu {
            rummage_embed::cpu_name().to_string()
        } else if r.description.is_empty() {
            r.name.clone()
        } else {
            r.description.clone()
        },
        status: match r.status {
            Status::Passed => DeviceStatus::Passed,
            Status::Failed => DeviceStatus::Failed,
            Status::Blacklisted => DeviceStatus::Blacklisted,
            Status::Skipped => DeviceStatus::Skipped,
        },
        reason: r.reason.clone(),
        text_ms: r.text_ms,
        image_ms: r.image_ms,
        audio_ms: r.audio_ms,
        video_ms: r.video_ms,
        speed: r.speed.clone(),
        state: None,
    }
}

pub(super) fn devices(report: &SelftestReport) -> (Vec<DeviceResult>, Option<usize>) {
    (
        report.candidates.iter().map(device).collect(),
        report.selected,
    )
}

/// 用户选择设备（`auto`、`cpu` 或显卡的标识）
pub(super) fn set_preferred_device(state: &Arc<State>, device: String) -> Result<Reply, ApiError> {
    if lock(&state.setup.selftest).running {
        return Err(bad_request("设备检测正在进行，请等它完成。"));
    }
    let old = state.preferred_device();
    if device == old {
        return Ok(Reply::Done);
    }
    // 命令行评测在 configure 之前设置，那时还没有自检记录，也没有流水线
    let images = state
        .config
        .get()
        .is_some_and(|cfg| mmproj_on(&state.params()) && cfg.media_model_ok());
    // 与流水线选设备的规则相同：本次运行中停用的显卡不用（所有显卡都停用时只用 CPU）
    let target = |d: &str| {
        let (all, disabled) = {
            let x = lock(&state.disabled);
            (x.all, x.keys.clone())
        };
        state
            .config
            .get()
            .filter(|_| !all)
            .and_then(|cfg| {
                selftest::preferred_gpu(&cfg.selftest_cache, &(cfg.gpus)(), images, d, &disabled)
            })
            .map(|r| r.device_id())
    };
    if device == CPU_DEVICE && !config(state)?.cpu_model_ok() {
        return Err(bad_request(
            "CPU 要用通用模型，请先在「设置 → 模型」中下载或导入。",
        ));
    }
    // 显卡要在自检记录中通过检测、未被屏蔽、仍在系统中（与流水线选用的条件相同）；本次运行中出了问题的也可以选，
    // 流水线照样跳过它，设备页说明改用了哪个设备
    if device != AUTO_DEVICE && device != CPU_DEVICE {
        let cfg = config(state)?;
        let gpus = (cfg.gpus)();
        if selftest::cached_gpu(&cfg.selftest_cache, &gpus, images, Some(&device), &[]).is_none() {
            return Err(bad_request(
                "这块显卡没有通过检测或目前不能使用，请重新检测后再选择。",
            ));
        }
    }
    let db = db(state)?;
    if device == AUTO_DEVICE {
        db.meta_delete(PREFERRED_DEVICE_KEY)
    } else {
        db.meta_set(PREFERRED_DEVICE_KEY, &device)
    }
    .map_err(unavailable)?;
    drop(db);
    // 当前引擎用的已经是新选择要用的设备（如自动选中的正是这块显卡）时不重建；还没有引擎时下次创建自然按新选择
    if let Some(p) = state.pipeline.get() {
        p.set_preferred_device(&device);
        let want = target(&device).unwrap_or_else(|| CPU_DEVICE.to_string());
        if p.engine_device().is_some_and(|cur| cur != want) {
            p.reload();
        }
    }
    state.broadcast_selftest();
    Ok(Reply::Done)
}

/// 解除屏蔽：删除这块显卡的全部屏蔽记录（`selftest::unblock_gpu`），随即开始只重测它的检测（`Mode::Retest`）。
/// 同时清掉它本次运行的停用：用户主动的操作不算自动重试，本次运行中出过驱动错误也当场重测。
/// 不当场重测、回复 `DoneWithWarning` 说明原因的情况：它不在系统中；缓存的结果没测过本次需要的图片与音视频
/// （只测一块要靠其余设备的缓存结果，这时请用户整体重新检测）；检测没能开始（缺模型、模型任务进行中等）。这时它在检测结果中显示为未检测，
/// 下次手动检测或下次启动后的自动检测时重测。在 `selftest` 锁内检查、修改并登记检测已开始（`started`），
/// 检测（含自动检测）不会在这期间抢先开始
/// `failed` 时改为删除它的未通过记录，重测未通过的设备
pub(super) fn unblock_gpu(
    state: &Arc<State>,
    device: String,
    failed: bool,
) -> Result<Reply, ApiError> {
    let done = if failed { "" } else { "已解除屏蔽。" };
    let cfg = config(state)?;
    let gpus = (cfg.gpus)();
    let (note, was_started) = {
        let mut st = lock(&state.setup.selftest);
        if st.running {
            return Err(bad_request(if failed {
                "设备检测正在进行，请等它完成后再重新检测。"
            } else {
                "设备检测正在进行，请等它完成后再解除屏蔽。"
            }));
        }
        let absent = selftest::gpu_records(&cfg.selftest_cache, &gpus)
            .absent
            .contains(&device);
        match selftest::clear_gpu_records(&cfg.selftest_cache, &device, failed) {
            Ok(true) => {}
            Ok(false) if failed => return Err(bad_request("这个设备没有未通过的记录。")),
            Ok(false) => return Err(bad_request("这块显卡没有被屏蔽。")),
            Err(err) => {
                log::error!("无法删除设备 {device} 的屏蔽或未通过记录：{err}");
                return Err(api_error(ApiError::INTERNAL, "操作失败，请重试。"));
            }
        }
        // 本次运行中检测过时显示的是内存中的结果，同样改为未检测
        for d in st.devices.iter_mut().filter(|d| d.id == device) {
            d.status = DeviceStatus::Skipped;
            d.reason = Some(selftest::UNBLOCKED_REASON.to_string());
            (d.text_ms, d.image_ms, d.audio_ms, d.video_ms) = (None, None, None, None);
            d.speed = None;
        }
        // 用户主动解除：清掉这块显卡本次运行的停用，当场重测（所有显卡都停用时，重测通过后只恢复它，见 `run_selftest`）
        lock(&state.disabled)
            .keys
            .retain(|k| k.device_id() != device);
        // 只测一块要靠其余设备的缓存结果；它们没测过本次需要的图片与音视频时，请用户整体重新检测
        let images = mmproj_on(&state.params()) && cfg.media_model_ok();
        let note = if absent {
            Some(format!(
                "{done}这块显卡目前不在系统中，重新接入后检测设备时会重新检测它。"
            ))
        } else if !selftest::cache_covers(&cfg.selftest_cache, images) {
            Some(format!("{done}检测结果需要更新，请重新检测全部设备。"))
        } else {
            None
        };
        let was = note.is_none() && state.setup.started.swap(true, Ordering::Relaxed);
        (note, was)
    };
    log::info!(
        "已清除设备 {device} 的{}记录。",
        if failed { "未通过" } else { "屏蔽" }
    );
    let note = match note {
        Some(n) => Some(n),
        None => match start_selftest(state, false, Mode::Retest(device)) {
            Ok(_) => None,
            Err(err) => {
                // 另一次检测已经开始时它已登记，不恢复
                if !lock(&state.setup.selftest).running {
                    state.setup.started.store(was_started, Ordering::Relaxed);
                }
                Some(format!("{done}{}", err.message))
            }
        },
    };
    let Some(message) = note else {
        return Ok(Reply::Done);
    };
    log::info!("清除记录后不当场重新检测：{message}");
    state.broadcast_selftest();
    Ok(Reply::DoneWithWarning { message })
}

/// 开始设备检测，`mode` 见 `Mode`
pub(super) fn start_selftest(
    state: &Arc<State>,
    onboarding: bool,
    mode: Mode,
) -> Result<Reply, ApiError> {
    let cfg = config(state)?.clone();
    if !cfg.text_model_ok() {
        return Err(bad_request("请先下载或导入模型，再检测设备。"));
    }
    // 本次运行中显卡出过驱动错误时不自动重试（自动检测由 `auto_selftest` 拦下）；用户手动检测或解除屏蔽可以当场重测
    {
        let mut st = lock(&state.setup.selftest);
        if st.running {
            return Err(bad_request("设备检测正在进行，请等它完成。"));
        }
        // 下载、导入或删除会替换或删掉检测要打开的模型文件；校验只读，不拦
        let busy = lock(&state.setup.task)
            .as_ref()
            .is_some_and(|t| t.state == ModelTaskState::Running && t.kind != ModelTaskKind::Verify);
        if busy {
            return Err(bad_request(
                "正在下载、导入或删除模型，请等它完成后再检测设备。",
            ));
        }
        *st = SelftestStatus {
            running: true,
            auto: mode == Mode::Auto,
            ..SelftestStatus::default()
        };
        state.setup.started.store(true, Ordering::Relaxed);
        *lock(&state.setup.only) = match &mode {
            Mode::Retest(d) => Some(d.clone()),
            _ => None,
        };
    }
    state.broadcast_selftest();
    let s = state.clone();
    let spawned = std::thread::Builder::new()
        .name("selftest".into())
        .spawn(move || run_selftest(&s, &cfg, onboarding, mode));
    if let Err(err) = spawned {
        log::error!("无法启动设备检测线程：{err}");
        *lock(&state.setup.selftest) = SelftestStatus::default();
        state.broadcast_selftest();
        return Err(api_error(ApiError::INTERNAL, "无法开始设备检测，请重试。"));
    }
    Ok(Reply::Done)
}

/// 检测拿到报告后更新本次运行的停用（`Disabled`）。用户主动的检测以结果为准、清除停用（不算自动重试，用户手动检测
/// 或解除屏蔽可以当场重测）：手动检测测了所有显卡，停用全部清除（`save` 为否，即列显卡卡住、本次又停用所有显卡时除外）；
/// 解除屏蔽后的重测只测一块，它通过而此前所有显卡都已停用时只恢复它，其他显卡改为逐块停用，到重新启动为止：
/// 本次报告中的显卡以外，检测前缓存报告（`previous`）中的显卡也停用，当时拔出、之后插回的也照样停用。
/// 出问题的设备（`failed`）：手动检测完成（`save`）后清除，此刻已屏蔽（`blocked`，设备标识）的除外，结果以本次为准；
/// 解除屏蔽后的重测只看那一块，通过时清除。暂时性失败的显卡本次运行中停用
pub(super) fn after_report(
    d: &mut Disabled,
    mode: &Mode,
    report: &SelftestReport,
    previous: Option<&SelftestReport>,
    blocked: &[String],
    save: bool,
) {
    let passed = |id: &str| {
        report
            .candidates
            .iter()
            .any(|r| r.status == Status::Passed && r.device_id() == id)
    };
    match mode {
        Mode::Manual if save => {
            d.keys.clear();
            d.all = false;
        }
        Mode::Retest(id) if d.all && passed(id) => {
            d.all = false;
            let others = report
                .candidates
                .iter()
                .chain(previous.iter().flat_map(|p| p.candidates.iter()))
                .filter(|r| r.backend.accel() && r.device_id() != *id);
            for r in others {
                let key = selftest::report_key(r);
                if !d.keys.contains(&key) {
                    d.keys.push(key);
                }
            }
        }
        _ => {}
    }
    let clear = d.failed.as_deref().is_some_and(|f| match mode {
        Mode::Manual if save => !blocked.iter().any(|b| b == f),
        Mode::Manual => passed(f),
        Mode::Retest(id) => f == id && passed(f),
        Mode::Auto => false,
    });
    if clear {
        d.failed = None;
    }
    for r in report.candidates.iter().filter(|r| r.transient) {
        let key = selftest::report_key(r);
        if !d.keys.contains(&key) {
            d.keys.push(key);
        }
    }
}

pub(super) fn run_selftest(state: &State, cfg: &Config, onboarding: bool, mode: Mode) {
    let auto = mode == Mode::Auto;
    // 解除屏蔽后的重测出错时照旧显示缓存中的结果（解除的显卡为未检测），并说明原因
    let cached = || {
        matches!(mode, Mode::Retest(_))
            .then(|| selftest::last_report(&cfg.selftest_cache))
            .flatten()
            .map(|r| devices(&r).0)
            .unwrap_or_default()
    };
    let values = state.params();
    let images = mmproj_on(&values) && cfg.media_model_ok();
    let pipeline = state.pipeline.get();
    // 自动检测期间照常用 CPU 建索引，只等用显卡的旧引擎释放；手动检测暂停建索引。选用 NPU 时自动检测也暂停：
    // CPU 用的通用版与 NPU 的模型不同，这段时间建的向量检测完还要重建，查询也比较不了 NPU 的向量
    let cpu_only = auto && pipeline.is_some_and(|p| !p.npu_chosen());
    if let Some(p) = pipeline {
        let ok = if cpu_only {
            let released = p.cpu_only(true);
            p.wait_engine_released(released, release_limit(&values))
        } else {
            let released = p.hold(Hold::Selftest, true);
            p.wait_released(released, release_limit(&values))
        };
        if !ok {
            log::warn!("等待处理中的文件结束超时，本次不检测设备。");
            if cpu_only {
                p.cpu_only(false);
            } else {
                p.hold(Hold::Selftest, false);
            }
            // 没有开始检测：界面照旧显示上次的结果与选中的设备；自动检测出错只写日志
            let (shown, selected) = selftest::last_report(&cfg.selftest_cache)
                .map(|r| devices(&r))
                .unwrap_or_default();
            *lock(&state.setup.selftest) = SelftestStatus {
                devices: shown,
                selected,
                error: (!auto)
                    .then(|| "正在处理的文件长时间没有结束，暂时无法检测设备，请稍后再试。".into()),
                ..SelftestStatus::default()
            };
            state.broadcast_selftest();
            return;
        }
    }
    // 等引擎释放期间已被中止（进入暂停）时，不再在探测子进程中初始化显卡
    if auto && state.setup.auto_cancel.load(Ordering::Relaxed) {
        log::info!("自动设备检测在开始前被中止。");
        if let Some(p) = pipeline {
            if cpu_only {
                p.cpu_only(false);
            } else {
                p.hold(Hold::Selftest, false);
            }
        }
        *lock(&state.setup.selftest) = SelftestStatus::default();
        state.broadcast_selftest();
        return;
    }
    let mut candidates = Vec::new();
    if cfg.litert.is_some() {
        // 通用版测 CPU 与 GPU；NPU 用本机芯片的 NPU 模型（见模块说明）
        if files(cfg, Slot::Main).is_some_and(|f| f.ready()) {
            candidates.extend([Candidate::Cpu, Candidate::Litert(Backend::Gpu)]);
        }
        if files(cfg, Slot::Npu).is_some_and(|f| f.ready()) {
            candidates.push(Candidate::Litert(Backend::Npu));
        }
    } else {
        candidates.push(Candidate::Cpu);
    }
    // 列出显卡要在探测子进程中初始化 Vulkan；发布版没有 Vulkan 运行库时只测 CPU
    // 列显卡的进程结束不了：显卡驱动可能已停止响应，本次运行停用所有显卡，只测 CPU 且结果不写缓存（下次启动仍要检测）
    let mut save = true;
    if cfg.litert.is_none() && cfg.vulkan_runtime.is_dir() {
        match rummage_embed::list_devices(
            &cfg.exe,
            &cfg.vulkan_runtime,
            &Limits::from_params(&values),
        ) {
            Ok(gpus) => candidates.extend(gpus.into_iter().map(Candidate::Gpu)),
            Err(err @ rummage_embed::Error::Stuck(_)) => {
                log::error!("列出显卡的进程结束不了，本次运行只用 CPU：{err}");
                lock(&state.disabled).all = true;
                save = false;
            }
            Err(err) => log::warn!("无法列出显卡，只检测 CPU：{err}"),
        }
    }
    // 解除屏蔽后的重测：检测前的缓存报告，恢复显卡时据此停用其余显卡（含当时拔出的，见 `after_report`）
    let previous = matches!(mode, Mode::Retest(_))
        .then(|| selftest::last_report(&cfg.selftest_cache))
        .flatten();
    // 只测一块时其余设备照上次的结果显示，只有这一块回到等待检测，列表不整体清空
    let shown = |id: &str| match &mode {
        Mode::Retest(d) if d != id => previous
            .as_ref()
            .and_then(|r| r.candidates.iter().find(|c| c.device_id() == id))
            .map(device),
        _ => None,
    };
    lock(&state.setup.selftest).devices = candidates
        .iter()
        .map(|c| {
            shown(&c.device_id()).unwrap_or_else(|| DeviceResult {
                id: c.device_id(),
                pci_id: match c {
                    Candidate::Gpu(g) => g.pci_id.clone(),
                    _ => None,
                },
                gpu: !matches!(c, Candidate::Cpu),
                name: match c {
                    Candidate::Cpu => rummage_embed::cpu_name().to_string(),
                    Candidate::Gpu(g) => g.description.clone(),
                    Candidate::Litert(b) => b.as_str().to_uppercase(),
                },
                status: DeviceStatus::Pending,
                reason: None,
                text_ms: None,
                image_ms: None,
                audio_ms: None,
                video_ms: None,
                speed: None,
                state: None,
            })
        })
        .collect();
    state.broadcast_selftest();
    let opts = selftest::Options {
        exe: cfg.exe.clone(),
        golden_dir: cfg.golden.clone(),
        cpu_runtime: cfg.cpu_runtime.clone(),
        vulkan_runtime: candidates
            .iter()
            .any(|c| matches!(c, Candidate::Gpu(_)))
            .then(|| cfg.vulkan_runtime.clone()),
        model: cfg.model.clone(),
        mmproj: cfg.mmproj.clone(),
        cache_file: cfg.selftest_cache.clone(),
        // 重新测试；屏蔽与未通过的记录仍然有效
        use_cache: false,
        save,
        images,
        auto,
        only: match &mode {
            Mode::Retest(d) => Some(d.clone()),
            _ => None,
        },
        video: crate::pipeline::speed_video(&values),
        // 手动检测不会被中止
        cancel: if auto {
            state.setup.auto_cancel.clone()
        } else {
            Arc::default()
        },
        litert: cfg.litert.clone(),
    };
    // 手动检测全部重测，本次运行停用的显卡也测；自动检测与重测跳过它们（重测的这块已在解除时清除）
    let disabled = match mode {
        Mode::Manual => Vec::new(),
        _ => lock(&state.disabled).keys.clone(),
    };
    let res = selftest::run(&opts, &candidates, &values, &disabled, &mut |i, done| {
        if let Some(d) = lock(&state.setup.selftest).devices.get_mut(i) {
            match done {
                Some(r) => *d = device(r),
                None => d.status = DeviceStatus::Testing,
            }
        }
        state.broadcast_selftest();
    });
    let got = res.is_ok();
    // 读缓存文件、列系统显卡，放在锁外，不挡住 `selftest_status`
    let blocked: Vec<String> = if got {
        selftest::gpu_records(&cfg.selftest_cache, &(cfg.gpus)())
            .blocked
            .iter()
            .map(|b| b.device_id())
            .collect()
    } else {
        Vec::new()
    };
    {
        let mut st = lock(&state.setup.selftest);
        st.running = false;
        match res {
            Ok(report) => {
                (st.devices, st.selected) = devices(&report);
                state.setup.tested.store(true, Ordering::Relaxed);
                after_report(
                    &mut lock(&state.disabled),
                    &mode,
                    &report,
                    previous.as_ref(),
                    &blocked,
                    save,
                );
            }
            // 自动检测出错或被中止只写日志，界面照旧显示上次的结果（`selftest_status` 在本次没有结果时读缓存）
            Err(err) if auto => {
                log::warn!("自动设备检测没有完成：{err}");
                *st = SelftestStatus::default();
            }
            Err(err) => {
                log::error!("设备检测无法进行：{err}");
                st.devices = cached();
                st.error = Some(format!("设备检测无法进行：{}", err.user_message()));
            }
        }
    }
    if let Some(p) = pipeline {
        // 拿到报告才按检测结果选择。引导中选了只用 CPU 之后又检测，是要用显卡，改回自动选择；
        // 设置页中用户的选择保留。检测出错时保持原来的选择
        if got && onboarding && state.preferred_device() == CPU_DEVICE {
            if let Ok(core) = &state.db
                && let Err(err) = lock(&core.db).meta_delete(PREFERRED_DEVICE_KEY)
            {
                log::warn!("无法清除所选的设备：{err}");
            }
            p.set_preferred_device(AUTO_DEVICE);
        }
        // 自动检测结束时 `cpu_only(false)` 按新记录重建引擎：通过就切回显卡
        if cpu_only {
            p.cpu_only(false);
        } else {
            if got {
                p.reload();
            }
            p.hold(Hold::Selftest, false);
        }
    }
    state.broadcast_selftest();
}
