//! 首次使用引导在核心状态上的部分：
//! - 引导是否完成记在 `meta` 表的 `onboarded`（`1` / `0`）。全新的数据目录没有这一项，由 `configure` 记为未完成。
//!   未完成时流水线保持 `Hold::Setup`，只扫描、建文件名索引，`FinishSetup` 后才开始建内容索引。
//!   引导进行到的步骤记在 `meta` 表的 `onboarding_step`，随数据目录迁移，重启后界面从这一步继续；`FinishSetup` 时删除。
//! - 模型按界面上的一行（`Slot`）处理：电脑为文字模型与图片与音视频模型；Android 为 LiteRT 通用版（含全部模态，`mmproj`
//!   与它相同）与本机芯片的 NPU 模型（芯片有 NPU 版时）。一行可以有多个文件（分词器、QNN 运行库），一起下载、校验；
//!   删除 NPU 模型时连同 QNN 运行库，分词器留给通用版。
//! - 模型下载、导入、校验与删除（`models`，每一行都能删除）在专用线程中进行，同一时间只有一个任务。核心常驻，界面重启后用 `SetupStatus`
//!   取回进度；进度以 `ModelTask` 事件广播，完成后唤醒索引流水线。除校验外，任务开始前流水线 `hold` 并等推理进程退出，
//!   被它打开的模型文件才能替换或删除。设备检测的子进程也打开着模型文件，所以检测与下载、导入、删除互斥：两边都在
//!   `selftest` 锁内检查对方并登记自己（锁的顺序固定为先 `selftest` 后 `task`），不留空隙。校验只读，不受限制。
//! - 每个模型文件上次完整校验的结果记在 `meta` 表的 `model_verified:<文件名>`（`1:<大小>:<修改时间>` / `0:…`）：下载、导入成功时
//!   记为通过，手动校验与引导中进入下载模型一步时的校验（`VerifyModels`，依次校验没有记录的文件）记下结果，删除时清除。
//!   文件的大小或修改时间与记录不符（手放、替换了文件）时当作没有记录。
//! - 设备自检在专用线程中运行 `selftest::run`（每个候选都在可强行结束的推理子进程中，GPU 有总时限）。开始前流水线
//!   `hold`，并等处理中的作业结束、旧引擎的推理进程退出（最多 `file_timeout_s + infer_timeout_s`）。拿到报告后让流水线
//!   `reload`，按新记录选择后端，不需要重启；引导中发起、用户此前选了 CPU 时改回自动选择，其余情况保留用户的选择；
//!   检测出错时保持原来的选择。
//! - 用户选择的设备记在 `meta` 表的 `preferred_device`（`pipeline::PREFERRED_DEVICE_KEY`），没有这一项时自动选择。
//!   只能选 CPU 或自检记录中通过检测、目前能用的显卡；选了的显卡之后不能用时流水线按自动选择，`SelftestStatus::preferred_error`
//!   说明原因。
//! - 本次运行中显卡出的问题（`State::disabled`，与流水线共用）：停用的显卡（自检中暂时性失败的，以及流水线运行中失效、
//!   唤醒验证失败的）自动检测时直接跳过，流水线选设备时也跳过；无法排除驱动原因时所有显卡都停用、只用 CPU，本次运行
//!   也不再自动检测设备（不自动重试）。用户主动的检测不算自动重试：手动检测测所有显卡，完成后
//!   停用全部清除；解除屏蔽时清掉这块显卡的停用并当场重测，所有显卡都已停用时，它通过后只恢复它，其他显卡（含上次检测中有、当时已拔出的）逐块停用到重新启动为止。流水线记下最近出问题的设备，它与实际使用的设备不同时由
//!   `SelftestStatus::failed` 指出（屏蔽后照样指出）；手动检测完成后以本次结果为准、不再指出，此刻已屏蔽的除外。每块显卡现在的情况（可用、本次运行停用、不在系统中、已屏蔽）
//!   由 `DeviceResult::state` 给出；已屏蔽的显卡一般不在检测结果中（本次测过且未通过时例外），由 `SelftestStatus::blocked` 列出，界面据此说明出问题的设备重启后是否照常使用。
//! - 解除屏蔽（`UnblockGpu`）：删除这块显卡的全部屏蔽记录，缓存的结果中它改为“已跳过”（界面显示为未检测），随即开始
//!   只测它的检测（`selftest::Mode::Retest`：其余设备照缓存原样显示、不测试，保存时只替换缓存中这一项；其余按手动检测对待，
//!   暂停建立索引、不受暂停条件限制、出错时说明原因）。不在系统中、缓存结果没测过本次需要的图片与音视频（请用户整体重新检测），或检测没能开始（缺模型、模型任务进行中）时不当场重测，
//!   回复说明原因，它保持未检测，下次手动检测或下次启动后的自动检测时重测；出错时同样保持未检测。手动检测中通过的显卡
//!   同样删除屏蔽记录。检测进行中不能解除。未通过的设备可以同样重测（`UnblockGpu` 的 `failed`）：删除它的未通过记录，其余流程相同。
//! - 缓存的自检结果过期时标记 `stale`：显卡有增减或更换、运行库或模型变化、所需模态（`index_images`、`index_audio`
//!   或 `index_video` 开启且 mmproj 可用）上次没测，或有暂时性失败、被跳过的显卡（本次运行中检测过时不算，下次启动重测）；
//!   只更新驱动不算（见 `selftest` 的 `cache` 模块）。
//! - 过期后自动在后台重新检测（`auto_selftest`，由暂停检测线程每 5 秒调用）：只在有待建立索引（含重建）的文件、
//!   不处于任何暂停（手动、电池、全屏、磁盘空间、睡眠）、不在引导、模型任务或清空索引中、本次运行显卡没出过错、
//!   用户没有选择只用 CPU 时开始；每次运行只发起一次（失败或超时也不再自动重试），用户本次运行中手动检测过也不再发起。
//!   检测期间流水线不 `hold`，改为 `Pipeline::cpu_only` 暂时只用 CPU、照常建索引（选用 NPU 时仍 `hold`：CPU 用的通用版与 NPU 的模型不同）；只测失效的部分（显卡有增减或更换时只测变化的显卡；运行库或模型变化、新需要图片与音视频模型时 CPU 也重测），
//!   不测有过屏蔽记录的显卡（`selftest::Options::auto`）。通过时按新记录切回显卡，不提示；未通过时保持 CPU，
//!   `SelftestStatus::auto` 让设置页说明原因。
//!   与模型任务的互斥、界面显示的检测状态与手动检测相同。
//!   进行中进入任何暂停（含即将睡眠）时中止（`stop_auto_selftest`，结束检测子进程，不缓存结果；已测完的显卡的屏蔽与未通过记录照常保存，正在测的显卡的子进程结束不了时屏蔽它）。自动检测出错
//!   （等引擎释放超时、样本无效）或被中止时只写日志，不写 `SelftestStatus::error`，界面照旧显示上次缓存的结果。
//! - 设备检测、检测状态与选择设备的实现在 `selftest` 子模块。
//! - 预估只给出各文件夹按种类的数量与大小、长图切片的段数（每个文件夹随机抽 `ESTIMATE_SAMPLE` 张图片只读文件头，按当前参数
//!   切段后放大到图片总数），以及数据目录所在磁盘的剩余空间，时间与空间由界面按自检速度换算。
mod selftest;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rummage_embed::npu::{self, QnnEntry};
use rummage_embed::space::{self, ModelFile};
use rummage_index::Db;
use rummage_proto::{
    AUTO_DEVICE, ApiError, CPU_DEVICE, Estimate, Event, KindTotal, ModelFileStatus, ModelTask,
    ModelTaskKind, ModelTaskPhase, ModelTaskState, ParamValue, Reply, Request, RootEstimate,
    SelftestStatus, SetupStatus, params,
};

use super::{State, api_error, db, lock, unavailable};
use crate::models::{self, Pinned, Progress};
use crate::pipeline::{
    self, Config, Hold, ONBOARDED_KEY, ONBOARDING_STEP_KEY, PREFERRED_DEVICE_KEY,
};
use crate::proxy::{self, Route, SaveError};

const VERIFIED_KEY: &str = "model_verified:";

/// 界面上的一行模型（见模块说明）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Main,
    Mmproj,
    Npu,
}

impl Slot {
    fn of(mmproj: bool, npu: bool) -> Slot {
        match (npu, mmproj) {
            (true, _) => Slot::Npu,
            (false, true) => Slot::Mmproj,
            (false, false) => Slot::Main,
        }
    }
}

/// 一行模型的文件：从 Hugging Face 下载的（第一个是这一行的主文件）与 QNN 运行库，各带本地路径
struct Files {
    /// 给用户看的名称
    label: &'static str,
    hf: Vec<(Pinned, PathBuf)>,
    qnn: Vec<(&'static QnnEntry, PathBuf)>,
}

impl Files {
    fn main(&self) -> &(Pinned, PathBuf) {
        &self.hf[0]
    }

    /// 全部文件（含 QNN 运行库）与路径
    fn all(&self) -> Vec<(&'static ModelFile, PathBuf)> {
        let hf = self.hf.iter().map(|(p, path)| (p.file, path.clone()));
        let qnn = self.qnn.iter().map(|(e, path)| (&e.file, path.clone()));
        hf.chain(qnn).collect()
    }

    fn ready(&self) -> bool {
        self.all().iter().all(|(f, path)| f.check(path).is_ok())
    }
}

/// 一行模型的文件；这个平台没有这一行时为空（电脑没有 NPU；Android 的 mmproj 就是通用版，按 `Main` 处理）
fn files(cfg: &Config, slot: Slot) -> Option<Files> {
    let Some(lite) = &cfg.litert else {
        let (file, path, label) = match slot {
            Slot::Main => (&space::MODEL, &cfg.model, "文字模型"),
            Slot::Mmproj => (&space::MMPROJ, &cfg.mmproj, "图片与音视频模型"),
            Slot::Npu => return None,
        };
        return Some(Files {
            label,
            hf: vec![(Pinned::gguf(file), path.clone())],
            qnn: Vec::new(),
        });
    };
    let tokenizer = (Pinned::TOKENIZER, lite.tokenizer.clone());
    if slot != Slot::Npu {
        return Some(Files {
            label: "通用模型",
            hf: vec![
                (Pinned::litert(&space::LITERT_MODEL), cfg.model.clone()),
                tokenizer,
            ],
            qnn: Vec::new(),
        });
    }
    let (m, (path, dir)) = (npu::local()?, lite.npu.as_ref()?);
    Some(Files {
        label: "NPU 模型",
        hf: vec![(Pinned::litert(&m.file), path.clone()), tokenizer],
        qnn: npu::qnn_entries(m.vendor)
            .into_iter()
            .map(|e| (e, dir.join(e.file.name)))
            .collect(),
    })
}

/// 全部行的 Hugging Face 文件（导入时按它识别），同一个文件只列一次
fn known_files(cfg: &Config) -> Vec<&'static ModelFile> {
    let mut v: Vec<&'static ModelFile> = Vec::new();
    for slot in [Slot::Main, Slot::Mmproj, Slot::Npu] {
        for (p, _) in files(cfg, slot).map(|f| f.hf).unwrap_or_default() {
            if !v.iter().any(|f| f.name == p.file.name) {
                v.push(p.file);
            }
        }
    }
    v
}

/// 引导相关的运行状态
#[derive(Default)]
pub(super) struct Setup {
    /// 最近一次模型任务
    task: Mutex<Option<ModelTask>>,
    /// 当前任务的取消标志
    cancel: Mutex<Arc<AtomicBool>>,
    /// 本次运行中最近一次自检的状态；`preferred`、`stale` 等在读取时填入
    selftest: Mutex<SelftestStatus>,
    /// 本次运行中拿到过检测报告（已写入缓存）：其中暂时性失败、被跳过的显卡下次启动才重测，现在不算过期
    tested: AtomicBool,
    /// 本次运行中开始过检测（自动或手动），之后不再自动发起
    started: AtomicBool,
    /// 自动检测的中止标志（`State::stop_auto_selftest`）；每次运行只自动检测一次，不需要复位
    auto_cancel: Arc<AtomicBool>,
    /// 最近一次检测只测的设备（解除屏蔽后的重测，`selftest::Mode::Retest`）；其他检测为空。
    /// 判断检测结果中哪些已屏蔽的显卡是本次真正测过的
    only: Mutex<Option<String>>,
}

fn bad_request(msg: impl Into<String>) -> ApiError {
    api_error(ApiError::BAD_REQUEST, msg)
}

pub(super) fn config(state: &State) -> Result<&Config, ApiError> {
    state
        .config
        .get()
        .ok_or_else(|| api_error(ApiError::UNAVAILABLE, "后台服务尚未就绪，请稍后再试。"))
}

fn models_dir(cfg: &Config) -> PathBuf {
    cfg.model
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// 开启了需要 mmproj 的类型（图片、音频或视频）
fn mmproj_on(values: &std::collections::BTreeMap<String, ParamValue>) -> bool {
    ["index_images", "index_audio", "index_video"]
        .iter()
        .any(|k| params::flag(values, k))
}

/// 等引擎释放的上限：处理中的作业最多还要 `file_timeout_s`，推理最多 `infer_timeout_s`
pub(super) fn release_limit(values: &std::collections::BTreeMap<String, ParamValue>) -> Duration {
    let secs = ["file_timeout_s", "infer_timeout_s"]
        .iter()
        .map(|k| pipeline::int(values, k).max(0) as u64)
        .sum();
    Duration::from_secs(secs)
}

/// 文件的大小与修改时间（纳秒），用来判断校验记录是否还对应这个文件；读不到时为空
fn file_stamp(path: &Path) -> Option<String> {
    let m = std::fs::metadata(path).ok()?;
    let t = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(format!("{}:{}", m.len(), t.as_nanos()))
}

/// 模型文件上次完整校验的结果；没有记录，或记录对应的不是现在这个文件时为空
fn recorded(db: &Db, name: &str, path: &Path) -> Result<Option<bool>, ApiError> {
    let v = db
        .meta_get(&format!("{VERIFIED_KEY}{name}"))
        .map_err(unavailable)?;
    Ok(v.and_then(|v| {
        let (ok, stamp) = v.split_once(':')?;
        (Some(stamp) == file_stamp(path).as_deref()).then_some(ok == "1")
    }))
}

/// 记下模型文件 `path` 上次完整校验的结果 `ok`，连同它现在的大小与修改时间；`None` 或读不到文件时清除。
/// 返回是否按要求写上了（数据库不可用、读不到文件或写入失败时为否）
fn set_verified(state: &State, name: &str, result: Option<(&Path, bool)>) -> bool {
    let Ok(core) = &state.db else {
        log::warn!("索引数据库不可用，无法记录模型的校验结果。");
        return false;
    };
    let db = lock(&core.db);
    let key = format!("{VERIFIED_KEY}{name}");
    let stamp = result.and_then(|(path, ok)| Some((ok, file_stamp(path)?)));
    let res = match &stamp {
        Some((ok, stamp)) => db.meta_set(&key, &format!("{}:{stamp}", u8::from(*ok))),
        None => db.meta_delete(&key),
    };
    if let Err(err) = res {
        log::warn!("无法记录模型的校验结果：{err}");
        return false;
    }
    result.is_none() || stamp.is_some()
}

impl State {
    /// 交给核心运行库、模型与黄金样本的位置；数据库可用且还没有引导记录（全新的数据目录）时记为未完成。
    /// 会读写数据库，在阻塞线程中调用
    pub fn configure(&self, cfg: Config) {
        if let Ok(core) = &self.db {
            let db = lock(&core.db);
            match db.meta_get(ONBOARDED_KEY) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    if let Err(err) = db.meta_set(ONBOARDED_KEY, "0") {
                        log::warn!("无法记录引导状态：{err}");
                    }
                }
                Err(err) => log::warn!("无法读取引导状态：{err}"),
            }
        }
        let _ = self.config.set(cfg);
    }

    fn update_task(&self, f: impl FnOnce(&mut ModelTask)) {
        let mut task = lock(&self.setup.task);
        if let Some(t) = task.as_mut() {
            f(t);
            // 在锁内广播，事件顺序与修改顺序一致；没有订阅者时 send 返回错误，忽略
            let _ = self.events.send(Event::ModelTask(t.clone()));
        }
    }

    /// 用户选择的设备，没有选择或读取失败时为 `auto`
    fn preferred_device(&self) -> String {
        let saved = match &self.db {
            Ok(core) => lock(&core.db)
                .meta_get(PREFERRED_DEVICE_KEY)
                .unwrap_or_else(|err| {
                    log::warn!("无法读取所选的设备：{err}");
                    None
                }),
            Err(_) => None,
        };
        saved.unwrap_or_else(|| AUTO_DEVICE.to_string())
    }

    /// 已完成引导；数据库不可用或读取失败时为否
    fn onboarded(&self) -> bool {
        let Ok(core) = &self.db else {
            return false;
        };
        lock(&core.db)
            .meta_get(ONBOARDED_KEY)
            .is_ok_and(|v| v.as_deref() == Some("1"))
    }
}

/// 引导相关的请求（`Request::SetupStatus` 等，见 `rummage_proto::Request`）
pub(super) fn respond(state: &Arc<State>, body: Request) -> Result<Reply, ApiError> {
    match body {
        Request::SetupStatus => status(state).map(|s| Reply::Setup(Box::new(s))),
        Request::FinishSetup => {
            let db = db(state)?;
            db.meta_delete(ONBOARDING_STEP_KEY).map_err(unavailable)?;
            db.meta_set(ONBOARDED_KEY, "1").map_err(unavailable)?;
            drop(db);
            if let Some(p) = state.pipeline.get() {
                p.hold(Hold::Setup, false);
            }
            Ok(Reply::Done)
        }
        Request::DownloadModels {
            source,
            mmproj,
            npu,
        } => {
            let base = models::base_url(&source).map_err(bad_request)?;
            let cfg = config(state)?;
            let mut rows = Vec::new();
            if npu {
                rows.push(files(cfg, Slot::Npu).ok_or_else(no_npu)?);
            } else {
                rows.push(files(cfg, Slot::Main).expect("每个平台都有主模型"));
                if mmproj && cfg.litert.is_none() {
                    rows.extend(files(cfg, Slot::Mmproj));
                }
            }
            let dir = models_dir(cfg);
            let hf: Vec<Pinned> = rows.iter().flat_map(|r| r.hf.iter().map(|x| x.0)).collect();
            let qnn: Vec<&'static QnnEntry> = rows
                .iter()
                .flat_map(|r| r.qnn.iter().map(|x| x.0))
                .collect();
            let qnn_dir = rows
                .iter()
                .find_map(|r| r.qnn.first().and_then(|(_, p)| p.parent()))
                .map_or_else(|| dir.clone(), Path::to_path_buf);
            let all: Vec<(&'static ModelFile, PathBuf)> =
                rows.iter().flat_map(Files::all).collect();
            let setting = proxy::load(&*db(state)?).map_err(unavailable)?;
            let route = Route::from_setting(&setting);
            let s = state.clone();
            start_task(state, ModelTaskKind::Download, move |cancel, progress| {
                models::download(&base, &hf, &dir, &qnn, &qnn_dir, &route, cancel, progress)?;
                for (f, path) in &all {
                    set_verified(&s, f.name, Some((path, true)));
                }
                Ok("模型已就绪。".to_string())
            })
        }
        Request::ImportModels { paths } => {
            if paths.is_empty() {
                return Err(bad_request("请选择要导入的模型文件。"));
            }
            let cfg = config(state)?;
            let dir = models_dir(cfg);
            let known = known_files(cfg);
            let labels: Vec<(&'static str, &'static str)> = [Slot::Main, Slot::Mmproj, Slot::Npu]
                .into_iter()
                .filter_map(|slot| files(cfg, slot))
                .map(|f| (f.main().0.file.name, f.label))
                .collect();
            let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
            let s = state.clone();
            start_task(state, ModelTaskKind::Import, move |cancel, progress| {
                models::import(&paths, &known, &dir, cancel, progress).map(|got| {
                    for f in &got {
                        set_verified(&s, f.name, Some((&dir.join(f.name), true)));
                    }
                    let mut names: Vec<&str> = Vec::new();
                    for (name, label) in &labels {
                        if got.iter().any(|f| f.name == *name) && !names.contains(label) {
                            names.push(label);
                        }
                    }
                    if names.is_empty() {
                        return "已导入。".to_string();
                    }
                    format!("已导入{}。", names.join("和"))
                })
            })
        }
        Request::CancelModelTask => {
            lock(&state.setup.cancel).store(true, Ordering::Relaxed);
            Ok(Reply::Done)
        }
        Request::VerifyModel { mmproj, npu } => {
            let cfg = config(state)?;
            let row = files(cfg, Slot::of(mmproj, npu)).ok_or_else(no_npu)?;
            if !row.ready() {
                return Err(bad_request(format!(
                    "{}尚未安装，请先下载或导入。",
                    row.label
                )));
            }
            let label = row.label;
            let list = row.all().into_iter().map(|(f, p)| (f, p, label)).collect();
            verify_models(state, list, format!("{label}校验通过。"))
        }
        Request::VerifyModels { mmproj, npu } => {
            let cfg = config(state)?;
            let mut rows = vec![Slot::Main];
            if mmproj && cfg.litert.is_none() {
                rows.push(Slot::Mmproj);
            }
            if npu {
                rows.push(Slot::Npu);
            }
            let mut list = Vec::new();
            {
                let db = db(state)?;
                // 记为未通过的不重校，由用户重新下载或导入；换了文件时记录失效，会重新校验
                for row in rows.into_iter().filter_map(|slot| files(cfg, slot)) {
                    for (f, path) in row.all() {
                        let seen = list
                            .iter()
                            .any(|(x, _, _): &(&ModelFile, PathBuf, &str)| x.name == f.name);
                        if !seen
                            && f.check(&path).is_ok()
                            && recorded(&db, f.name, &path)?.is_none()
                        {
                            list.push((f, path, row.label));
                        }
                    }
                }
            }
            if list.is_empty() {
                return Ok(Reply::Done);
            }
            verify_models(state, list, "模型校验通过。".to_string())
        }
        Request::DeleteModel { mmproj, npu } => {
            let cfg = config(state)?;
            let row = files(cfg, Slot::of(mmproj, npu)).ok_or_else(no_npu)?;
            let label = row.label;
            // 分词器留给通用版；NPU 模型连同 QNN 运行库
            let main = row.main().clone();
            let qnn: Vec<(&'static ModelFile, PathBuf)> =
                row.qnn.iter().map(|(e, p)| (&e.file, p.clone())).collect();
            let cache = cfg.litert.as_ref().map(|l| l.cache_dir.clone());
            let s = state.clone();
            // 删除主模型后流水线取不到引擎，状态为 `no_model`，不再识别新内容（`pipeline::engine`）
            start_task(state, ModelTaskKind::Delete, move |_, progress| {
                progress(&Progress {
                    phase: ModelTaskPhase::Preparing,
                    file: main.0.file.name,
                    index: 1,
                    count: 1,
                    done: 0,
                    total: 0,
                    speed: None,
                });
                let dir = main
                    .1
                    .parent()
                    .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
                let installed = models::remove(&dir, main.0.file, label)?;
                set_verified(&s, main.0.file.name, None);
                if let Some(cache) = &cache {
                    models::remove_caches(cache, main.0.file.name);
                }
                for (f, path) in &qnn {
                    let dir = path
                        .parent()
                        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
                    models::remove(&dir, f, label)?;
                    set_verified(&s, f.name, None);
                }
                // 选了 CPU 而 CPU 要用的通用版已删除：改回自动选择（只有 NPU 模型时不能选 CPU）
                if s.preferred_device() == CPU_DEVICE
                    && s.config.get().is_some_and(|c| !c.cpu_model_ok())
                    && let Err(err) = selftest::set_preferred_device(&s, AUTO_DEVICE.to_string())
                {
                    log::warn!("无法把所选的设备改回自动选择：{}", err.message);
                }
                Ok(if installed {
                    format!("已删除{label}。")
                } else {
                    "已删除下载了一部分的文件。".to_string()
                })
            })
        }
        Request::SetProxy { setting } => match proxy::save(&*db(state)?, setting) {
            Ok(saved) => Ok(Reply::Proxy(saved)),
            Err(SaveError::Invalid(msg)) => Err(bad_request(msg)),
            Err(SaveError::Db(err)) => Err(unavailable(err)),
        },
        Request::RunSelftest { onboarding } => {
            selftest::start_selftest(state, onboarding, selftest::Mode::Manual)
        }
        Request::SetPreferredDevice { device } => selftest::set_preferred_device(state, device),
        Request::UnblockGpu { device, failed } => selftest::unblock_gpu(state, device, failed),
        Request::SetOnboardingStep { step } => {
            db(state)?
                .meta_set(ONBOARDING_STEP_KEY, &step.to_string())
                .map_err(unavailable)?;
            Ok(Reply::Done)
        }
        Request::Estimate => estimate(state).map(Reply::Estimate),
        other => unreachable!("server::respond 只把引导相关的请求交给这里：{other:?}"),
    }
}

/// 芯片没有 NPU 版模型时请求 NPU 模型的回复
fn no_npu() -> ApiError {
    bad_request("这台设备的芯片没有对应的 NPU 模型。")
}

fn status(state: &State) -> Result<SetupStatus, ApiError> {
    let cfg = config(state)?;
    let rows: Vec<Option<Files>> = [Slot::Main, Slot::Mmproj, Slot::Npu]
        .into_iter()
        .map(|slot| files(cfg, slot))
        .collect();
    let (onboarded, step, proxy, verified) = {
        let db = db(state)?;
        let onboarded = db.meta_get(ONBOARDED_KEY).map_err(unavailable)?;
        // 格式不对时从头开始
        let step = db.meta_get(ONBOARDING_STEP_KEY).map_err(unavailable)?;
        // 每一行全部文件的校验记录：都通过才算通过，有一个未通过就是未通过，否则没有记录
        let mut verified = Vec::new();
        for row in &rows {
            let mut all = Some(true);
            for (f, path) in row.as_ref().map(Files::all).unwrap_or_default() {
                all = match (all, recorded(&db, f.name, &path)?) {
                    (Some(false), _) | (_, Some(false)) => Some(false),
                    (Some(true), Some(true)) => Some(true),
                    _ => None,
                };
            }
            verified.push(all);
        }
        (
            onboarded.as_deref() == Some("1"),
            step.and_then(|s| s.parse().ok()).unwrap_or(0),
            proxy::load(&db).map_err(unavailable)?,
            verified,
        )
    };
    // 先取出任务再读自检状态：不同时持有 `task` 与 `selftest` 两把锁（顺序见模块说明）
    let task = lock(&state.setup.task).clone();
    let file = |i: usize| {
        let row = rows[i].as_ref()?;
        let (main, path) = row.main();
        let ready = row.ready();
        Some(ModelFileStatus {
            name: main.file.name.to_string(),
            bytes: row.all().iter().map(|(f, _)| f.size).sum(),
            ready,
            partial_bytes: std::fs::metadata(
                path.with_file_name(format!("{}.part", main.file.name)),
            )
            .map_or(0, |m| m.len()),
            verified: verified[i].filter(|_| ready),
            url: main.page(models::OFFICIAL),
        })
    };
    let model = file(0).expect("每个平台都有主模型");
    Ok(SetupStatus {
        onboarded,
        step,
        mmproj: file(1).unwrap_or_else(|| model.clone()),
        model,
        npu: file(2),
        tokenizer_url: cfg
            .litert
            .as_ref()
            .map(|_| Pinned::TOKENIZER.page(models::OFFICIAL)),
        npu_chip: rows[2]
            .as_ref()
            .map(|_| rummage_embed::cpu_name().to_string()),
        task,
        selftest: state.selftest_status(),
        proxy,
    })
}

/// 在后台依次完整校验 `files`（文件、路径、给用户看的名称）并记下结果，成功时的消息为 `done`；
/// 一个文件校验不通过时继续校验其余的，一次说明所有出问题的模型
fn verify_models(
    state: &Arc<State>,
    files: Vec<(&'static ModelFile, PathBuf, &'static str)>,
    done: String,
) -> Result<Reply, ApiError> {
    let s = state.clone();
    start_task(state, ModelTaskKind::Verify, move |cancel, progress| {
        let mut failed = Vec::new();
        for (i, (file, path, label)) in files.iter().enumerate() {
            let ok = models::verify(path, file, cancel, &mut |p| {
                progress(&Progress {
                    index: i as u32 + 1,
                    count: files.len() as u32,
                    ..p.clone()
                })
            })?;
            // 没记下时引导会一直等校验记录，按失败结束
            if !set_verified(&s, file.name, Some((path, ok))) {
                return Err(models::Error::Failed(
                    "无法保存校验结果，请重试。".to_string(),
                ));
            }
            if !ok {
                failed.push(*label);
            }
        }
        if !failed.is_empty() {
            return Err(models::Error::Failed(format!(
                "{}已损坏或不完整，请重新下载或导入。",
                failed.join("和")
            )));
        }
        Ok(done)
    })
}

/// 开始一个模型任务；已有任务在进行时拒绝。`work` 在专用线程中运行，成功时返回给用户看的完成说明。
/// 校验只读文件，不暂停建立索引；其他任务要替换或删除模型文件，先暂停并等推理进程退出，设备检测进行中时拒绝
fn start_task(
    state: &Arc<State>,
    kind: ModelTaskKind,
    work: impl FnOnce(&AtomicBool, &mut dyn FnMut(&Progress)) -> Result<String, models::Error>
    + Send
    + 'static,
) -> Result<Reply, ApiError> {
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let selftest = lock(&state.setup.selftest);
        let mut task = lock(&state.setup.task);
        if task
            .as_ref()
            .is_some_and(|t| t.state == ModelTaskState::Running)
        {
            return Err(bad_request(
                "模型正在下载、导入、校验或删除，请等它完成后再试。",
            ));
        }
        if kind != ModelTaskKind::Verify && selftest.running {
            return Err(bad_request("设备检测正在进行，请等它完成后再试。"));
        }
        *lock(&state.setup.cancel) = cancel.clone();
        let t = ModelTask {
            kind,
            state: ModelTaskState::Running,
            phase: ModelTaskPhase::Preparing,
            file: None,
            index: 0,
            count: 0,
            done: 0,
            total: 0,
            speed: None,
            message: None,
        };
        let _ = state.events.send(Event::ModelTask(t.clone()));
        *task = Some(t);
    }
    let s = state.clone();
    let spawned = std::thread::Builder::new()
        .name("model-task".into())
        .spawn(move || {
            // 任务期间推理暂停；先等旧引擎的推理进程退出，被它打开的模型文件才能替换或删除。等不到时照常进行，
            // 改名或删除失败时由 models 提示文件正被使用
            let pipeline = s.pipeline.get().filter(|_| kind != ModelTaskKind::Verify);
            if let Some(p) = pipeline {
                let released = p.hold(Hold::Models, true);
                if !p.wait_released(released, release_limit(&s.params())) {
                    log::warn!("等待推理引擎释放模型文件超时，继续模型任务。");
                }
            }
            let res = work(&cancel, &mut |p| {
                s.update_task(|t| {
                    t.phase = p.phase;
                    t.file = Some(p.file.to_string());
                    t.index = p.index;
                    t.count = p.count;
                    t.done = p.done;
                    t.total = p.total;
                    t.speed = p.speed;
                })
            });
            let verb = match kind {
                ModelTaskKind::Download => "下载",
                ModelTaskKind::Import => "导入",
                ModelTaskKind::Verify => "校验",
                ModelTaskKind::Delete => "删除",
            };
            if let Err(models::Error::Failed(msg)) = &res {
                log::warn!("模型{verb}失败：{msg}");
            }
            s.update_task(|t| {
                t.speed = None;
                (t.state, t.message) = match res {
                    Ok(msg) => (ModelTaskState::Done, Some(msg)),
                    Err(models::Error::Cancelled) => (
                        ModelTaskState::Cancelled,
                        Some(match kind {
                            ModelTaskKind::Download => {
                                "已取消下载，已下载的部分会保留，下次继续。".to_string()
                            }
                            _ => format!("已取消{verb}。"),
                        }),
                    ),
                    Err(models::Error::Failed(msg)) => (ModelTaskState::Failed, Some(msg)),
                };
            });
            if let Some(p) = pipeline {
                p.hold(Hold::Models, false);
            }
        });
    if let Err(err) = spawned {
        log::error!("无法启动模型任务线程：{err}");
        state.update_task(|t| {
            t.state = ModelTaskState::Failed;
            t.message = Some("无法开始任务，请重试。".into());
        });
        return Err(api_error(ApiError::INTERNAL, "无法开始任务，请重试。"));
    }
    Ok(Reply::Done)
}

/// 预估时每个文件夹抽样读取文件头的图片数：只读几 KB，几百张不到一秒
const ESTIMATE_SAMPLE: u32 = 200;

/// 按抽样的图片推算各文件夹长图切片的总段数：抽到的图片按当前参数切段，段数之和按图片总数放大
fn estimate_slices(
    sample: &[(i64, PathBuf)],
    totals: &[(i64, String, u64, u64)],
    values: &BTreeMap<String, ParamValue>,
) -> BTreeMap<i64, u64> {
    let ratio = params::float(values, "long_image_ratio");
    let max = params::int(values, "long_image_max_slices").max(0) as u32;
    let mut out = BTreeMap::new();
    for (root, path) in sample {
        // 读文件头也要先排除网盘占位文件
        if rummage_index::ensure_local(path).is_err() {
            continue;
        }
        let n = rummage_media::dimensions(path).map_or(0, |(w, h)| {
            rummage_media::slices(w, h, ratio, max).len() as u64
        });
        let e = out.entry(*root).or_insert((0u64, 0u64));
        *e = (e.0 + 1, e.1 + n);
    }
    out.into_iter()
        .map(|(root, (seen, slices))| {
            let files = totals
                .iter()
                .find(|t| t.0 == root && t.1 == "image")
                .map_or(0, |t| t.2);
            (root, slices * files / seen.max(1))
        })
        .collect()
}

fn estimate(state: &State) -> Result<Estimate, ApiError> {
    let (roots, totals, sample) = {
        let db = db(state)?;
        (
            db.list_roots().map_err(unavailable)?,
            db.kind_totals().map_err(unavailable)?,
            db.sample_images(ESTIMATE_SAMPLE).map_err(unavailable)?,
        )
    };
    let slices = estimate_slices(&sample, &totals, &state.params());
    Ok(Estimate {
        scanning: state.scan_busy(),
        free_gb: state
            .db
            .as_ref()
            .ok()
            .and_then(|c| crate::platform::free_gb(&c.dir)),
        roots: roots
            .into_iter()
            .map(|r| RootEstimate {
                scanning: state.counting(r.id),
                id: r.id,
                path: r.path,
                files: r.files.max(0) as u64,
                kinds: totals
                    .iter()
                    .filter(|t| t.0 == r.id)
                    .map(|t| {
                        (
                            t.1.clone(),
                            KindTotal {
                                files: t.2,
                                bytes: t.3,
                                slices: if t.1 == "image" {
                                    slices.get(&r.id).copied().unwrap_or(0)
                                } else {
                                    0
                                },
                            },
                        )
                    })
                    .collect(),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests;
