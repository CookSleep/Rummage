//! 向量计算的统一入口：其他 crate 只通过这里使用推理。
//!
//! - 输入：`Input` 区分查询、文档、图像、音频与视频窗口；任务前缀在本 crate 内拼接（`prefix`），调用方不拼。
//! - 输出：`Embedding` 带向量空间 ID（`space`）；向量都经过数值检查和 L2 归一化（`vector`）。
//! - 后端：`EmbeddingBackend` 为远程推理（PC 代算）预留的抽象，当前唯一的实现是 `LocalBackend`（`local`）：
//!   一个调度线程按优先级（查询先于索引，同级先来先到）把请求逐个交给推理子进程 `rummage-core worker`。
//! - 推理子进程（`worker`）：只在其中加载 llama.cpp；Windows 上放进 Job Object 并设内存上限，
//!   初始化与单次推理都有时限。默认只加载主模型，遇到图像或音频时才带上 mmproj 重启；空闲超时后结束。
//!   CPU 模式超时、崩溃后下次请求重新拉起，连续失败 3 次后不再重试；
//!   GPU 模式出错不再拉起（显存或内存分配失败只让这一项失败，下次请求重新拉起）；出现设备丢失时返回 `Error::DeviceLost`，由上层屏蔽该设备。
//!   GPU 由推理进程按描述与 PCI 号在它自己枚举出的设备中选定，原始序号也在推理进程中现算（`worker`）。
//! - 自检（`selftest`）：用黄金样本检查每个候选后端并选出最快的，结果按设备与版本缓存，未通过的 GPU 屏蔽。
//! - 协议见 `frame`；Windows 专有代码只在平台模块中（`windows.rs` / `other.rs`）。

mod frame;
mod local;
pub mod prefix;
pub mod selftest;
pub mod space;
pub mod vector;
mod worker;

#[cfg(not(windows))]
#[path = "other.rs"]
mod platform;
#[cfg(windows)]
#[path = "windows.rs"]
mod platform;

use std::path::PathBuf;
use std::time::Duration;

pub use frame::{DeviceInfo, GpuInfo};
pub use local::{Gpu, Limits, LocalBackend, WorkerSpec, list_devices};
pub use platform::adapters;
pub use space::{Backend, SpaceConfig, SpaceId};
pub use worker::{WorkerArgs, run_worker};

/// 一块显卡适配器的属性（`adapters`），供诊断包使用；只查询属性，不加载推理运行库。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Adapter {
    pub description: String,
    /// PCI 厂商与设备 ID
    pub vendor_id: u32,
    pub device_id: u32,
    /// 专用显存
    pub vram_mb: u64,
    /// 用户模式驱动版本；查询失败时为空
    pub driver: Option<String>,
}

/// 推理进程连续失败多少次后不再重新拉起（由上层回退到下一个后端）。
pub const MAX_FAILURES: u32 = 3;
/// 结束推理进程后最多等它退出多久；驱动挂起时进程可能结束不了，到期就不再等它，按设备失效处理
pub(crate) const KILL_GRACE: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("无法读取模型文件 \"{}\"：{source}", .path.display())]
    ModelIo {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("模型文件 \"{}\" 的大小为 {actual} 字节，应为 {expected} 字节，文件可能不完整或不是所需的版本。", .path.display())]
    ModelSize {
        path: PathBuf,
        actual: u64,
        expected: u64,
    },
    #[error("未加载多模态模型文件")]
    NoMmproj,
    #[error("输入无效：{0}")]
    InvalidInput(String),
    /// 文字查询无法编码（超过上下文长度等）；文件内容的同类错误为 `InvalidInput`，两者给用户看的整句不同
    #[error("查询无法编码：{0}")]
    QueryTooLong(String),
    /// 视频窗口超过 token 上限；与 `InvalidInput` 一样与后端是否正常无关
    #[error("输入过长：{0}")]
    TooLong(String),
    #[error("向量数值异常：{0}")]
    BadVector(String),
    #[error("无法启动推理进程：{0}")]
    Spawn(String),
    #[error("推理引擎启动失败：{0}")]
    InitFailed(String),
    /// 推理进程可见的 GPU 与要求的不符；`LocalBackend` 内部换一个推理进程重试，不会返回给调用方。
    #[error("推理引擎启动失败：{0}")]
    GpuMismatch(String),
    /// 推理进程找不到选中的显卡（外接显卡已拔出、被停用等）；之后的请求都返回它，由上层换下一个能用的设备，不屏蔽。
    #[error("找不到显卡，它可能已拔出、被停用或驱动未加载：{0}")]
    GpuNotFound(String),
    #[error("推理引擎在 {} 秒内没有就绪，已结束推理进程。", .0.as_secs_f64())]
    InitTimeout(Duration),
    #[error("单次推理超过 {} 秒，已结束推理进程。", .0.as_secs_f64())]
    InferTimeout(Duration),
    #[error("推理进程异常退出：{0}")]
    Crashed(String),
    /// 供上层屏蔽该设备；之后的请求都返回它。
    #[error("显卡设备丢失，本次运行不再使用它。\n{0}")]
    DeviceLost(String),
    /// 推理出错（输入无效的情况为 `InvalidInput`）；CPU 模式计入连续失败。
    #[error("推理失败：{0}")]
    Inference(String),
    #[error("推理进程的输出不符合协议：{0}")]
    Protocol(String),
    /// GPU 后端失效，或 CPU 模式连续失败 `MAX_FAILURES` 次；之后的请求都返回它，由上层回退。
    #[error("推理引擎不可用：{0}")]
    Unavailable(String),
    /// GPU 模式连续 `MAX_FAILURES` 次显存或内存分配失败（如显存一直被别的程序占满）；之后的请求都返回它。
    /// 与驱动无关，上层换下一块显卡
    #[error("显存或内存不足：{0}")]
    OutOfMemory(String),
    /// 推理进程在 `KILL_GRACE` 内结束不了（驱动挂死等）；之后的请求都返回它，`restart` 也不清除。
    /// 显卡上的由上层屏蔽这块卡、停用所有显卡改用 CPU；CPU 上的本次运行不再识别内容
    #[error("推理进程结束不了：{0}")]
    Stuck(String),
    #[error("推理调度线程已停止。")]
    Stopped,
    #[error("黄金样本无效：{0}")]
    Golden(String),
}

/// 单个输入用尽显存或内存时 `Error::Inference` 的内容，原样显示给用户
pub(crate) const OUT_OF_MEMORY: &str = "显存或内存不足，无法处理此文件。可以在「设置 → 索引」中降低“图片识别精度”，或在「设置 → 高级」中调高“内容识别进程内存上限（GB）”。";

impl Error {
    /// 显示给用户的整句（文件的索引状态、设备检测结果等，不只针对文件，所以只有缺模型、输入无法识别这类确实针对文件的才写“此文件”）。`Display` 带函数名、返回码等细节，只写进日志
    pub fn user_message(&self) -> String {
        match self {
            Error::ModelIo { .. } | Error::ModelSize { .. } => {
                "模型文件无法读取或已损坏，请重新下载模型。".into()
            }
            Error::NoMmproj => "尚未下载图片与音视频模型，无法识别此文件的内容。".into(),
            Error::InvalidInput(_) => "此文件的内容无法识别。".into(),
            Error::QueryTooLong(_) => "搜索内容过长，请缩短后再试。".into(),
            Error::TooLong(_) => "内容过长，无法识别。".into(),
            Error::BadVector(_) => "识别结果异常，无法使用。".into(),
            Error::Spawn(_) | Error::InitFailed(_) | Error::GpuMismatch(_) => {
                "内容识别无法启动。".into()
            }
            Error::GpuNotFound(_) => "找不到所选的显卡，它可能已拔出、被停用或驱动未加载。".into(),
            Error::InitTimeout(d) => {
                format!("内容识别在 {} 秒内没有启动完成，已停止。", d.as_secs_f64())
            }
            Error::InferTimeout(d) => format!("识别内容超过 {} 秒，已停止。", d.as_secs_f64()),
            Error::Crashed(_) => "内容识别意外中止。".into(),
            Error::DeviceLost(_) => "显卡出现错误，本次运行不再使用它。".into(),
            Error::Inference(msg) if msg == OUT_OF_MEMORY => msg.clone(),
            Error::Inference(_) | Error::Protocol(_) => "识别内容时出错。".into(),
            Error::Unavailable(_) => "内容识别不可用。".into(),
            Error::OutOfMemory(_) => "显存或内存不足，无法继续使用这块显卡。".into(),
            Error::Stuck(_) => "内容识别进程停止响应且无法结束，建议重新启动电脑。".into(),
            Error::Stopped => "内容识别已停止。".into(),
            Error::Golden(_) => "设备检测所需的样本文件无效，请重新安装 Rummage。".into(),
        }
    }
}

/// 一个待编码的输入。文本由本 crate 加任务前缀（前缀方案 A）；图像不加前缀。
#[derive(Debug, Clone)]
pub enum Input {
    Query(String),
    Document {
        title: Option<String>,
        text: String,
    },
    /// 已由 `rummage_media::prepare` / `fit` 按视觉 token 档位 `tokens` 缩放的图像，向量属于该档位的图像空间。
    Image {
        rgb: rummage_media::Rgb,
        tokens: u32,
    },
    /// 以图搜图的查询：图像同 `Image`，可附一段文字描述，图文合成一条输入、得到一个向量（主模型卡的交错输入）。
    /// 文字加查询前缀放在图像之前；文字为空白时与 `Image` 完全相同。向量属于该档位的图像空间
    ImageQuery {
        rgb: rummage_media::Rgb,
        tokens: u32,
        text: String,
    },
    /// 16 kHz 单声道 f32 采样（`rummage_media::audio`），不超过 30 秒；不加前缀。
    /// 向量属于预处理参数版本为 `preprocess`（`space::audio_preprocess`）的音频空间
    Audio {
        samples: Vec<f32>,
        preprocess: String,
    },
    /// 一个视频窗口（`rummage_media::video::assemble`），不加前缀；经 `mtmd_tokenize` 后超过 `max_tokens` 时
    /// 返回 `Error::TooLong`。向量属于预处理参数版本为 `preprocess`（`space::video_preprocess`）的视频空间
    Video {
        parts: Vec<rummage_media::video::VideoPart>,
        preprocess: String,
        max_tokens: usize,
    },
}

/// 交互查询永远插队。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Query,
    Index,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Embedding {
    /// 已 L2 归一化。
    pub vector: Vec<f32>,
    pub space: SpaceId,
}

pub trait EmbeddingBackend: Send + Sync {
    /// 阻塞到结果返回；不要在异步执行器上直接调用。
    fn embed(&self, input: Input, priority: Priority) -> Result<Embedding, Error>;
    /// 文本输入加上任务前缀后的 token 数，含 `<bos>`、`<eos>`（切片上限含前缀与特殊 token）；
    /// 只分词不推理。图像与音频输入返回 `InvalidInput`。阻塞到结果返回。
    fn count_tokens(&self, input: &Input, priority: Priority) -> Result<usize, Error>;
    /// 修改空闲卸载的时限（参数 `engine_idle_min`），立即生效；没有推理进程的后端不需要实现。
    fn set_idle_timeout(&self, _idle: Duration) {}
    /// 结束推理进程并清除失败记录，下一次请求时重新拉起（唤醒后验证未通过时用）；阻塞到完成。
    /// 没有推理进程的后端不需要实现。
    fn restart(&self) -> Result<(), Error> {
        Ok(())
    }
}
