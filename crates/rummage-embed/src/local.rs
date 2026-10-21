//! `LocalBackend`：本机推理子进程 + 调度线程。
//!
//! - 调度：请求进入优先级队列，`Query` 先于 `Index`，同级先来先到；调度线程每次取一个交给推理子进程，
//!   子进程一次只处理一个请求。调用方在 `embed` / `count_tokens` 中阻塞等待结果。
//! - 监督：子进程在第一次请求时启动（`rummage-core worker …`），Windows 上放进 Job Object 并限制内存；
//!   启动到“就绪”用 `worker_init_timeout_s`，单次推理用 `infer_timeout_s`，超时就结束整个作业。
//!   子进程放进作业后才发 `Start`，子进程收到后才加载运行库。
//! - CPU 模式：崩溃、超时后下一次请求重新拉起；连续失败（含推理出错与不合格的向量）`MAX_FAILURES` 次后
//!   返回“推理引擎不可用”，不再重试，由上层回退。
//! - GPU 模式：超时、崩溃、推理出错都结束子进程且不再拉起，返回“推理引擎不可用”（超时很可能就是 TDR，
//!   重启会再次初始化显卡）。例外是推理出错或崩溃、且本次请求期间的 stderr 中有显存或内存分配失败
//!   （单个输入过大）：只让这个请求失败（`Error::Inference`），结束子进程，下一次请求在同一块卡上重新拉起；
//!   连续 `MAX_FAILURES` 次时返回 `Error::OutOfMemory`（与驱动无关，上层换下一块显卡）。总是失败的文件由上层的失败黑名单兜底。
//! - 设备丢失：结束进程、读完 stderr（只保留最后 200 行）后，其中或错误信息中出现设备丢失
//!   （b11461 打印 `ggml_vulkan: device lost on <设备>`，见 ggml-vulkan.cpp 256、260、14297 行）时返回
//!   `Error::DeviceLost`，本次运行不再使用该设备，由上层屏蔽。
//! - 结束推理进程最多等 `KILL_GRACE`（驱动挂死时进程可能结束不了，见 `process`）：到期就不再等它，之后的请求都返回
//!   `Error::Stuck`（`restart` 也不清除），显卡上的由上层屏蔽该显卡、改用 CPU，CPU 上的本次运行不再识别内容；
//!   调度线程与 `LocalBackend` 的释放都不会因此永久卡住。
//! - 指定 GPU 时把它的描述与 PCI 号交给推理进程，由它现算 Vulkan 原始序号、只露出这一块卡（见 `worker`）；
//!   父进程不传序号，并去掉继承来的 `GGML_VK_VISIBLE_DEVICES`。推理进程报告可见的 GPU 不符（两次枚举之间顺序变了）时
//!   换一个推理进程重试一次，再不符就按找不到处理。找不到这块卡时返回 `Error::GpuNotFound`：之后的请求都返回它，
//!   这不是显卡出错，上层换下一个能用的设备、不屏蔽。
//! - 按需加载 mmproj 与空闲卸载：`WorkerSpec::mmproj` 只是允许使用的 mmproj，推理进程默认只加载主模型；
//!   遇到图像、音频或视频而当前进程没有加载 mmproj 时，结束它并带上 mmproj 重新拉起，之后一直保持，直到空闲卸载。
//!   LiteRT 只有一个模型文件，各模态的编码器由引擎按需加载，不因此重启。
//!   超过 `Limits::idle`（参数 `engine_idle_min`，可用 `set_idle_timeout` 随时修改）没有请求时结束推理进程，
//!   下一次请求再拉起（只加载主模型）。这两种结束都是主动的，不计入连续失败次数。
//! - 自检用：`start` 提前启动推理进程（计初始化耗时，允许时连同 mmproj 一起加载，避免测图像时再初始化一次显卡）；
//!   `kill_worker` 从调度线程之外结束推理进程（总时限到期时），调度线程随后收到“推理进程关闭了输出”，按上面的规则处理。
//! - GPU 模式下不合格的向量（NaN 等）同样不再拉起，但返回 `Error::BadVector` 本身：这是确定的结果，不是驱动错误
//!   （自检记为“未通过”，运行中上层只在本次运行停用这块显卡）。
//! - 搜索内容超过上下文长度时（文字查询的输入错误、以图搜图的文字描述超过 token 上限）返回 `Error::QueryTooLong`，
//!   给用户看的整句与文件内容的不同。
//! - 唤醒后验证：`restart` 结束推理进程并清除失败次数与“不可用”状态，下一次请求重新拉起；
//!   设备丢失、结束不了的状态保留，不会去重新初始化已丢失的显卡。
//! - 子进程本身（启动、放进作业、帧收发、结束）见 `process`；监督（启动、失败处理、按需加载 mmproj）见 `supervisor`。

use std::collections::{BTreeMap, VecDeque};
use std::ffi::{CStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rummage_media::video::VideoPart;
use rummage_proto::params::{self, ParamValue};

use crate::frame::{DeviceInfo, FromWorker, GpuInfo, MediaPart, ToWorker};
use crate::space::{self, Backend, SpaceConfig};
use crate::{
    Accel, Embedding, EmbeddingBackend, Error, Input, KILL_GRACE, LiteRtArgs, Priority, npu,
    platform, prefix,
};

mod process;
mod supervisor;

use process::{Proc, Recv};
use supervisor::Supervisor;

/// 由推理进程自己设置（`worker`）；启动推理进程时从继承的环境中去掉，免得它影响设备枚举。
pub(crate) const VK_VISIBLE_ENV: &CStr = c"GGML_VK_VISIBLE_DEVICES";
/// 错误信息中附带的 stderr 行数。
const ERROR_LINES: usize = 5;
/// 单个音频输入的采样数上限：模型最长 30 秒
pub(crate) const MAX_AUDIO_SAMPLES: usize =
    (rummage_media::audio::MAX_SEGMENT_S * rummage_media::audio::SAMPLE_RATE) as usize;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// b11461 的 ggml-vulkan 只打印 `ggml_vulkan: device lost on <设备>` 后终止；Vulkan-Hpp 的异常名
/// 与错误码名（`ErrorDeviceLost`、`VK_ERROR_DEVICE_LOST`）也一并识别。
fn device_lost(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("device lost") || t.contains("device_lost") || t.contains("devicelost")
}

/// 显存或内存分配失败：ggml-vulkan 的 Vulkan-Hpp 异常名、ggml 的分配失败日志（b11461 ggml-vulkan-buffers.cpp、
/// ggml-alloc.c、ggml-backend.cpp、ggml.c）。
fn out_of_memory(text: &str) -> bool {
    let t = text.to_lowercase();
    [
        "outofdevicememory",
        "outofhostmemory",
        "out_of_device_memory",
        "out_of_host_memory",
        "failed to allocate",
        "insufficient memory",
    ]
    .iter()
    .any(|m| t.contains(m))
}

/// 推理子进程的资源与时限，取自参数表。
#[derive(Debug, Clone)]
pub struct Limits {
    pub mem_bytes: u64,
    pub init_timeout: Duration,
    pub infer_timeout: Duration,
    /// 空闲多久后结束推理进程。
    pub idle: Duration,
}

impl Limits {
    /// 缺少或不合法的取值用参数表的默认值。
    pub fn from_params(values: &BTreeMap<String, ParamValue>) -> Self {
        let int = |key: &str| params::int(values, key) as u64;
        Limits {
            mem_bytes: int("worker_mem_gb") << 30,
            init_timeout: Duration::from_secs(int("worker_init_timeout_s")),
            infer_timeout: Duration::from_secs(int("infer_timeout_s")),
            idle: Duration::from_secs(int("engine_idle_min") * 60),
        }
    }
}

/// 选中的 GPU，由推理进程按描述与 PCI 号选定（见模块说明）。
#[derive(Debug, Clone)]
pub struct Gpu {
    /// ggml 设备描述；与 PCI 号一起供推理进程查找这块卡。
    pub description: String,
    pub pci_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WorkerSpec {
    /// rummage-core 可执行文件，以 `worker` 子命令启动。
    pub exe: PathBuf,
    pub runtime_dir: PathBuf,
    pub model: PathBuf,
    /// 允许使用的 mmproj；为空时只能编码文本。是否加载由运行时决定：遇到图像或音频时才加载（见模块说明）。
    pub mmproj: Option<PathBuf>,
    pub threads: u32,
    /// 为空时只用 CPU。LiteRT 的 GPU、NPU 也放在这里（描述为后端名），推理进程按 GPU 模式监督。
    pub gpu: Option<Gpu>,
    /// 用 LiteRT 时的参数；`model` 为通用版或 NPU 版的 `.litertlm`，`mmproj` 与它相同或为空
    pub litert: Option<LiteRtArgs>,
}

impl WorkerSpec {
    /// `mmproj` 为真且允许使用 mmproj 时带上它。
    fn args(&self, mmproj: bool) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            "worker".into(),
            "--runtime-dir".into(),
            self.runtime_dir.clone().into(),
            "--model".into(),
            self.model.clone().into(),
            "--threads".into(),
            self.threads.to_string().into(),
        ];
        if let Some(l) = &self.litert {
            args.extend([
                "--litert".into(),
                l.accel.as_str().into(),
                "--tokenizer".into(),
                l.tokenizer.clone().into(),
            ]);
            if let Some(d) = &l.cache_dir {
                args.extend(["--cache-dir".into(), d.clone().into()]);
            }
            if let Accel::Npu(d) = &l.accel {
                args.extend(["--dispatch-dir".into(), d.clone().into()]);
            }
            return args;
        }
        if let Some(p) = self.mmproj.as_ref().filter(|_| mmproj) {
            args.extend(["--mmproj".into(), p.clone().into()]);
        }
        if let Some(g) = &self.gpu {
            args.extend(["--gpu".into(), g.description.clone().into()]);
            if let Some(pci) = &g.pci_id {
                args.extend(["--gpu-pci".into(), pci.into()]);
            }
        }
        args
    }
}

/// `rummage-core` 列出设备的子命令参数。
pub fn list_devices_args(runtime_dir: &Path) -> Vec<OsString> {
    vec![
        "worker".into(),
        "--list-devices".into(),
        "--runtime-dir".into(),
        runtime_dir.as_os_str().to_owned(),
    ]
}

#[derive(Default)]
struct Status {
    pid: Option<u32>,
    device: Option<DeviceInfo>,
    /// 推理进程启动后（就绪之前）即设置，结束后清空。
    job: Option<Arc<platform::Job>>,
}

/// 可见的 GPU 不符时（见模块说明）用新的推理进程再启动一次，仍不符就按找不到这块卡处理。
fn retry_mismatch(mut spawn: impl FnMut() -> Result<(), Error>) -> Result<(), Error> {
    match spawn() {
        Err(Error::GpuMismatch(_)) => {}
        res => return res,
    }
    log::warn!("推理进程看到的 GPU 与要求的不符，重新启动推理进程再试一次。");
    match spawn() {
        Err(Error::GpuMismatch(msg)) => Err(Error::GpuNotFound(msg)),
        res => res,
    }
}

/// 文本输入加上任务前缀；图像、音频与视频返回 `None`。
pub(crate) fn prefixed(input: &Input) -> Option<String> {
    Some(match input {
        Input::Query(q) => prefix::query(q),
        Input::Document { title, text } => prefix::document(title.as_deref(), text),
        Input::Image { .. }
        | Input::ImageQuery { .. }
        | Input::Audio { .. }
        | Input::Video { .. } => return None,
    })
}

/// 视频窗口（与图文交错的以图搜图查询）的请求与负载（`frame::MediaPart`）。音频段与图像同单独输入一样检查（30 秒、48 的倍数由推理进程核对）
fn video_request(
    parts: &[VideoPart],
    max_tokens: usize,
    min_tier: u32,
) -> Result<(ToWorker, Vec<u8>), Error> {
    let mut payload = Vec::new();
    let mut heads = Vec::with_capacity(parts.len());
    for p in parts {
        heads.push(match p {
            VideoPart::Text(text) => MediaPart::Text { text: text.clone() },
            VideoPart::Frame(rgb) => {
                payload.extend_from_slice(&rgb.data);
                MediaPart::Image {
                    width: rgb.width,
                    height: rgb.height,
                }
            }
            VideoPart::Audio(samples) => {
                if samples.is_empty() || samples.len() > MAX_AUDIO_SAMPLES {
                    return Err(Error::InvalidInput(format!(
                        "视频窗口的音频段有 {} 个采样，应在 1 到 {MAX_AUDIO_SAMPLES} 之间。",
                        samples.len()
                    )));
                }
                payload.extend(samples.iter().flat_map(|x| x.to_le_bytes()));
                MediaPart::Audio {
                    samples: samples.len(),
                }
            }
        });
    }
    if !heads.iter().any(|h| matches!(h, MediaPart::Image { .. })) {
        return Err(Error::InvalidInput("视频窗口中没有画面帧。".to_string()));
    }
    Ok((
        ToWorker::Video {
            parts: heads,
            max_tokens,
            min_tier,
        },
        payload,
    ))
}

/// 两级优先队列：查询先于索引，同级先来先到。
struct Pending<T> {
    query: VecDeque<T>,
    index: VecDeque<T>,
    closed: bool,
    /// 空闲卸载的时限，`set_idle_timeout` 修改后唤醒调度线程重新计算。
    idle: Duration,
}

impl<T> Pending<T> {
    fn new(idle: Duration) -> Self {
        Pending {
            query: VecDeque::new(),
            index: VecDeque::new(),
            closed: false,
            idle,
        }
    }

    fn push(&mut self, priority: Priority, item: T) {
        match priority {
            Priority::Query => self.query.push_back(item),
            Priority::Index => self.index.push_back(item),
        }
    }

    fn pop(&mut self) -> Option<T> {
        self.query.pop_front().or_else(|| self.index.pop_front())
    }
}

enum Request {
    /// 只启动推理进程（`LocalBackend::start`）。
    Start,
    Embed(Input),
    Count(Input),
    Restart,
}

enum Reply {
    Done,
    Embedding(Embedding),
    Tokens(usize),
}

struct Job {
    req: Request,
    reply: mpsc::Sender<Result<Reply, Error>>,
}

enum Next {
    Job(Job),
    /// 推理进程运行中、空闲时限内没有请求。
    Idle,
    /// 队列已关闭且为空。
    Closed,
}

struct Queue {
    pending: Mutex<Pending<Job>>,
    ready: Condvar,
}

impl Queue {
    /// 阻塞到有请求；`running`（推理进程在运行）时从调用起计空闲时限，到期返回 `Idle`。
    fn next(&self, running: bool) -> Next {
        let since = Instant::now();
        let mut p = lock(&self.pending);
        loop {
            if let Some(job) = p.pop() {
                return Next::Job(job);
            }
            if p.closed {
                return Next::Closed;
            }
            if !running {
                p = self.ready.wait(p).unwrap_or_else(PoisonError::into_inner);
                continue;
            }
            let left = p.idle.saturating_sub(since.elapsed());
            if left.is_zero() {
                return Next::Idle;
            }
            p = self
                .ready
                .wait_timeout(p, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

pub struct LocalBackend {
    queue: Arc<Queue>,
    status: Arc<Mutex<Status>>,
    thread: Option<JoinHandle<()>>,
}

impl LocalBackend {
    /// 核对模型文件大小并启动调度线程；推理进程在第一次请求时启动。
    pub fn new(spec: WorkerSpec, limits: Limits) -> Result<Self, Error> {
        let cfg = match &spec.litert {
            Some(l) => {
                let name = spec.model.file_name().and_then(|n| n.to_str());
                let file = name.and_then(npu::litert_model).ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "\"{}\" 不是已知的 LiteRT 模型文件",
                        spec.model.display()
                    ))
                })?;
                file.check(&spec.model)?;
                space::TOKENIZER.check(&l.tokenizer)?;
                SpaceConfig::litert(file, l.accel.backend())
            }
            None => {
                space::MODEL.check(&spec.model)?;
                if let Some(p) = &spec.mmproj {
                    space::MMPROJ.check(p)?;
                }
                let backend = if spec.gpu.is_some() {
                    Backend::Vulkan
                } else {
                    Backend::Cpu
                };
                SpaceConfig::current(backend)
            }
        };
        let text_space = cfg.text();
        let queue = Arc::new(Queue {
            pending: Mutex::new(Pending::new(limits.idle)),
            ready: Condvar::new(),
        });
        let status = Arc::new(Mutex::new(Status::default()));
        let mut sup = Supervisor {
            spec,
            limits,
            proc: None,
            failures: 0,
            dead: None,
            status: status.clone(),
            with_mmproj: false,
        };
        let q = queue.clone();
        let thread = thread::Builder::new()
            .name("embed-dispatch".to_string())
            .spawn(move || {
                loop {
                    let job = match q.next(sup.proc.is_some()) {
                        Next::Job(job) => job,
                        Next::Idle => {
                            sup.unload();
                            continue;
                        }
                        Next::Closed => return,
                    };
                    let res = match job.req {
                        Request::Start => {
                            let mmproj = sup.spec.mmproj.is_some();
                            sup.ensure_started(mmproj).map(|()| Reply::Done)
                        }
                        Request::Embed(input) => {
                            sup.handle(input, &text_space, &cfg).map(Reply::Embedding)
                        }
                        Request::Count(input) => sup.count(&input).map(Reply::Tokens),
                        Request::Restart => {
                            sup.restart();
                            Ok(Reply::Done)
                        }
                    };
                    // 调用方已不再等待时丢弃结果。
                    let _ = job.reply.send(res);
                }
            })
            .map_err(|err| Error::Spawn(format!("无法启动调度线程：{err}")))?;
        Ok(LocalBackend {
            queue,
            status,
            thread: Some(thread),
        })
    }

    /// 当前推理进程的 PID；没有运行时为空。
    pub fn worker_pid(&self) -> Option<u32> {
        lock(&self.status).pid
    }

    /// 推理进程实际使用的设备；还没有启动成功过时为空。
    pub fn device(&self) -> Option<DeviceInfo> {
        lock(&self.status).device.clone()
    }

    /// 阻塞到推理进程就绪，用于自检计初始化耗时；已在运行时立即返回。
    pub fn start(&self) -> Result<(), Error> {
        self.submit(Request::Start, Priority::Query).map(|_| ())
    }

    /// 结束当前的推理进程（含正在初始化的），可在任意线程调用；正在等待的请求随即失败。
    pub(crate) fn kill_worker(&self) {
        if let Some(job) = &lock(&self.status).job {
            job.kill();
        }
    }

    fn submit(&self, req: Request, priority: Priority) -> Result<Reply, Error> {
        let (reply, rx) = mpsc::channel();
        {
            let mut p = lock(&self.queue.pending);
            if p.closed {
                return Err(Error::Stopped);
            }
            p.push(priority, Job { req, reply });
        }
        self.queue.ready.notify_one();
        // 调度线程退出（含 panic）时发送端被丢弃。
        rx.recv().unwrap_or(Err(Error::Stopped))
    }
}

impl EmbeddingBackend for LocalBackend {
    fn embed(&self, input: Input, priority: Priority) -> Result<Embedding, Error> {
        match self.submit(Request::Embed(input), priority)? {
            Reply::Embedding(e) => Ok(e),
            _ => unreachable!("推理请求总是返回向量"),
        }
    }

    fn count_tokens(&self, input: &Input, priority: Priority) -> Result<usize, Error> {
        // 不必复制图像像素或音频采样再交给调度线程。
        if matches!(
            input,
            Input::Image { .. }
                | Input::ImageQuery { .. }
                | Input::Audio { .. }
                | Input::Video { .. }
        ) {
            return Err(Error::InvalidInput(
                "只能计算文本输入的 token 数。".to_string(),
            ));
        }
        match self.submit(Request::Count(input.clone()), priority)? {
            Reply::Tokens(n) => Ok(n),
            _ => unreachable!("计数请求总是返回 token 数"),
        }
    }

    fn set_idle_timeout(&self, idle: Duration) {
        lock(&self.queue.pending).idle = idle;
        self.queue.ready.notify_all();
    }

    fn restart(&self) -> Result<(), Error> {
        self.submit(Request::Restart, Priority::Query).map(|_| ())
    }
}

impl Drop for LocalBackend {
    fn drop(&mut self) {
        lock(&self.queue.pending).closed = true;
        self.queue.ready.notify_all();
        // 调度线程退出时丢弃 Supervisor，推理进程随之结束。
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// 在子进程中列出运行库能看到的 GPU（做法见 `worker` 模块说明）。只加载运行库、不加载模型，时限按初始化时限计。
pub fn list_devices(
    exe: &Path,
    runtime_dir: &Path,
    limits: &Limits,
) -> Result<Vec<GpuInfo>, Error> {
    let mut proc = Proc::spawn(exe, list_devices_args(runtime_dir), limits.mem_bytes)?;
    let res = match proc.recv(limits.init_timeout) {
        Recv::Frame(FromWorker::Devices { devices }, _) => Ok(devices),
        Recv::Frame(FromWorker::Error { message, .. }, _) => Err(Error::InitFailed(message)),
        Recv::Frame(other, _) => Err(Error::Protocol(format!("列出设备时收到 {other:?}"))),
        Recv::Timeout => Err(Error::InitTimeout(limits.init_timeout)),
        Recv::Closed(msg) => Err(Error::Crashed(msg)),
    };
    let (_, tail, ended) = proc.kill(KILL_GRACE);
    // 结束不了说明驱动可能已停止响应：不用它列出的显卡，本次只测 CPU
    if !ended {
        return Err(Error::Stuck(format!(
            "列出显卡的进程在 {} 秒内没有结束。",
            KILL_GRACE.as_secs()
        )));
    }
    if res.is_err() && device_lost(&tail) {
        return Err(Error::DeviceLost(tail));
    }
    res
}

#[cfg(test)]
mod tests;
