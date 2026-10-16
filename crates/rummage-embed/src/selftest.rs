//! 推理后端自检：用黄金样本检查每个候选后端，选出最快的通过者。
//!
//! - 候选：CPU，加上 `list_devices` 列出的每块 GPU（由调用方给出）。
//! - 样本：黄金样本中的文本、纯图像、音频与视频窗口项（`Options::images` 为否时只测文本、不加载 mmproj：用户只选了文档时
//!   不下载 mmproj）；文本按清单的 kind 拼前缀（并与清单中的 `input` 核对），
//!   图像用 `rummage_media::prepare` 按参考向量的档位缩放，音频读成 16 kHz 采样，视频窗口按清单组装（`golden`）。
//! - 每个候选：启动 `LocalBackend` 并计初始化耗时 → 全部样本算一遍，向量经数值检查（`vector::normalize`），
//!   逐条与参考向量比较，余弦须 ≥ `selftest_min_cosine` → 再算一遍计时（第一遍兼作预热），按模态取中位数。
//! - GPU 候选：整个过程限时 `gpu_selftest_timeout_s`，超时就结束推理进程并判失败；
//!   出错（含设备丢失、后端失效、崩溃）立即判失败，`LocalBackend` 的 GPU 模式本身不重试。
//!   测完等几秒再查这段时间内系统日志中的显卡驱动事件，有就判失败（`driver_events`）。
//!   驱动错误：设备丢失、推理进程崩溃或后端失效（GPU 模式下 `LocalBackend` 把超时、崩溃、推理出错都报成
//!   `Unavailable`）、总时限到期、系统日志中的驱动事件。任何一块 GPU 出现驱动错误，就停止本次剩下的所有 GPU 测试，
//!   记为“已跳过”、不屏蔽：多块卡（尤其同厂商）共用一个驱动，驱动正在重置时再初始化别的卡，可能使驱动反复超时重置、只能重启。
//!   余弦不达标、NaN 等普通失败不算驱动错误。
//!   测第一块 GPU 之前先空查一次系统日志，读不到就不测任何 GPU（检测不到驱动错误就不能安全地测）。
//! - 缓存：键为每个候选的设备指纹（描述、PCI 号，不含随枚举顺序变化的 ggml 设备名称）、驱动版本、运行库版本、模型哈希，
//!   另记下检测时系统中的显卡（`gpu_adapters`）。设备、运行库与模型相同时直接返回上次的结果，不看驱动版本（见 `cache`）。
//!   没有被中止的检测都写入缓存，暂时性失败或被跳过的显卡也写入：其他设备的结果照用，这些显卡下次检测时重测。
//! - 自动检测（`Options::auto`）只测失效的部分（`CacheFile::previous`）：同一设备（描述、PCI 号、运行库与模型都相同）上次测过、
//!   不是暂时性失败或被跳过的，且测过本次需要的模态，沿用上次的结果。所以只是显卡有增减或更换时只测新出现或
//!   更换的显卡；运行库或模型变化、新需要图片与音视频模型时所有设备（含 CPU）重测。手动检测全部重测。
//! - 失败的 GPU 按证据轻重分三级，记录与缓存同在 `cache/selftest.json`、用同一套键：
//!   1. 设备丢失、推理进程结束不了，或系统日志中有显卡驱动事件：**持久屏蔽**，键中任一项变化前都直接跳过、不再测试。
//!   2. 崩溃、超时、后端失效，但系统日志中没有驱动事件；显卡显存不足、列出后找不到：只判本次失败，不写记录，
//!      结果中标明 `transient`，下次检测时重测。
//!      崩溃或超时也可能只是显存一时不足等暂时问题，不足以断定显卡或驱动有问题。无法读取系统日志时同样处理。
//!   3. 余弦不达标、出现 NaN 等确定的结果：记为**未通过**，同样不再自动重测。
//!
//!   驱动或运行库更新后重测屏蔽过的显卡前须先征得用户同意：核心在结果过期时自动发起的检测（`Options::auto`）不测
//!   有过屏蔽记录的显卡，用户在设置中手动检测时才测。设置页的“重新检测”不清除屏蔽与未通过记录；用户在设备页
//!   “解除屏蔽”（`unblock_gpu`）才删除这块显卡的全部屏蔽记录，核心随即只测它（`Options::only`）。全部重测的手动检测中通过的显卡视为用户同意，同样删除它的屏蔽记录（未通过记录不动）；
//!   自动检测与只测一块时不这样做（`Options::manual`）。
//!   第 2 级（本次运行停用）的显卡由调用方记住（核心的 `State` 汇总自检、建索引与唤醒验证中出错的显卡），
//!   自动检测时传入 `run`，直接记为“已跳过”，同一次运行中不再自动初始化它们（用户手动检测时调用方传空，当场重测）。
//! - `Options::only`（解除屏蔽后的重测）只测这一个设备，其余设备照缓存原样显示，保存时只替换缓存中这一项。
//! - `selftest.json` 的读改写（自检保存结果、建索引时屏蔽、清除记录）都在 `CACHE_LOCK` 内进行；自检保存前重新读取
//!   文件再合并本次的记录，不会覆盖期间写入的屏蔽记录。
//! - 选择：通过的候选按图像耗时中位数排序（只测文本时按文本耗时，`by_speed`），选最快的一个，文本、图像与音频使用同一个后端。
//!   只测了文本的结果不能用来处理图片、音频或视频：`cached_gpu` 要求 mmproj 时不选它，需要重新检测。
//! - 建索引（核心进程）只用 `preferred_gpu` 读记录：用户在设置中指定、检测通过的显卡（`CandidateReport::device_id`）能用时用它，
//!   否则按性能顺序取第一个能用的设备（显卡要未被屏蔽、键中各项不变、仍在系统中、不在本次运行停用的集合中），是 CPU 时用 CPU；
//!   本次运行停用哪些显卡由核心按驱动错误的证据决定。核心不发起自检。
//!   运行中出现设备丢失、推理进程结束不了，或后端失效且这段时间系统日志中有驱动事件时，用 `blacklist_gpu` 按同一套键
//!   写入屏蔽记录。
//! - 中止（`Options::cancel`，核心在自动检测进行中进入暂停时置位）：结束正在测试的推理进程，不测其余候选，不缓存结果；
//!   中止前已测完的显卡的屏蔽与未通过记录照常保存（设备丢失须立即屏蔽）。正在测试的显卡带回设备丢失或推理进程结束不了
//!   （驱动挂死，`stuck`）的同样屏蔽：中止后报告作废，只记“本次运行停用”不会生效，流水线仍可能选回它。
//! - 黄金样本的读取与核对见 `golden`；唤醒后验证推理引擎用 `wake_sample` 只取第一条文本样本。

mod cache;
mod candidate;
mod golden;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rummage_proto::params::{self, ParamValue};
use serde::{Deserialize, Serialize};

use crate::frame::GpuInfo;
use crate::local::{Gpu, Limits, WorkerSpec};
use crate::space::{self, Backend};
use crate::{Error, KILL_GRACE, platform};

pub use cache::{
    BlockedGpu, GpuRecords, UNBLOCKED_REASON, blacklist_gpu, cache_covers, cache_current,
    cached_gpu, gpu_adapters, gpu_records, last_report, preferred_gpu, report_key, unblock_gpu,
};
use cache::{CacheFile, Cached, Record, cache_lock};
use candidate::{check_driver_events, test, test_in_order};
pub use golden::Sample;

/// GPU 测完（或运行中失效）后等系统日志写入可能的 TDR 事件再查询：TDR 默认 2 秒检测，留余量等 5 秒。
pub const TDR_LOG_WAIT: Duration = Duration::from_secs(5);
/// 查询窗口在候选耗时之外多留的余量，抵消计时起点与事件时间戳的误差。
pub const TDR_WINDOW_MARGIN: Duration = Duration::from_secs(2);
/// 结束推理进程后等测试线程返回的时间：比推理进程自己等退出的 `KILL_GRACE` 长，结束不了时测试线程先带回 `Stuck`，
/// 结果确定
const WAIT_GRACE: Duration = Duration::from_secs(KILL_GRACE.as_secs() + 5);
/// 等测试结果时检查中止标志（`Options::cancel`）的间隔
const CANCEL_POLL: Duration = Duration::from_millis(200);
/// 查不到驱动版本时的记法。此时键中的驱动版本不随驱动更新而变，被屏蔽的显卡不会因更新驱动而重测，
/// 要靠设备页的“解除屏蔽”。
const UNKNOWN_DRIVER: &str = "unknown";

pub enum Candidate {
    Cpu,
    Gpu(GpuInfo),
}

impl Candidate {
    /// 与 `CandidateReport::device_id` 相同
    pub fn device_id(&self) -> String {
        match self {
            Candidate::Cpu => rummage_proto::CPU_DEVICE.to_string(),
            Candidate::Gpu(g) => gpu_id(&g.description, g.pci_id.as_deref()),
        }
    }
}

fn gpu_id(description: &str, pci_id: Option<&str>) -> String {
    format!("gpu:{description}|{}", pci_id.unwrap_or_default())
}

/// 缓存与屏蔽的键。显卡按描述与 PCI 号识别，与推理进程选卡的依据相同（`worker`）；
/// 不含 ggml 设备名称（`Vulkan0` 等），它随枚举顺序变化，键里有它时顺序一变缓存就失效、屏蔽记录也对不上。
/// 后端由描述区分：CPU 的描述与驱动版本为空，显卡都是 Vulkan、描述不为空。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceKey {
    pub description: String,
    pub pci_id: Option<String>,
    pub driver: String,
    pub runtime: String,
    /// 主模型与 mmproj 的 SHA-256（`space` 中钉定的值）。
    pub model: String,
}

impl DeviceKey {
    /// 同一个设备、运行库与模型，不看驱动版本（缓存是否可用，见 `cache`）
    pub fn same_device(&self, other: &DeviceKey) -> bool {
        self.description == other.description
            && self.pci_id == other.pci_id
            && self.runtime == other.runtime
            && self.model == other.model
    }

    /// 与 `CandidateReport::device_id` 相同
    pub fn device_id(&self) -> String {
        if self.description.is_empty() {
            return rummage_proto::CPU_DEVICE.to_string();
        }
        gpu_id(&self.description, self.pci_id.as_deref())
    }
}

/// 键中的模型哈希：主模型与 mmproj 的 SHA-256。
fn models() -> String {
    format!("{}+{}", space::MODEL.sha256, space::MMPROJ.sha256)
}

fn key_with(c: &Candidate, driver: Option<String>) -> DeviceKey {
    let model = models();
    let runtime = space::RUNTIME.to_string();
    match c {
        Candidate::Cpu => DeviceKey {
            description: String::new(),
            pci_id: None,
            driver: String::new(),
            runtime,
            model,
        },
        Candidate::Gpu(g) => DeviceKey {
            description: g.description.clone(),
            pci_id: g.pci_id.clone(),
            driver: driver.unwrap_or_else(|| UNKNOWN_DRIVER.to_string()),
            runtime,
            model,
        },
    }
}

/// GPU 的驱动版本用 DXGI 按设备描述查询。
pub fn device_key(c: &Candidate) -> DeviceKey {
    let driver = match c {
        Candidate::Cpu => None,
        Candidate::Gpu(g) => platform::driver_version(&g.description),
    };
    key_with(c, driver)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    Text,
    Image,
    Audio,
    /// 视频窗口，带音频与纯画面两条
    Video,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Passed,
    Failed,
    /// 此前未通过且键没有变化，本次没有测试。
    Blacklisted,
    /// 另一块显卡出现驱动错误，本次没有测试；不屏蔽。
    Skipped,
}

const SESSION_DISABLED_REASON: &str =
    "这块显卡本次运行中出现错误，重新启动 Rummage 后可以再次检测。";
const SKIPPED_REASON: &str =
    "另一块显卡在检测中出现驱动错误，为避免影响系统，本次不再检测其他显卡。";
const NO_EVENT_LOG_REASON: &str = "无法读取系统日志，不能确认显卡驱动是否出错，本次不检测显卡。";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SampleResult {
    pub id: String,
    pub modality: Modality,
    pub cosine: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateReport {
    pub backend: Backend,
    /// ggml 设备名称与描述；CPU 为推理进程实际报告的值。
    pub name: String,
    pub description: String,
    pub pci_id: Option<String>,
    /// GPU 的驱动版本，查不到时为 `unknown`；CPU 为空。
    pub driver: Option<String>,
    pub status: Status,
    /// 未通过或被跳过的原因，中文整句，多条用换行分隔。
    pub reason: Option<String>,
    /// 第一遍的逐条余弦。
    pub samples: Vec<SampleResult>,
    pub init_ms: Option<f64>,
    /// 第二遍各模态单条耗时的中位数。
    pub text_ms: Option<f64>,
    pub image_ms: Option<f64>,
    pub audio_ms: Option<f64>,
    /// 视频窗口（带音频与纯画面两条的中位数）
    pub video_ms: Option<f64>,
    /// 出现了驱动错误（见模块说明）；之后的 GPU 都不再测试。
    pub driver_error: bool,
    /// 出现了设备丢失：持久屏蔽的依据之一。只在本次自检中使用，不写入记录
    #[serde(skip)]
    pub device_lost: bool,
    /// 崩溃、超时、显存不足等暂时性失败（第 2 级）：调用方在本次运行中停用它；缓存中带这一项的下次检测时重测
    pub transient: bool,
    /// 推理进程结束不了（驱动挂死）：与设备丢失一样屏蔽。只在本次自检中使用，不写入记录
    #[serde(skip)]
    pub stuck: bool,
}

impl CandidateReport {
    fn new(c: &Candidate, key: &DeviceKey) -> Self {
        let (backend, name, driver) = match c {
            Candidate::Cpu => (Backend::Cpu, "CPU".to_string(), None),
            Candidate::Gpu(g) => (Backend::Vulkan, g.name.clone(), Some(key.driver.clone())),
        };
        CandidateReport {
            backend,
            name,
            description: key.description.clone(),
            pci_id: key.pci_id.clone(),
            driver,
            status: Status::Failed,
            reason: None,
            samples: Vec::new(),
            init_ms: None,
            text_ms: None,
            image_ms: None,
            audio_ms: None,
            video_ms: None,
            driver_error: false,
            device_lost: false,
            transient: false,
            stuck: false,
        }
    }

    /// 后端返回的错误。设备丢失记为驱动错误；GPU 的崩溃、超时与后端失效也记为驱动错误，从严处理
    /// （停止测试其余 GPU），但是否持久屏蔽另看证据（`verdict`）。
    fn backend_error(&mut self, context: String, err: &Error) {
        let gpu = self.backend == Backend::Vulkan;
        self.device_lost |= matches!(err, Error::DeviceLost(_));
        self.stuck |= matches!(err, Error::Stuck(_));
        let oom = matches!(err, Error::OutOfMemory(_))
            || matches!(err, Error::Inference(msg) if msg == crate::OUT_OF_MEMORY);
        // 显卡在列出之后不见了，或显存一时被别的程序占满，与它是否正常无关：只判本次失败
        self.transient |= gpu && (matches!(err, Error::GpuNotFound(_)) || oom);
        self.driver_error |= match err {
            Error::DeviceLost(_) | Error::Stuck(_) => true,
            Error::Unavailable(_)
            | Error::Crashed(_)
            | Error::InitTimeout(_)
            | Error::InferTimeout(_) => gpu,
            _ => false,
        };
        log::warn!("{} 检测失败：{context}{err}", self.name);
        // 设备检测的原因按设备说，不用针对文件的整句
        self.fail(if oom {
            "显存或内存不足（可能被其他程序占用），本次不使用这个设备。".to_string()
        } else {
            err.user_message()
        });
    }

    fn fail(&mut self, reason: String) {
        self.status = Status::Failed;
        self.reason = Some(match self.reason.take() {
            Some(prev) => format!("{prev}\n{reason}"),
            None => reason,
        });
    }

    /// 设备标识，用户在设置中选择设备时用：CPU 为 `cpu`，显卡为 `gpu:<描述>|<PCI 号>`。
    /// 与缓存的键一样按描述与 PCI 号识别（同型号的两块卡 PCI 号不同），不含随枚举顺序变化的 ggml 设备名称
    pub fn device_id(&self) -> String {
        match self.backend {
            Backend::Cpu => rummage_proto::CPU_DEVICE.to_string(),
            Backend::Vulkan => gpu_id(&self.description, self.pci_id.as_deref()),
        }
    }

    /// 选中这个候选时传给 `WorkerSpec::gpu` 的值；CPU 为空。
    pub fn gpu(&self) -> Option<Gpu> {
        (self.backend == Backend::Vulkan).then(|| Gpu {
            description: self.description.clone(),
            pci_id: self.pci_id.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelftestReport {
    /// 与传入的候选一一对应。
    pub candidates: Vec<CandidateReport>,
    /// 选中的候选在 `candidates` 中的位置；没有可用的后端时为空。
    pub selected: Option<usize>,
    pub from_cache: bool,
}

impl SelftestReport {
    pub fn selected(&self) -> Option<&CandidateReport> {
        self.selected.map(|i| &self.candidates[i])
    }
}

/// 最近 `window` 内系统日志中的显卡驱动事件（每条一句说明）；查询失败时返回错误，不能当作没有事件
pub use platform::driver_events;

/// 测完一块 GPU 后查到的系统日志情况
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Events {
    None,
    Found,
    Unreadable,
}

/// 一块 GPU 的测试结果怎么记录（见模块说明的三级规则）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// 通过，不记录
    Passed,
    /// 持久屏蔽
    Blacklist,
    /// 只判本次失败，不写记录；本次结果照常缓存，标明 `transient`，下次检测时重测
    Transient,
    /// 记为未通过
    Failed,
}

fn verdict(r: &CandidateReport, events: Events) -> Verdict {
    if r.status != Status::Failed {
        return Verdict::Passed;
    }
    if r.device_lost || r.stuck || events == Events::Found {
        return Verdict::Blacklist;
    }
    if r.driver_error || r.transient || events == Events::Unreadable {
        return Verdict::Transient;
    }
    Verdict::Failed
}

/// 唤醒后验证推理引擎用的样本：黄金样本目录中的第一条文本样本，读取与核对方式与自检相同；不读图像。
pub fn wake_sample(golden_dir: &Path) -> Result<Sample, Error> {
    golden::first_text(golden_dir)
}

/// 自检的文件位置。
pub struct Options {
    /// rummage-core 可执行文件，推理进程以它的 `worker` 子命令启动。
    pub exe: PathBuf,
    /// 黄金样本目录（开发时为 `assets/golden`）。
    pub golden_dir: PathBuf,
    pub cpu_runtime: PathBuf,
    /// 没有 GPU 候选时可以为空。
    pub vulkan_runtime: Option<PathBuf>,
    pub model: PathBuf,
    pub mmproj: PathBuf,
    /// `<数据目录>\cache\selftest.json`。
    pub cache_file: PathBuf,
    /// 为否时不使用缓存的结果、重新测试；屏蔽记录仍然有效，结果照常写入。
    pub use_cache: bool,
    /// 为否时本次结果不写入缓存，上次的结果也丢弃（列显卡的进程结束不了、只测了 CPU 时，下次启动仍算需要检测）；
    /// 屏蔽与未通过记录照常保存
    pub save: bool,
    /// 测图像与音频样本并加载 mmproj；为否时只测文本
    pub images: bool,
    /// 核心在结果过期时自动发起的检测：只测失效的部分，其余沿用上次的结果（见模块说明）；
    /// 有过屏蔽记录的显卡即使驱动或运行库已更新也不测
    pub auto: bool,
    /// 置位时中止检测：结束正在测试的推理进程，不再测其余候选，不缓存结果（已测完的显卡的记录照常保存），`run` 返回 `Error::Stopped`
    pub cancel: Arc<AtomicBool>,
    /// 只测这个设备（`CandidateReport::device_id`，解除屏蔽后的重测）：其余设备照缓存中的结果原样显示（没有时为未检测），
    /// 不检查也不测试；保存时只替换缓存中这一项，其余照旧（缓存为空时整份写入，其余为未检测、下次检测时再测）。
    /// 系统中没有这个设备时返回 `Error::GpuNotFound`。不保存时（`save` 为否，列显卡的进程卡住）与手动检测一样整份缓存丢弃
    pub only: Option<String>,
}

impl Options {
    /// 全部重测的手动检测：其中通过的显卡视为用户同意解除屏蔽。自动检测与只测一块（`only`）时不算，
    /// 免得顺带解除缓存中通过、运行中却被屏蔽的其他显卡
    fn manual(&self) -> bool {
        !self.auto && self.only.is_none()
    }
}

/// 运行自检。只有黄金样本无效（`Error::Golden`）或被中止（`Error::Stopped`）时返回错误；后端失败都写在报告中。
/// `disabled` 为本次运行中停用的显卡（见模块说明），直接记为“已跳过”、不初始化。
/// `progress` 在开始测试每个候选前以它的序号和 `None` 调用，测完后再以它的结果调用，界面据此逐台显示
/// （用缓存的结果时不调用；因另一块显卡出错而跳过的不调用，结果在报告中）。
pub fn run(
    opts: &Options,
    candidates: &[Candidate],
    values: &BTreeMap<String, ParamValue>,
    disabled: &[DeviceKey],
    progress: &mut dyn FnMut(usize, Option<&CandidateReport>),
) -> Result<SelftestReport, Error> {
    let keys: Vec<DeviceKey> = candidates.iter().map(device_key).collect();
    let adapters = cache::gpu_adapters();
    let cache = {
        let _guard = cache_lock();
        CacheFile::load(&opts.cache_file)
    };
    if opts.use_cache
        && let Some(mut report) = cache.cached_report(&keys, opts.images)
    {
        report.from_cache = true;
        return Ok(report);
    }
    let cancelled = || opts.cancel.load(Ordering::Relaxed);
    if cancelled() {
        return Err(Error::Stopped);
    }

    let max_pixels = params::max_image_pixels(values);
    let mut golden = golden::load(&opts.golden_dir, max_pixels)?;
    if !opts.images {
        golden.samples.retain(|s| s.modality == Modality::Text);
    }
    let golden = Arc::new(golden);
    let min_cos = params::float(values, "selftest_min_cosine");
    let total = Duration::from_secs(params::int(values, "gpu_selftest_timeout_s") as u64);
    let threads = params::int(values, "index_threads") as u32;
    let limits = Limits::from_params(values);
    let spec = |runtime_dir: &Path, gpu| WorkerSpec {
        exe: opts.exe.clone(),
        runtime_dir: runtime_dir.to_path_buf(),
        model: opts.model.clone(),
        mmproj: opts.images.then(|| opts.mmproj.clone()),
        threads,
        gpu,
    };

    let mut reports: Vec<CandidateReport> = candidates
        .iter()
        .zip(&keys)
        .map(|(c, key)| CandidateReport::new(c, key))
        .collect();
    // 自动检测沿用上次仍然有效的结果，只测新出现、更换或结果已失效的设备（见模块说明）
    // 只测指定设备时，其余设备照缓存原样显示
    let other = |k: &DeviceKey| opts.only.as_ref().is_some_and(|o| *o != k.device_id());
    if let Some(only) = &opts.only
        && !keys.iter().any(|k| k.device_id() == *only)
    {
        return Err(Error::GpuNotFound(only.clone()));
    }
    let previous: Vec<Option<CandidateReport>> = keys
        .iter()
        .map(|k| match &opts.only {
            Some(_) => other(k).then(|| cache.shown(k)).flatten(),
            None => cache.previous(k, opts.images).filter(|_| opts.auto),
        })
        .collect();
    let reused: Vec<bool> = keys
        .iter()
        .zip(&previous)
        .map(|(k, p)| p.is_some() || other(k))
        .collect();
    // 读不到系统日志就检测不到驱动错误，不测任何 GPU（沿用上次结果的不用测）。
    let needs_log = candidates
        .iter()
        .zip(&reused)
        .any(|(c, reused)| matches!(c, Candidate::Gpu(_)) && !reused);
    let event_log = if needs_log {
        platform::driver_events(Duration::from_secs(1))
            .map(|_| ())
            .map_err(|err| log::warn!("无法读取系统日志：{err}"))
    } else {
        Ok(())
    };
    // 本次新写的屏蔽与未通过记录，保存前合并进重新读取的文件
    let mut records: Vec<(DeviceKey, Record, String)> = Vec::new();
    let mut test_one = |i: usize, r: &mut CandidateReport| {
        if cancelled() {
            return;
        }
        if other(&keys[i]) {
            match &previous[i] {
                Some(prev) => *r = prev.clone(),
                None => r.status = Status::Skipped,
            }
            return;
        }
        let gpu = matches!(candidates[i], Candidate::Gpu(_));
        if gpu && skip_gpu(r, &keys[i], &cache, disabled, opts.auto) {
            return;
        }
        if let Some(prev) = &previous[i] {
            // 显卡的驱动版本按现在的记（运行中屏蔽、本次运行停用都按它的键）；CPU 的名称是推理进程报告的处理器型号，沿用
            let fresh = std::mem::replace(r, prev.clone());
            if gpu {
                r.name = fresh.name;
                r.driver = fresh.driver;
            }
            return;
        }
        let Candidate::Gpu(g) = &candidates[i] else {
            let spec = spec(&opts.cpu_runtime, None);
            return test(r, spec, &limits, &golden, min_cos, None, &opts.cancel);
        };
        if event_log.is_err() {
            r.status = Status::Skipped;
            r.reason = Some(NO_EVENT_LOG_REASON.to_string());
            return;
        }
        let Some(runtime) = &opts.vulkan_runtime else {
            return r
                .fail("缺少显卡计算所需的组件，未检测这块显卡。请重新安装 Rummage。".to_string());
        };
        let gpu = Gpu {
            description: g.description.clone(),
            pci_id: g.pci_id.clone(),
        };
        let start = Instant::now();
        test(
            r,
            spec(runtime, Some(gpu)),
            &limits,
            &golden,
            min_cos,
            Some(total),
            &opts.cancel,
        );
        // 中止时推理进程是被结束的，结果不能说明显卡的好坏；结束不了（驱动挂死）时屏蔽，见模块说明
        if cancelled() {
            if r.stuck || r.device_lost {
                let reason = r.reason.clone().unwrap_or_default();
                records.push((keys[i].clone(), Record::Blacklisted, reason));
            }
            return;
        }
        let events = check_driver_events(r, start);
        let reason = r.reason.clone().unwrap_or_default();
        match verdict(r, events) {
            Verdict::Passed => {}
            Verdict::Blacklist => records.push((keys[i].clone(), Record::Blacklisted, reason)),
            Verdict::Failed => records.push((keys[i].clone(), Record::Failed, reason)),
            Verdict::Transient => r.transient = true,
        }
    };
    test_in_order(&mut reports, &reused, |i, r| {
        // 沿用上次结果的不测，不显示“检测中”
        if !reused[i] {
            progress(i, None);
        }
        test_one(i, r);
        progress(i, Some(r));
    });
    if cancelled() {
        // 中止前已测完的显卡的屏蔽与未通过记录照常保存（设备丢失须立即屏蔽），本次结果不缓存
        if !records.is_empty() {
            let _guard = cache_lock();
            let mut cache = CacheFile::load(&opts.cache_file);
            for (key, kind, reason) in records {
                cache.record(key, kind, &reason);
            }
            if let Err(err) = cache.save(&opts.cache_file) {
                log::warn!("无法写入自检记录 {}：{err}", opts.cache_file.display());
            }
        }
        return Err(Error::Stopped);
    }

    let selected = select(&reports, opts.images);
    let report = SelftestReport {
        candidates: reports,
        selected,
        from_cache: false,
    };
    // 保存前重新读取，合并期间别处写入的屏蔽记录
    let _guard = cache_lock();
    let mut cache = CacheFile::load(&opts.cache_file);
    cache.merge(records, &report, opts.manual());
    // 暂时性失败或被跳过的显卡也照常缓存：其他设备的结果照用，这些显卡下次检测时重测（`CacheFile::current`、`previous`）。
    // 不保存时（`Options::save`）上次的结果也丢弃：显卡可能已不可靠，下次启动重新检测
    let only = opts
        .only
        .as_ref()
        .and_then(|o| keys.iter().position(|k| k.device_id() == *o));
    match only {
        Some(i) if opts.save && cache.cached.is_some() => {
            cache.replace(keys[i].clone(), report.candidates[i].clone(), opts.images)
        }
        _ => {
            cache.cached = opts.save.then(|| Cached {
                keys,
                report: report.clone(),
                mmproj: opts.images,
                adapters,
            })
        }
    }
    if let Err(err) = cache.save(&opts.cache_file) {
        log::warn!("无法写入自检记录 {}：{err}", opts.cache_file.display());
    }
    Ok(report)
}

/// GPU 测试前的检查：本次运行中停用、有屏蔽或未通过记录时写好状态与原因并返回是，不测试也不沿用上次的结果。
/// `auto`（自动重新检测）时，同一块显卡有过屏蔽记录也跳过，即使驱动或运行库已更新
fn skip_gpu(
    r: &mut CandidateReport,
    key: &DeviceKey,
    cache: &CacheFile,
    disabled: &[DeviceKey],
    auto: bool,
) -> bool {
    // 同一设备即算，不看驱动版本（拔出时查不到驱动，见 `cache` 模块说明）
    if disabled.iter().any(|d| d.same_device(key)) {
        r.status = Status::Skipped;
        r.reason = Some(SESSION_DISABLED_REASON.to_string());
        return true;
    }
    let Some(b) = cache.blacklisted(key) else {
        if !(auto && cache.ever_blacklisted(key)) {
            return false;
        }
        r.status = Status::Blacklisted;
        return true;
    };
    log::info!("{} 此前有屏蔽或未通过记录：{}", r.name, b.reason);
    // 已屏蔽的不另写原因：界面上只列在“已屏蔽的显卡”中，显示屏蔽记录本身的原因
    (r.status, r.reason) = match b.kind {
        Record::Blacklisted => (Status::Blacklisted, None),
        Record::Failed => (
            Status::Failed,
            Some(
                "这块显卡此前检测时结果不达标，更新显卡驱动或 Rummage 前不再自动检测。".to_string(),
            ),
        ),
    };
    true
}

/// 通过的候选中图像耗时中位数最小的一个；`images` 为否（只测了文本）时按文本耗时。
fn select(reports: &[CandidateReport], images: bool) -> Option<usize> {
    by_speed(reports, images).first().copied()
}

/// 通过的候选按性能排序（与 `select` 的指标相同），最快的在前；没有这项耗时的排在最后
fn by_speed(reports: &[CandidateReport], images: bool) -> Vec<usize> {
    let ms = |r: &CandidateReport| if images { r.image_ms } else { r.text_ms };
    let mut v: Vec<usize> = (0..reports.len())
        .filter(|&i| reports[i].status == Status::Passed)
        .collect();
    v.sort_by(|&a, &b| {
        let (x, y) = (ms(&reports[a]), ms(&reports[b]));
        x.is_none()
            .cmp(&y.is_none())
            .then_with(|| x.unwrap_or(0.0).total_cmp(&y.unwrap_or(0.0)))
    });
    v
}

#[cfg(test)]
mod tests;
