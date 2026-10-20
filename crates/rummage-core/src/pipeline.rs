//! 索引流水线：把 `jobs` 中的作业变成片段与向量。
//!
//! 线程：
//! - 调度线程：按优先级取一个 `pending` 作业（`Db::claim_job`，置为 `preprocessing`），交给空闲的预处理线程。
//!   没有作业时等待唤醒（扫描结束、`AddRoot`、有作业被放回），另每 60 秒兜底检查一次，不轮询数据库。
//! - 预处理线程 `index_threads` 个（`prep`）：每个文件放在单独的任务线程中处理并限时，超时就放弃等待该线程，
//!   panic 由 `catch_unwind` 捕获，都计一次失败；结果经长度为 `index_threads` 的有界队列交给推理线程。
//! - 推理线程：作业置为 `embedding`，逐个片段调用 `embed`；内容相同的图片、音频、视频只推理一次；
//!   带音频的视频第一个窗口失败或超过 token 上限时整段改为纯画面（`embed`）。
//! - 写库线程（`write`）：每 32 个文件或每 500 毫秒提交一次，提交后同步内存向量索引，最多每 500 毫秒广播一次 `IndexProgress`，
//!   并记下作业完成的时刻，用于估算剩余时间（`eta_s`）。
//!
//! 查询（`query_engine`、`embed_query`）与索引共用同一个推理引擎，以 `Priority::Query` 插队。
//!
//! 后端：按用户选择的设备（`meta` 表的 `preferred_device`）：没有选择时，按自检记录的性能顺序用第一个能用的设备
//! （显卡要未被屏蔽、仍在系统中、本次运行中没有出过问题，`selftest::preferred_gpu`）；选了 CPU 时只用 CPU；选了某块显卡时，
//! 它在自检记录中通过检测、未被屏蔽、仍在系统中、本次运行中没有出过问题就用它，否则按没有选择处理。
//! 自检由引导或设置发起（`server::setup`）：检测期间 `hold` 让调度线程不领取作业并丢弃当前引擎，
//! `wait_released` 等处理中的作业结束、旧引擎的推理进程退出后才开始检测；检测完成后 `reload` 按新记录重建引擎，不需要重启。
//! 结果过期时核心在后台自动检测：期间不 `hold`，改为 `cpu_only` 暂时只用 CPU、照常建索引，只等用 GPU 的旧引擎释放。
//! 引导未完成时（`meta` 表的 `onboarded` 为 `0`）同样 `hold`，扫描与文件名索引照常进行。
//! `hold` 的设置与 `engine` 中的检查都在引擎槽的锁内，不会在设置之后又建出新引擎。
//! 本次运行中显卡出的问题记在 `Shared::disabled`（运行中失效、唤醒验证失败）：停用的显卡选设备、重新检测时都跳过；
//! 无法排除驱动原因时所有显卡都停用、只用 CPU，本次运行也不再自动检测设备。`reload` 不清除它们；
//! 用户手动检测或解除屏蔽后的重测通过时由 `server::setup` 清除（用户主动的操作不算自动重试）。
//! 是否处理图片、音频、视频以“mmproj 可用且参数 `index_images` / `index_audio` / `index_video` 开启”为准，修改后由核心调用
//! `reload`；有一个开启就加载 mmproj。
//! 运行中推理后端失效时，正在推理的作业计一次失败，已预处理、还绑定旧引擎的作业放回 `pending` 不计失败，下一个作业起换设备：
//! 找不到显卡（已拔出）、显存不足、显卡算出无效结果（NaN 等）这类明确不是驱动问题的，换剩下的设备中最快的；其余一律只用 CPU
//! （驱动证据门槛，`engine`）。显卡算出 NaN 时正在推理的作业也放回、不计失败（不是文件的问题）；CPU 上算出 NaN 按文件失败处理。
//! CPU 的推理进程结束不了时本次运行不再识别内容（显卡的按驱动问题改用 CPU）。持久屏蔽见 `Shared::engine_failed`。查询遇到这些错误时换用新建的引擎再算一次
//! （`embed_query`），搜索不因此退回只匹配文件名。
//! 推理引擎在取第一个作业前创建，视觉 token 档位或 mmproj 的有无变化时重建。
//! 主模型缺失时不取作业，状态为 `no_model`；只缺 mmproj 时只处理文本类，图片、音频与视频作业留在 `pending`，处理完后状态为
//! `no_mmproj`；推理后端创建失败时状态为 `error` 并附原因。
//! 放弃等待的任务线程（超时）达到 `index_threads` 个时，预处理线程暂停领取新作业，直到它们退出（`prep`）。
//! 领取作业与退出时放回作业都在数据库锁内，并在锁内检查停止标志：放回之后不会再领出新作业。
//! 停止：`stop` 让各线程在手头的文件处理完后退出，并把处理中的作业放回 `pending`（不计失败）；
//! 核心崩溃时没有这一步，下次启动由 `Db::recover_jobs` 计一次失败。
//! 降温：`set_slow` 打开时调度线程一次只处理一个作业，完成后休息与它处理时长相同的时间再领取下一个（`schedule`）。
//! 暂停：`set_paused` 给出原因（`resource`）时调度线程不再领取新作业，并丢弃推理引擎，推理进程随正在推理的片段算完而退出，
//! 不再占用显卡与内存；原因变化时立即广播一次 `IndexProgress`。系统睡眠与唤醒验证期间同样不领取新作业，报为
//! `sleeping`（`wake`）。查询不受暂停影响，暂停期间需要时新建推理引擎，空闲后按 `engine_idle_min` 卸载；只有唤醒验证
//! 进行中时查询先等它结束。
//! 引擎从槽中丢弃（暂停、设备检测、选择的设备或参数改变，`Shared::retire`）后，还绑定它的文件算完手头的片段就放回
//! `pending`、不计失败，由新引擎重算：检测完成或换设备后立即用新设备，不等已领取的文件在旧设备上算完。
//! 进度广播（写库线程的与状态变化时立即发的）都在 `paused` 锁内读取状态并发出，旧状态不会晚于新状态。
//! 空闲卸载的时限（`engine_idle_min`）修改后由 `apply_engine_idle` 交给当前引擎，新建的引擎从参数中读取。
//! 分数校准：校准线程（`calib`）每 60 秒检查一次，有“空间 × 模态”需要时在后台计算背景分布。
//! 设置与索引绑定：启动时与影响索引的参数修改后，只把向量的参数版本与当前设置不符的已完成文件放回重建，
//! 重建排在新文件之后，见 `rebuild`。
//! 文档的内嵌图片、音频、视频在文档的预处理中取出，按各自的模态建立片段，见 `docmedia`；文档作业可只算正文或只算内嵌媒体
//! （`Claimed::parts`），写入时只替换这一部分的片段。
mod calib;
mod docmedia;
mod embed;
mod engine;
mod hold;
mod options;
mod prep;
mod rebuild;
mod schedule;
mod wake;
mod write;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use rummage_embed::space;
use rummage_embed::{
    EmbeddingBackend, Error as EmbedError, Input, Priority, SpaceConfig, selftest,
};
use rummage_index::{Claimed, Db, DbError, Outcome, Space, VIndex};
use rummage_media::video::WindowOpts;
use rummage_proto::{
    AUTO_DEVICE, Event, IndexCounts, IndexState, IndexStatus, ParamValue, PauseReason, params,
};
use tokio::sync::broadcast;

use crate::lock;

pub(crate) use docmedia::part_ext;
pub use hold::Hold;
use hold::Released;
pub use options::PREFERRED_DEVICE_KEY;
pub use options::{Config, Disabled, DisabledGpus, DriverEvents, EngineSpec, Factory, Options};
pub(crate) use options::{ONBOARDED_KEY, ONBOARDING_STEP_KEY};
pub(crate) use prep::reason::label;
pub(crate) use rebuild::void_kinds;
// 参数表中的值；缺少或不合法时用默认值
pub(crate) use rummage_proto::params::{float, int, value as param};
use schedule::schedule;

// 粗排向量由 rummage-embed 截断，rummage-index 按自己的维数存储，两边必须一致
const _: () = assert!(rummage_embed::vector::COARSE_DIM == rummage_index::vindex::COARSE_DIM);

/// 没有作业时兜底检查的间隔
const IDLE_CHECK: Duration = Duration::from_secs(60);
/// 推理线程记住的最近图片、音频、视频片段向量数（长图、音频、视频每段一项），用于同时处理的相同文件（还没写入数据库）
const RECENT_IMAGES: usize = 1024;
/// 视频预设（参数 `video_preset`）：(值, 窗口秒数, 每窗口帧数, 带音频)，都不重叠；值 0 为“自定义”，用高级项
const VIDEO_PRESETS: [(i64, u32, u32, bool); 3] =
    [(1, 2, 2, false), (2, 4, 4, false), (3, 6, 6, true)];

/// 设备检测测速用的视频设置（`selftest::VideoSpeed`），与建索引相同
pub fn speed_video(values: &BTreeMap<String, ParamValue>) -> selftest::VideoSpeed {
    let v = video_opts(values);
    selftest::VideoSpeed {
        window_s: v.windows.window_s,
        frames: v.windows.frames,
        audio: v.audio_tokens.is_some(),
    }
}

/// 实际使用的视频设置：预设，或“自定义”时的高级项
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoOpts {
    pub windows: WindowOpts,
    /// 每帧的视觉 token 档位（`video_frame_tokens`）
    pub frame_tokens: u32,
    /// 带音频时的 token 上限（`video_max_tokens`）；纯画面不设上限，窗口最多 10 帧 140 档约 1400 token，在推理上下文以内
    pub audio_tokens: Option<usize>,
}

pub fn video_opts(values: &BTreeMap<String, ParamValue>) -> VideoOpts {
    let preset = int(values, "video_preset");
    let (window_s, overlap_s, frames, audio) = match VIDEO_PRESETS.iter().find(|p| p.0 == preset) {
        Some(p) => (p.1, 0, p.2, p.3),
        None => {
            let window_s = int(values, "video_window_s").max(1) as u32;
            (
                window_s,
                rummage_media::audio::effective_overlap(
                    window_s,
                    int(values, "video_overlap_s").max(0) as u32,
                ),
                int(values, "video_frames").max(1) as u32,
                params::flag(values, "video_audio"),
            )
        }
    };
    VideoOpts {
        windows: WindowOpts {
            window_s,
            overlap_s,
            frames,
        },
        frame_tokens: int(values, "video_frame_tokens") as u32,
        audio_tokens: audio.then(|| int(values, "video_max_tokens").max(1) as usize),
    }
}

/// 一个推理后端及其文本空间。图片、长图切片与视频帧的档位不属于引擎：推理进程接受任意档位的输入（mtmd 的视觉 token
/// 上限取模型允许的最大值），各输入按领取作业时的参数（`Task::params`）缩放，向量空间也按它算出
struct Engine {
    /// 不得克隆到 Engine 之外，否则 wait_released 失效
    backend: Arc<dyn EmbeddingBackend>,
    /// 用 GPU 时为自检记录中选中的那一项，设备丢失时据此屏蔽
    gpu: Option<selftest::CandidateReport>,
    /// 状态中显示的设备名称
    device: String,
    /// 处理图片（mmproj 可用且 `index_images` 开启）
    images: bool,
    /// 处理音频（mmproj 可用且 `index_audio` 开启）
    audio: bool,
    /// 处理视频（mmproj 可用且 `index_video` 开启）
    video: bool,
    /// 图片、音频与视频的空间按档位、切段与窗口参数由它算出
    cfg: SpaceConfig,
    text_space: Space,
    /// 出现过设备丢失或后端失效；还绑定它的已预处理作业放回 `pending`
    failed: AtomicBool,
    /// 出现过驱动类错误（已停用所有显卡并下过结论）
    driver_failed: AtomicBool,
    /// 已从引擎槽丢弃（暂停、设备检测、换设备、参数变化）：还绑定它、没算完的文件放回 `pending`，改用新引擎
    retired: AtomicBool,
    /// 放在最后：字段按声明顺序释放，它释放时推理后端（及其推理进程）已经结束
    released: Released,
}

/// 一次推理请求开始时的时刻与挂起代数，GPU 失效时据此判断证据（`Shared::engine_failed`）
#[derive(Clone, Copy)]
struct Start {
    /// 从这里起查系统日志中的驱动事件
    at: Instant,
    /// 请求开始（索引作业为领取）时的挂起代数（`wake`）
    generation: u64,
}

/// 写库线程收到的消息；每条只经通道传递一次，不必为大小装箱
#[allow(clippy::large_enum_variant)]
enum Msg {
    Outcome(Claimed, Outcome),
    /// 状态变了（如变为空闲），请广播进度
    Progress,
}

#[derive(Clone)]
struct Task {
    job: Claimed,
    engine: Arc<Engine>,
    /// 领取时的挂起代数
    generation: u64,
    /// 取作业时的参数
    params: BTreeMap<String, ParamValue>,
}

struct Item {
    locator: String,
    text: Option<String>,
    input: Input,
    /// 长图切片、文本片段自己的空间与参数版本，见 `rummage_index::NewSegment::space`
    space: Option<Box<(Space, String)>>,
}

/// 预处理完成、等待推理的文件
struct Prepared {
    job: Claimed,
    engine: Arc<Engine>,
    /// 领取时的挂起代数
    generation: u64,
    /// `image`、`document`、`audio` 或 `video`
    kind: &'static str,
    /// 片段向量所在的空间（长图切片与文本片段另见 `Item::space`）
    space: Space,
    /// 带音频的视频：失败时整段改用的纯画面空间；其他为空
    fallback: Option<Space>,
    quick_hash: Vec<u8>,
    content_hash: Option<Vec<u8>>,
    items: Vec<Item>,
    /// 图片解码后的宽高、视频转正后的宽高
    size: Option<(u32, u32)>,
    /// 音频、视频的元数据
    meta_json: Option<String>,
    /// 音频采样、视频帧占用的内存额度，推理完成后随它一起释放（`prep::MediaMem`）
    _mem: Option<prep::MediaLease>,
    /// 文档的内嵌媒体，每个媒体单独推理，失败只跳过它自己
    media: Vec<docmedia::Media>,
    /// 只算了文档的一部分时要替换的模态（`Outcome::Stored::replace`）；为空时替换全部片段
    replace: Option<Vec<&'static str>>,
    /// 文档的内嵌媒体还没处理完（`Outcome::Stored::more`）
    more: bool,
    /// 文档的内嵌媒体中这一批无法识别的个数与是否接着上一批（`Outcome::Stored::media_failed`）；没算内嵌媒体时为空
    media_failed: Option<(u32, bool)>,
}

struct Shared {
    db: Arc<Mutex<Db>>,
    /// 写库线程提交后同步
    index: Arc<Mutex<VIndex>>,
    params: Arc<Mutex<BTreeMap<String, ParamValue>>>,
    events: broadcast::Sender<Event>,
    opts: Options,
    /// 运行中失效、唤醒验证失败的 GPU 记入这里，与核心的设备检测共用（`State::disabled`）
    disabled: DisabledGpus,
    engine: Mutex<Option<Arc<Engine>>>,
    /// 推理进程结束不了时给用户看的原因：本次运行不再建任何引擎（`engine`）
    dead: Mutex<Option<String>>,
    /// 后台自动检测设备进行中，暂时只用 CPU、照常建索引（`Pipeline::cpu_only`）。只在引擎槽的锁内修改
    cpu_only: AtomicBool,
    /// 用户选择的设备：`auto`、`cpu` 或显卡的标识（`PREFERRED_DEVICE_KEY`）
    preferred: Mutex<String>,
    /// 引导未完成、设备自检进行中：不领取作业、不建引擎。只在引擎槽的锁内修改
    held_setup: AtomicBool,
    held_selftest: AtomicBool,
    /// 模型下载或导入进行中
    held_models: AtomicBool,
    /// 清空索引进行中
    held_clear: AtomicBool,
    /// 最近一次丢弃（`hold`、`reload`、`drop_engine`）的引擎的释放标志，`wait_released` 一并等它
    last_released: Mutex<Option<Arc<AtomicBool>>>,
    no_model: AtomicBool,
    /// 当前引擎没有加载 mmproj
    no_mmproj: AtomicBool,
    /// 推理后端创建失败的原因
    error: Mutex<Option<String>>,
    /// 已放弃等待、还在运行的任务线程（`prep`）
    stuck: Arc<prep::Stuck>,
    /// 已解码、等待推理的音频采样与视频帧占用的内存（`prep`）
    media_mem: Arc<prep::MediaMem>,
    /// 调度线程上次取到了作业
    busy: AtomicBool,
    /// 已取出、还没有最终结果写入的作业数
    inflight: AtomicUsize,
    woken: Mutex<bool>,
    wake_cv: Condvar,
    stop: AtomicBool,
    /// 降温：一次只处理一个作业，完成后休息同样长的时间（`schedule`）
    slow: AtomicBool,
    /// 暂停领取新作业的原因（不含睡眠）。广播进度时在这把锁内读取状态，事件顺序与修改顺序一致
    paused: Mutex<Option<PauseReason>>,
    /// 睡眠与唤醒验证的状态（`wake`）
    sleep: Mutex<wake::Sleep>,
    /// `sleep` 中的验证结束时通知等待的查询
    sleep_cv: Condvar,
    /// 唤醒验证串行执行
    verify: Mutex<()>,
    /// 最近完成的作业，估算剩余时间用（`write`）
    recent: Mutex<write::Recent>,
    /// 影响索引的参数修改时正在处理的作业（`Pipeline::requeue`）：它们按旧参数计算，结果写入时改为放回 `pending`。
    /// 只在数据库锁内读写
    stale: Mutex<Vec<i64>>,
    /// 这一批要重建的文件数（`rebuild`）
    rebuild_total: Mutex<u64>,
    /// 背景分布的计算串行执行（`calib`）
    calib_lock: Mutex<()>,
    /// 停止时置真并唤醒校准线程
    calib_stop: Mutex<bool>,
    calib_cv: Condvar,
}

/// 向量空间的数据库行：`image_tier` 为空时是文本空间，否则是该视觉 token 档位的图像空间
pub fn space_row(cfg: &SpaceConfig, image_tier: Option<u32>) -> Space {
    match image_tier {
        Some(tier) => row(cfg, cfg.image(tier), true, cfg.image_version(tier)),
        None => row(cfg, cfg.text(), false, space::TEXT_PREPROCESS.to_string()),
    }
}

/// 音频或视频空间的数据库行；`preprocess` 为 `space::audio_preprocess` / `space::video_preprocess` 给出的版本
pub fn media_space_row(cfg: &SpaceConfig, preprocess: &str) -> Space {
    row(cfg, cfg.media(preprocess), true, preprocess.to_string())
}

fn row(cfg: &SpaceConfig, id: space::SpaceId, mmproj: bool, preprocess: String) -> Space {
    Space {
        id: id.0,
        family: cfg.family.clone(),
        model_sha256: cfg.model_sha256.clone(),
        mmproj_sha256: mmproj.then(|| cfg.mmproj_sha256.clone()),
        runtime: cfg.runtime.clone(),
        backend: cfg.backend.as_str().to_string(),
        precision: cfg.precision.clone(),
        prefix_scheme: cfg.prefix_scheme.clone(),
        preprocess,
    }
}

impl Shared {
    fn stop_calib(&self) {
        *lock(&self.calib_stop) = true;
        self.calib_cv.notify_all();
    }

    fn wake(&self) {
        *lock(&self.woken) = true;
        self.wake_cv.notify_all();
    }

    fn wait(&self) {
        let mut woken = lock(&self.woken);
        if !*woken && !self.stop.load(Ordering::Relaxed) {
            woken = self
                .wake_cv
                .wait_timeout(woken, IDLE_CHECK)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        *woken = false;
    }

    /// 设置状态标志，有变化时请写库线程广播进度
    fn set_flag(&self, flag: &AtomicBool, value: bool, out: &Sender<Msg>) {
        if flag.swap(value, Ordering::Relaxed) != value {
            let _ = out.send(Msg::Progress);
        }
    }

    fn set_error(&self, error: Option<String>, out: &Sender<Msg>) {
        let mut cur = lock(&self.error);
        if *cur != error {
            *cur = error;
            let _ = out.send(Msg::Progress);
        }
    }

    /// 报告的暂停原因：手动 > 睡眠 > 其他（`wake`）
    fn reported(&self, paused: Option<PauseReason>) -> Option<PauseReason> {
        match paused {
            Some(m @ PauseReason::Manual { .. }) => Some(m),
            _ if lock(&self.sleep).held() => Some(PauseReason::Sleeping),
            other => other,
        }
    }

    fn status(&self) -> Result<IndexStatus, DbError> {
        let paused = *lock(&self.paused);
        self.status_with(paused)
    }

    /// `paused` 为调用方在 `paused` 锁内读出的值
    fn status_with(&self, paused: Option<PauseReason>) -> Result<IndexStatus, DbError> {
        let counts = lock(&self.db).job_counts()?;
        let error = lock(&self.error).clone();
        let media_pending = ["image", "audio", "video"]
            .iter()
            .any(|k| counts.get(*k).is_some_and(|c| c.pending > 0));
        let state = if self.no_model.load(Ordering::Relaxed) {
            IndexState::NoModel
        } else if error.is_some() {
            IndexState::Error
        } else if self.busy.load(Ordering::Relaxed) || self.inflight.load(Ordering::Relaxed) > 0 {
            IndexState::Running
        } else if media_pending && self.no_mmproj.load(Ordering::Relaxed) {
            IndexState::NoMmproj
        } else {
            IndexState::Idle
        };
        let mut s = IndexStatus {
            state,
            paused: self.reported(paused),
            error,
            total: 0,
            done: 0,
            failed: 0,
            pending: 0,
            by_kind: BTreeMap::new(),
            backend: lock(&self.engine).as_ref().map(|e| e.device.clone()),
            eta_s: None,
            rebuild: self.rebuild_status()?,
        };
        for (kind, c) in counts {
            s.total += c.total;
            s.done += c.done;
            s.failed += c.failed;
            s.pending += c.pending;
            s.by_kind.insert(
                kind,
                IndexCounts {
                    total: c.total,
                    done: c.done,
                    failed: c.failed,
                    pending: c.pending,
                },
            );
        }
        if s.state == IndexState::Running && s.paused.is_none() {
            // 剩余作业只算调度线程能领取的（离线、云端、缺 mmproj 时的图片、音频与视频不算），不用 `pending`
            let (images, audio, video) = lock(&self.engine)
                .as_ref()
                .map_or((false, false, false), |e| (e.images, e.audio, e.video));
            let remaining = lock(&self.db).claimable_jobs(images, audio, video)?;
            s.eta_s = lock(&self.recent).eta(Instant::now(), remaining);
        }
        Ok(s)
    }

    /// 广播一次进度（写库线程的节流广播、暂停与睡眠状态变化时的立即广播都走这里）。在 `paused` 锁内读取状态并发出，
    /// 旧状态不会晚于新状态发出
    fn broadcast(&self) {
        let paused = lock(&self.paused);
        self.send_status(*paused);
    }

    fn send_status(&self, paused: Option<PauseReason>) {
        match self.status_with(paused) {
            // 没有订阅者时 send 返回错误，忽略
            Ok(s) => {
                let _ = self.events.send(Event::IndexProgress(s));
            }
            Err(err) => log::warn!("无法统计索引进度：{err}"),
        }
    }
}

pub struct Pipeline {
    shared: Arc<Shared>,
    /// 查询时新建引擎用（`Shared::engine`）
    out: Sender<Msg>,
}

/// 查询用的推理引擎
pub struct QueryEngine(Arc<Engine>);

impl QueryEngine {
    pub fn text_space(&self) -> &Space {
        &self.0.text_space
    }

    /// 加载了 mmproj 且开启了图片
    pub fn images(&self) -> bool {
        self.0.images
    }

    /// 档位为 `tier` 的图像空间；没有加载 mmproj 或没有开启图片时为空
    pub fn image_space(&self, tier: u32) -> Option<Space> {
        self.0.images.then(|| space_row(&self.0.cfg, Some(tier)))
    }
}

fn spawn(name: String, f: impl FnOnce() + Send + 'static) {
    if let Err(err) = thread::Builder::new().name(name.clone()).spawn(f) {
        log::error!("无法启动索引线程 {name}：{err}");
    }
}

impl Pipeline {
    /// 先把上次崩溃时停在处理中的作业计一次失败、把不符合当前设置的已完成文件放回重建（`rebuild`），再启动各线程；
    /// `paused` 为启动时的暂停原因。会读写数据库，在阻塞线程中调用
    pub fn start(
        db: Arc<Mutex<Db>>,
        index: Arc<Mutex<VIndex>>,
        params: Arc<Mutex<BTreeMap<String, ParamValue>>>,
        events: broadcast::Sender<Event>,
        opts: Options,
        disabled: DisabledGpus,
        paused: Option<PauseReason>,
    ) -> Pipeline {
        match lock(&db).recover_jobs() {
            Ok(0) => {}
            Ok(n) => log::warn!("{n} 个文件在上次退出时正在处理，各计一次失败。"),
            Err(err) => log::error!("无法恢复上次中断的作业：{err}"),
        }
        let threads = int(&lock(&params), "index_threads").max(1) as usize;
        let meta = |key: &str| {
            lock(&db).meta_get(key).unwrap_or_else(|err| {
                log::warn!("无法读取 {key}：{err}");
                None
            })
        };
        let preferred = meta(PREFERRED_DEVICE_KEY).unwrap_or_else(|| AUTO_DEVICE.to_string());
        // 只有核心明确记为未完成时才等引导；没有记录（测试、命令行）照常处理
        let setup = meta(ONBOARDED_KEY).as_deref() == Some("0");
        let shared = Arc::new(Shared {
            db,
            index,
            params,
            events,
            opts,
            disabled,
            engine: Mutex::new(None),
            dead: Mutex::new(None),
            cpu_only: AtomicBool::new(false),
            preferred: Mutex::new(preferred),
            held_setup: AtomicBool::new(setup),
            held_selftest: AtomicBool::new(false),
            held_models: AtomicBool::new(false),
            held_clear: AtomicBool::new(false),
            last_released: Mutex::new(None),
            no_model: AtomicBool::new(false),
            no_mmproj: AtomicBool::new(false),
            error: Mutex::new(None),
            stuck: Arc::new(prep::Stuck::default()),
            media_mem: Arc::new(prep::MediaMem::default()),
            busy: AtomicBool::new(true),
            inflight: AtomicUsize::new(0),
            woken: Mutex::new(false),
            wake_cv: Condvar::new(),
            stop: AtomicBool::new(false),
            slow: AtomicBool::new(false),
            paused: Mutex::new(paused),
            sleep: Mutex::new(wake::Sleep::default()),
            sleep_cv: Condvar::new(),
            verify: Mutex::new(()),
            recent: Mutex::new(write::Recent::default()),
            stale: Mutex::new(Vec::new()),
            rebuild_total: Mutex::new(0),
            calib_lock: Mutex::new(()),
            calib_stop: Mutex::new(false),
            calib_cv: Condvar::new(),
        });
        let requeued = {
            let db = lock(&shared.db);
            shared.requeue_stale(&db)
        };
        match requeued {
            Ok(n) if n.is_empty() => {}
            Ok(n) => log::info!("{n:?} 个文件的索引与当前设置不符，排队重建。"),
            Err(err) => log::error!("无法检查哪些文件要按当前设置重建：{err}"),
        }
        let (out_tx, out_rx) = mpsc::channel();
        // 容量为 0：调度线程手里最多压着一个已取出的作业
        let (task_tx, task_rx) = mpsc::sync_channel::<Task>(0);
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Prepared>(threads);
        let s = shared.clone();
        spawn("index-write".into(), move || write::run(&s, out_rx));
        let (s, out) = (shared.clone(), out_tx.clone());
        spawn("index-embed".into(), move || embed::run(&s, ready_rx, &out));
        let task_rx = Arc::new(Mutex::new(task_rx));
        for i in 0..threads {
            let (s, rx, ready, out) = (
                shared.clone(),
                task_rx.clone(),
                ready_tx.clone(),
                out_tx.clone(),
            );
            spawn(format!("index-prep-{i}"), move || {
                prep::supervise(&s, &rx, &ready, &out)
            });
        }
        drop(ready_tx);
        let (s, out) = (shared.clone(), out_tx.clone());
        spawn("index-calib".into(), move || calib::run(&s, &out));
        let (s, out) = (shared.clone(), out_tx.clone());
        spawn("index-schedule".into(), move || {
            schedule(&s, &task_tx, &out)
        });
        Pipeline {
            shared,
            out: out_tx,
        }
    }

    /// 有新作业时调用
    pub fn wake(&self) {
        self.shared.wake();
    }

    pub fn status(&self) -> Result<IndexStatus, DbError> {
        self.shared.status()
    }

    /// 立即广播一次进度：作业状态在流水线之外改变时（重试失败的文件），暂停中也要让界面与托盘看到新的计数
    pub fn broadcast(&self) {
        self.shared.broadcast();
    }

    /// 设置暂停原因；有变化时立即广播进度。暂停时丢弃推理引擎，解除时唤醒调度线程
    pub fn set_paused(&self, reason: Option<PauseReason>) {
        let mut cur = lock(&self.shared.paused);
        if *cur == reason {
            return;
        }
        *cur = reason;
        if reason.is_none() {
            self.shared.wake();
        }
        self.shared.send_status(reason);
        drop(cur);
        if reason.is_some() {
            self.shared.retire(&mut lock(&self.shared.engine));
        }
    }

    /// 打开或关闭降温；关闭后调度线程不再等完剩下的休息时间
    pub fn set_slow(&self, slow: bool) {
        if self.shared.slow.swap(slow, Ordering::Relaxed) != slow {
            log::info!(
                "{}",
                if slow {
                    "手机发热，放慢建立索引"
                } else {
                    "结束降温"
                }
            );
        }
    }

    /// 参数 `engine_idle_min` 修改后交给当前的推理引擎，立即生效
    pub fn apply_engine_idle(&self) {
        let min = int(&lock(&self.shared.params), "engine_idle_min").max(0) as u64;
        if let Some(e) = &*lock(&self.shared.engine) {
            e.backend.set_idle_timeout(Duration::from_secs(min * 60));
        }
    }

    /// 有已取出、还没有最终结果写入的作业
    pub fn busy(&self) -> bool {
        self.shared.inflight.load(Ordering::Relaxed) > 0
    }

    /// 当前的推理引擎，引擎槽为空（如 GPU 刚失效被丢弃）时立即新建，不等调度线程；主模型不可用、暂停建引擎、
    /// 创建失败时为空。唤醒验证进行中时先等它结束（`wake`）
    pub fn query_engine(&self) -> Option<QueryEngine> {
        self.wait_verified();
        self.shared
            .engine(&self.out)
            .filter(|e| !e.failed.load(Ordering::Relaxed))
            .map(QueryEngine)
    }

    /// 以查询优先级计算向量。推理后端失效时与索引一样丢弃该引擎（换设备按驱动证据门槛，见 `engine`），
    /// 换成新建的引擎再算一次，`engine` 随之替换，调用方之后从它取向量空间；是否屏蔽 GPU 要先等几秒查系统日志，
    /// 放到后台线程
    pub fn embed_query(
        &self,
        engine: &mut QueryEngine,
        input: Input,
    ) -> Result<Vec<f32>, EmbedError> {
        let start = self.shared.start();
        let err = match engine.0.backend.embed(input.clone(), Priority::Query) {
            Ok(e) => return Ok(e.vector),
            Err(err) => err,
        };
        let dropped = engine::backend_failed(&engine.0, &err);
        // 换哪个设备在这里同步决定（驱动证据门槛），下面新建的引擎不会先初始化别的显卡
        if self.shared.drop_engine(&engine.0, &err, start) {
            let (s, e) = (self.shared.clone(), engine.0.clone());
            let (lost, msg) = (engine::lost(&err), err.to_string());
            spawn("gpu-verdict".into(), move || {
                s.judge_gpu(&e, lost, &msg, start)
            });
        }
        self.shared.wake();
        if !dropped {
            return Err(err);
        }
        let Some(next) = self.query_engine() else {
            return Err(err);
        };
        if Arc::ptr_eq(&next.0, &engine.0) {
            return Err(err);
        }
        log::warn!("查询改用 {} 重新计算：{err}", next.0.device);
        *engine = next;
        engine
            .0
            .backend
            .embed(input, Priority::Query)
            .map(|e| e.vector)
    }

    /// 正常退出前调用：各线程在手头的文件处理完后退出，处理中的作业放回 `pending`。
    /// 停止标志在数据库锁内设置，调度线程在同一把锁内检查后才领取，放回之后不会再领出新作业
    pub fn stop(&self) {
        let db = lock(&self.shared.db);
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Err(err) = db.release_jobs() {
            log::warn!("无法放回处理中的作业：{err}");
        }
        drop(db);
        self.shared.wake();
        self.shared.stop_calib();
        self.shared.stuck.wake_all();
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake();
        self.shared.stop_calib();
    }
}

#[cfg(test)]
mod tests;
