//! llama.cpp 的最小 FFI 与安全封装：用 CPU 或指定的一块 GPU 计算 EmbeddingGemma 2 的多模态向量。
//!
//! 流程与参考实现 `scripts/golden/llama_embed.py`（只有 CPU）一一对应：
//! - 加载：`Runtime::load` 从运行库目录加载 ggml-base、ggml、llama、mtmd，再 `ggml_backend_load_all_from_path`；
//!   之后枚举设备（`Runtime::devices`）。推理在应用进程内运行（Android）时用 `Runtime::load_once`，每个目录只加载一次。
//! - 设备：不指定时只用 CPU，主模型 `n_gpu_layers = 0`、设备列表只放 CPU 设备，mtmd `use_gpu = false`；
//!   指定一块 GPU 时设备列表只放这块 GPU，`n_gpu_layers = 999`、
//!   `split_mode = NONE`、`main_gpu = 0`，mtmd `use_gpu = true` 并指定同一设备，其余上下文参数按 b11461 默认。
//! - 精度：Flash Attention 按调用方给的开关（`Embedder::new` 的 `flash_attn`）在 llama 上下文与 mtmd 两侧显式设置，
//!   不用默认的 AUTO；按后端开或关的规则在 `rummage-embed` 中。
//! - 上下文：嵌入模式、MEAN 池化、NON_CAUSAL，`n_ctx = n_batch = n_ubatch = 2048`（非因果编码不能拆微批），`n_seq_max = 1`。
//! - 输入：纯文本用 `llama_tokenize`；含媒体时文本里用 `mtmd_default_marker` 占位，`mtmd_tokenize` 展开后，
//!   媒体块 `mtmd_encode_chunk` 并按行放入批。整个输入一个序列、一次 ENCODE，取 `llama_get_embeddings_seq` 的池化结果。
//!   一个输入可以含多张图像、多段音频（视频窗口）；`embed_limited` 在 `mtmd_tokenize` 后先合计各块的 token 数，
//!   超过调用方给的上限就不编码。
//! - 输出：池化后、未归一化的向量；归一化与数值检查由 `rummage-embed` 统一做。
//! - 计数：`count_tokens` 用与纯文本输入相同的 `llama_tokenize` 设置分词，只返回 token 数（含 `<bos>`、`<eos>`），供切片使用。
//! - mmproj 可以不加载：只编码文本时不初始化 mtmd，遇到含媒体的输入报错。
//! - 中止（`Embedder::new` 的 `abort`，推理在应用进程内运行、线程无法强制结束时用）：标志置位后，模型与 mmproj 的加载
//!   （进度回调返回假）、llama 的编码（`abort_callback`，只对 CPU 生效）和 mtmd 的编码都尽快停下，返回 `Error::Aborted`。
//!   mtmd 没有中止回调，借用调度器的 `cb_eval`：每 `ABORT_CHECK_NODES` 个节点要一次结果（`ask` 为真时返回真），
//!   随后 `ask` 为假的调用返回假即停止本段计算（b11461 ggml-backend.cpp 1898-1919；只在 `ask` 时看标志会整段算完）；
//!   之后在逐个媒体块的循环中检查标志。中止只在计算节点之间生效（i7-12700H 上 1120 档图像最慢的单个节点约 2.4 秒）。
//!   不给标志时这些回调一律不设。
//! - `vulkan`：加载 ggml 之前按原始顺序列出 Vulkan 物理设备，供调用方算出 `GGML_VK_VISIBLE_DEVICES`。
//!
//! 图像须由调用方缩放到最终尺寸（48 的倍数）；mtmd 给出的软 token 数与按尺寸推算的不符时报错，避免 mtmd 再次缩放。

mod ffi;
pub mod vulkan;

use std::ffi::{CStr, CString, c_char, c_int, c_void};
#[cfg(not(target_os = "android"))]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use ffi::Api;

/// 一个输入必须放进一个微批。2048 足够放下 1120 档图像或黄金样本的视频窗口。
const MAX_TOKENS: u32 = 2048;
/// mtmd 每 48×48 像素一个视觉 token。
pub const PATCH_PIXELS: u32 = 48;
/// 一张图像允许的视觉 token 数上限，即模型允许的最大值。
const MAX_IMAGE_TOKENS: u32 = 1120;
const AUDIO_SAMPLE_RATE: c_int = 16000;
/// mtmd 编码时每隔多少个计算节点检查一次中止标志（见模块说明）
const ABORT_CHECK_NODES: u32 = 32;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("找不到运行库文件 \"{0}\"")]
    MissingFile(PathBuf),
    #[error("无法加载运行库 \"{file}\"：{source}")]
    LoadLibrary {
        file: PathBuf,
        source: libloading::Error,
    },
    #[error("运行库 \"{file}\" 缺少符号 {symbol}：{source}")]
    MissingSymbol {
        file: String,
        symbol: String,
        source: libloading::Error,
    },
    #[error("路径不是有效的 UTF-8 或含有空字符：{0}")]
    InvalidPath(PathBuf),
    #[error("没有找到任何计算设备")]
    NoDevice,
    #[error("{0}")]
    Vulkan(String),
    #[error("设备无效：{0}")]
    InvalidDevice(String),
    #[error("无法加载模型 {0}")]
    LoadModel(PathBuf),
    #[error("无法创建推理上下文")]
    CreateContext,
    #[error("无法加载多模态投影模型 {0}")]
    LoadMmproj(PathBuf),
    #[error("多模态投影模型不支持图像或音频，或音频采样率不是 16 kHz")]
    MmprojUnsupported,
    #[error("未加载多模态模型文件")]
    NoMmproj,
    #[error("输入无效：{0}")]
    InvalidInput(String),
    /// 含媒体的输入经 `mtmd_tokenize` 后超过调用方给的 token 上限（视频窗口）
    #[error("输入有 {tokens} 个 token，超过上限 {limit}")]
    TooManyTokens { tokens: usize, limit: usize },
    #[error("推理失败：{0}")]
    Inference(String),
    #[error("推理已中止")]
    Aborted,
}

/// 一个输入片段，按顺序拼成一个序列。
pub enum Part<'a> {
    Text(&'a str),
    /// 已缩放到最终尺寸的 RGB 像素，按行存放，长度为 `width * height * 3`。
    Image {
        width: u32,
        height: u32,
        rgb: &'a [u8],
    },
    /// 16 kHz 单声道、[-1, 1] 的 f32 采样。
    Audio(&'a [f32]),
}

static LAST_LOG_LEVEL: AtomicI32 = AtomicI32::new(0);

/// 只输出 WARN 及以上的日志；INFO / DEBUG 每次推理很多，不输出。推理子进程写到 stderr，由父进程读取末尾判断设备丢失等；
/// Android 上推理在应用进程内运行，交给 `log`
extern "C" fn log_callback(level: c_int, text: *const c_char, _user: *mut c_void) {
    // CONT 沿用上一条的级别。
    let level = if level == ffi::GGML_LOG_LEVEL_CONT {
        LAST_LOG_LEVEL.load(Ordering::Relaxed)
    } else {
        LAST_LOG_LEVEL.store(level, Ordering::Relaxed);
        level
    };
    if level < ffi::GGML_LOG_LEVEL_WARN || text.is_null() {
        return;
    }
    // SAFETY: llama.cpp 传入以空字符结尾的字符串，只在回调期间读取。
    let msg = unsafe { CStr::from_ptr(text) };
    #[cfg(target_os = "android")]
    {
        let msg = msg.to_string_lossy();
        let msg = msg.trim_end();
        if msg.is_empty() {
            return;
        }
        if level >= ffi::GGML_LOG_LEVEL_ERROR {
            log::error!("{msg}");
        } else {
            log::warn!("{msg}");
        }
    }
    // 写 stderr 失败时忽略，不能在 C 回调里 panic。
    #[cfg(not(target_os = "android"))]
    let _ = std::io::stderr().write_all(msg.to_bytes());
}

/// 中止标志与 mtmd 调度回调的节点计数；由 `Embedder` 装箱持有，地址交给各回调
struct Abort {
    flag: Arc<AtomicBool>,
    nodes: AtomicU32,
}

/// 回调的 `user_data`；回调只在 `Embedder` 的生命周期内被调用
fn abort_of<'a>(data: *mut c_void) -> &'a Abort {
    // SAFETY: data 是 `Embedder` 持有的 `Box<Abort>` 的地址，它在模型、上下文与 mtmd 都释放之后才释放
    unsafe { &*data.cast::<Abort>() }
}

/// llama 上下文的 `abort_callback`
extern "C" fn abort_requested(data: *mut c_void) -> bool {
    abort_of(data).flag.load(Ordering::Relaxed)
}

/// 模型与 mmproj 加载的进度回调：返回假中止加载
extern "C" fn keep_loading(_progress: f32, data: *mut c_void) -> bool {
    !abort_requested(data)
}

/// mtmd 调度器的 `cb_eval`（见模块说明）
extern "C" fn eval_check(_tensor: *mut c_void, ask: bool, data: *mut c_void) -> bool {
    let abort = abort_of(data);
    if ask {
        return abort.nodes.fetch_add(1, Ordering::Relaxed) % ABORT_CHECK_NODES
            == ABORT_CHECK_NODES - 1;
    }
    !abort.flag.load(Ordering::Relaxed)
}

fn c_path(path: &Path) -> Result<CString, Error> {
    // llama.cpp 与 ggml 按 UTF-8 解释路径。
    path.to_str()
        .and_then(|s| CString::new(s).ok())
        .ok_or_else(|| Error::InvalidPath(path.to_path_buf()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Cpu,
    Gpu,
    /// 核显，使用系统内存。
    Igpu,
    /// BLAS、AMX 等加速器与 META 设备。
    Other,
}

/// ggml 后端设备的信息。
#[derive(Debug, Clone)]
pub struct Device {
    /// 在 `Runtime::devices` 中的序号，即 `Embedder::new` 的 `device` 参数。
    pub index: usize,
    pub name: String,
    pub description: String,
    pub kind: DeviceKind,
    /// PCI 总线号，形如 "0000:01:00.0"；后端不提供时为空。
    pub pci_id: Option<String>,
    pub memory_total: u64,
}

/// 已加载的运行库。只加载一次，供模型与上下文共用。
pub struct Runtime {
    api: Arc<Api>,
    /// 与 `devices` 一一对应的设备句柄，由 ggml 的全局注册表持有，与运行库同生命周期。
    handles: Vec<ffi::DevT>,
    devices: Vec<Device>,
}

// SAFETY: 设备句柄指向 ggml 全局注册表中的设备，随 `api` 持有的运行库常驻、加载后不再变化；
// ggml 允许在多个线程中用同一个设备分别创建模型与上下文（llama.cpp 的服务端即如此）
unsafe impl Send for Runtime {}
// SAFETY: 同上；`Runtime` 加载后只读
unsafe impl Sync for Runtime {}

impl Runtime {
    /// 目录中的后端都会被加载（`ggml_backend_load_all_from_path`）。调用方要保证未经自检的 GPU 后端
    /// 只在可强行结束的子进程中加载：驱动出错可能卡死整个进程。
    pub fn load(dir: &Path) -> Result<Self, Error> {
        let api = Api::load(dir)?;
        let dir_c = c_path(dir)?;
        let text = |p: *const c_char| {
            // SAFETY: 非空时是 ggml 设备持有的以空字符结尾的字符串，这里立即复制。
            (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
        };
        let mut handles = Vec::new();
        let mut devices = Vec::new();
        // SAFETY: 函数指针来自刚加载且仍被 `api` 持有的库；回调是不捕获状态的 extern "C" 函数；目录字符串在调用期间有效；
        // `DevProps` 的字段都是整数、布尔或指针，全零是合法值，随后由 ggml 填写。
        unsafe {
            // llama_log_set 同时设置 ggml 的日志回调（b11461 src/llama-impl.cpp）。
            (api.llama_log_set)(log_callback, ptr::null_mut());
            (api.mtmd_log_set)(log_callback, ptr::null_mut());
            (api.ggml_backend_load_all_from_path)(dir_c.as_ptr());
            (api.llama_backend_init)();
            for k in 0..(api.ggml_backend_dev_count)() {
                let dev = (api.ggml_backend_dev_get)(k);
                let mut props: ffi::DevProps = std::mem::zeroed();
                (api.ggml_backend_dev_get_props)(dev, &mut props);
                handles.push(dev);
                devices.push(Device {
                    index: k,
                    name: text(props.name).unwrap_or_default(),
                    description: text(props.description).unwrap_or_default(),
                    kind: match props.type_ {
                        ffi::GGML_BACKEND_DEVICE_TYPE_CPU => DeviceKind::Cpu,
                        ffi::GGML_BACKEND_DEVICE_TYPE_GPU => DeviceKind::Gpu,
                        ffi::GGML_BACKEND_DEVICE_TYPE_IGPU => DeviceKind::Igpu,
                        _ => DeviceKind::Other,
                    },
                    // 不知道 PCI 号时可能给空串，与没有给出同样处理，上层只需比较 `None`
                    pci_id: text(props.device_id).filter(|p| !p.is_empty()),
                    memory_total: props.memory_total as u64,
                });
            }
        }
        if devices.is_empty() {
            return Err(Error::NoDevice);
        }
        Ok(Runtime {
            api: Arc::new(api),
            handles,
            devices,
        })
    }

    /// 同一进程中多次使用运行库时用它（Android 上推理在应用进程内运行）：每个目录只加载一次，之后返回同一个。
    /// `ggml_backend_load_all_from_path` 重复调用会重复注册后端
    pub fn load_once(dir: &Path) -> Result<Arc<Self>, Error> {
        static LOADED: Mutex<Vec<(PathBuf, Arc<Runtime>)>> = Mutex::new(Vec::new());
        let mut loaded = LOADED.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, rt)) = loaded.iter().find(|(d, _)| d == dir) {
            return Ok(rt.clone());
        }
        let rt = Arc::new(Self::load(dir)?);
        loaded.push((dir.to_path_buf(), rt.clone()));
        Ok(rt)
    }

    pub fn devices(&self) -> &[Device] {
        &self.devices
    }
}

struct Model {
    api: Arc<Api>,
    ptr: *mut c_void,
}

impl Drop for Model {
    fn drop(&mut self) {
        // SAFETY: `ptr` 由 llama_model_load_from_file 返回且非空；依赖它的上下文与 mtmd 已先释放（见 `Embedder` 字段顺序）。
        unsafe { (self.api.llama_model_free)(self.ptr) }
    }
}

struct Context {
    api: Arc<Api>,
    ptr: *mut c_void,
    batch: *mut c_void,
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: 两个指针由对应的 init 函数返回；批属于上下文，先释放批再释放上下文。
        unsafe {
            if !self.batch.is_null() {
                (self.api.llama_batch_ext_free)(self.batch);
            }
            (self.api.llama_free)(self.ptr);
        }
    }
}

struct Mtmd {
    api: Arc<Api>,
    ptr: *mut c_void,
}

impl Drop for Mtmd {
    fn drop(&mut self) {
        // SAFETY: `ptr` 由 mtmd_init_from_file 返回且非空。
        unsafe { (self.api.mtmd_free)(self.ptr) }
    }
}

/// mtmd 位图与输入块的临时持有者，出错时也能释放。
struct MediaInputs<'a> {
    api: &'a Api,
    bitmaps: Vec<*mut c_void>,
    chunks: *mut c_void,
}

impl Drop for MediaInputs<'_> {
    fn drop(&mut self) {
        // SAFETY: 指针由对应的 init 函数返回，各释放一次。
        unsafe {
            if !self.chunks.is_null() {
                (self.api.mtmd_input_chunks_free)(self.chunks);
            }
            for &bm in &self.bitmaps {
                (self.api.mtmd_bitmap_free)(bm);
            }
        }
    }
}

/// 主模型 + mmproj（可选）+ 推理上下文。字段按释放顺序排列：批与上下文、mtmd、主模型，最后是各回调用到的中止标志。
pub struct Embedder {
    ctx: Context,
    mtmd: Option<Mtmd>,
    model: Model,
    abort: Option<Box<Abort>>,
    api: Arc<Api>,
    marker: String,
    n_embd_inp: usize,
    n_embd_out: usize,
}

impl Embedder {
    /// `device` 为 `Runtime::devices` 中的序号，必须是 GPU 或核显；`None` 时只用 CPU。
    /// `mmproj` 为 `None` 时不初始化 mtmd，只能编码纯文本。`flash_attn` 同时用于 llama 上下文与 mtmd。
    /// `abort` 置位时加载与推理尽快停下并返回 `Error::Aborted`（见模块说明）；为 `None` 时不设任何回调
    pub fn new(
        rt: &Runtime,
        model: &Path,
        mmproj: Option<&Path>,
        threads: i32,
        device: Option<usize>,
        flash_attn: bool,
        abort: Option<Arc<AtomicBool>>,
    ) -> Result<Self, Error> {
        let api = rt.api.clone();
        let abort = abort.map(|flag| {
            Box::new(Abort {
                flag,
                nodes: AtomicU32::new(0),
            })
        });
        let abort_data = abort.as_deref().map_or(ptr::null_mut(), |a| {
            ptr::from_ref(a).cast_mut().cast::<c_void>()
        });
        let aborted = || {
            abort
                .as_ref()
                .is_some_and(|a| a.flag.load(Ordering::Relaxed))
        };
        let model_c = c_path(model)?;
        let mmproj_c = mmproj.map(c_path).transpose()?;
        let gpu = match device {
            None => None,
            Some(i) => {
                let d = rt
                    .devices
                    .get(i)
                    .ok_or_else(|| Error::InvalidDevice(format!("没有序号为 {i} 的设备")))?;
                if !matches!(d.kind, DeviceKind::Gpu | DeviceKind::Igpu) {
                    return Err(Error::InvalidDevice(format!(
                        "{}（{}）不是 GPU",
                        d.name, d.description
                    )));
                }
                Some(rt.handles[i])
            }
        };
        // 以空指针结尾的设备列表：指定 GPU 时只含这块 GPU，否则只含 CPU 设备；llama 在加载时复制该列表。
        let mut devs: Vec<ffi::DevT> = match gpu {
            Some(g) => vec![g],
            None => rt
                .devices
                .iter()
                .filter(|d| d.kind == DeviceKind::Cpu)
                .map(|d| rt.handles[d.index])
                .collect(),
        };
        if devs.is_empty() {
            return Err(Error::NoDevice);
        }
        devs.push(ptr::null_mut());
        let flash_attn = if flash_attn {
            ffi::LLAMA_FLASH_ATTN_TYPE_ENABLED
        } else {
            ffi::LLAMA_FLASH_ATTN_TYPE_DISABLED
        };

        // SAFETY: 参数结构体与头文件快照布局一致（见 ffi 的大小测试）；字符串与设备列表在调用期间有效。
        let model_ptr = unsafe {
            let mut mp = (api.llama_model_default_params)();
            mp.devices = devs.as_mut_ptr();
            if gpu.is_some() {
                // 全部层放在这一块 GPU 上。
                mp.n_gpu_layers = 999;
                mp.split_mode = ffi::LLAMA_SPLIT_MODE_NONE;
                mp.main_gpu = 0;
            } else {
                mp.n_gpu_layers = 0;
            }
            if abort.is_some() {
                mp.progress_callback = Some(keep_loading);
                mp.progress_callback_user_data = abort_data;
            }
            (api.llama_model_load_from_file)(model_c.as_ptr(), mp)
        };
        if model_ptr.is_null() && aborted() {
            return Err(Error::Aborted);
        }
        if model_ptr.is_null() {
            return Err(Error::LoadModel(model.to_path_buf()));
        }
        let model_h = Model {
            api: api.clone(),
            ptr: model_ptr,
        };

        // SAFETY: 同上；模型指针有效。
        let ctx_ptr = unsafe {
            let mut cp = (api.llama_context_default_params)();
            cp.n_ctx = MAX_TOKENS;
            cp.n_batch = MAX_TOKENS;
            cp.n_ubatch = MAX_TOKENS;
            cp.n_seq_max = 1;
            cp.n_threads = threads;
            cp.n_threads_batch = threads;
            cp.embeddings = true;
            cp.pooling_type = ffi::LLAMA_POOLING_TYPE_MEAN;
            cp.attention_type = ffi::LLAMA_ATTENTION_TYPE_NON_CAUSAL;
            cp.flash_attn_type = flash_attn;
            // 只用 CPU 时不让调度器把 KQV 或其他算子搬到运行库中的 GPU 上；指定 GPU 时按默认。
            if gpu.is_none() {
                cp.offload_kqv = false;
                cp.op_offload = false;
            }
            if abort.is_some() {
                cp.abort_callback = Some(abort_requested);
                cp.abort_callback_data = abort_data;
            }
            (api.llama_init_from_model)(model_ptr, cp)
        };
        if ctx_ptr.is_null() {
            return Err(Error::CreateContext);
        }
        let mut ctx = Context {
            api: api.clone(),
            ptr: ctx_ptr,
            batch: ptr::null_mut(),
        };
        // SAFETY: 上下文指针有效。
        ctx.batch = unsafe { (api.llama_batch_ext_init)(ctx_ptr) };
        if ctx.batch.is_null() {
            return Err(Error::CreateContext);
        }

        // SAFETY: 模型指针有效。
        let (n_embd_inp, n_embd_out) = unsafe {
            (
                (api.llama_model_n_embd_inp)(model_ptr),
                (api.llama_model_n_embd_out)(model_ptr),
            )
        };
        let (Some(mmproj), Some(mmproj_c)) = (mmproj, mmproj_c) else {
            return Ok(Embedder {
                ctx,
                mtmd: None,
                model: model_h,
                abort,
                api,
                marker: String::new(),
                n_embd_inp: n_embd_inp as usize,
                n_embd_out: n_embd_out as usize,
            });
        };

        // SAFETY: 同上；marker 为空，device 为空或 ggml 全局注册表中的设备句柄，与运行库同生命周期。
        let mtmd_ptr = unsafe {
            let mut p = (api.mtmd_context_params_default)();
            p.use_gpu = gpu.is_some();
            p.device = gpu.unwrap_or(ptr::null_mut());
            p.print_timings = false;
            p.n_threads = threads;
            p.flash_attn_type = flash_attn;
            p.warmup = true;
            // 图像已由调用方缩放到最终尺寸，mtmd 不再缩放；上限取模型允许的最大值 1120。
            p.image_min_tokens = 1;
            p.image_max_tokens = MAX_IMAGE_TOKENS as c_int;
            if abort.is_some() {
                p.cb_eval = Some(eval_check);
                p.cb_eval_user_data = abort_data;
                p.progress_callback = Some(keep_loading);
                p.progress_callback_user_data = abort_data;
            }
            (api.mtmd_init_from_file)(mmproj_c.as_ptr(), model_ptr, p)
        };
        if mtmd_ptr.is_null() && aborted() {
            return Err(Error::Aborted);
        }
        if mtmd_ptr.is_null() {
            return Err(Error::LoadMmproj(mmproj.to_path_buf()));
        }
        let mtmd = Mtmd {
            api: api.clone(),
            ptr: mtmd_ptr,
        };

        // SAFETY: mtmd 指针有效；默认标记是 mtmd 内部的静态字符串。
        let (ok, marker) = unsafe {
            let ok = (api.mtmd_support_vision)(mtmd_ptr)
                && (api.mtmd_support_audio)(mtmd_ptr)
                && (api.mtmd_get_audio_sample_rate)(mtmd_ptr) == AUDIO_SAMPLE_RATE;
            let marker = CStr::from_ptr((api.mtmd_default_marker)())
                .to_string_lossy()
                .into_owned();
            (ok, marker)
        };
        if !ok {
            return Err(Error::MmprojUnsupported);
        }
        Ok(Embedder {
            ctx,
            mtmd: Some(mtmd),
            model: model_h,
            abort,
            api,
            marker,
            n_embd_inp: n_embd_inp as usize,
            n_embd_out: n_embd_out as usize,
        })
    }

    fn aborted(&self) -> bool {
        self.abort
            .as_ref()
            .is_some_and(|a| a.flag.load(Ordering::Relaxed))
    }

    /// 把一个 token 或一行嵌入放到序列位置 `pos`，并要求输出它的嵌入（参与池化）。
    fn place(&self, idx: i32, pos: i32) -> Result<(), Error> {
        if idx < 0 {
            return Err(Error::Inference(format!("批已满或输入无效（{idx}）")));
        }
        // M-RoPE 模型每个位置要多个值，传 4 个，只有第一个有效。
        let p = [pos, 0, 0, 0];
        // SAFETY: 批指针有效，`idx` 是刚加入的条目，`p` 在调用期间有效。
        let ok = unsafe {
            (self.api.llama_batch_ext_set_pos)(self.ctx.batch, idx, p.as_ptr())
                && (self.api.llama_batch_ext_set_output_embd)(self.ctx.batch, idx, true)
        };
        if !ok {
            return Err(Error::Inference(format!("无法设置第 {idx} 个 token")));
        }
        Ok(())
    }

    fn add_token(&self, tok: ffi::LlamaToken, pos: &mut i32) -> Result<(), Error> {
        // SAFETY: 批指针有效。
        let idx = unsafe { (self.api.llama_batch_ext_add_token)(self.ctx.batch, 0, tok) };
        self.place(idx, *pos)?;
        *pos += 1;
        Ok(())
    }

    /// 纯文本分词：加特殊 token（`<bos>`、`<eos>`）并解析文本中的特殊 token，`embed` 与 `count_tokens` 共用。
    fn tokenize(&self, text: &str) -> Result<Vec<ffi::LlamaToken>, Error> {
        let api = &*self.api;
        let n_bytes =
            i32::try_from(text.len()).map_err(|_| Error::InvalidInput("文本过长".to_string()))?;
        let mut toks = vec![0 as ffi::LlamaToken; text.len() + 8];
        // SAFETY: 词表来自有效模型；缓冲区长度与传入的上限一致。
        let n = unsafe {
            let vocab = (api.llama_model_get_vocab)(self.model.ptr);
            (api.llama_tokenize)(
                vocab,
                text.as_ptr().cast(),
                n_bytes,
                toks.as_mut_ptr(),
                toks.len() as i32,
                true,
                true,
            )
        };
        if n < 0 {
            return Err(Error::Inference(format!("分词失败（{n}）")));
        }
        toks.truncate(n as usize);
        Ok(toks)
    }

    /// 纯文本的 token 数，含 `<bos>` 与 `<eos>`；与 `embed` 的纯文本路径分词方式相同，不做推理。
    pub fn count_tokens(&mut self, text: &str) -> Result<usize, Error> {
        Ok(self.tokenize(text)?.len())
    }

    /// 计算输入的向量：池化后、未归一化。
    pub fn embed(&mut self, parts: &[Part]) -> Result<Vec<f32>, Error> {
        self.embed_limited(parts, MAX_TOKENS as usize)
    }

    /// 同 `embed`，但含媒体的输入经 `mtmd_tokenize` 后超过 `max_tokens` 个 token 时不推理、返回 `TooManyTokens`
    pub fn embed_limited(&mut self, parts: &[Part], max_tokens: usize) -> Result<Vec<f32>, Error> {
        let has_input = parts.iter().any(|p| match p {
            Part::Text(t) => !t.is_empty(),
            _ => true,
        });
        if !has_input {
            return Err(Error::InvalidInput("输入为空".to_string()));
        }
        let api = self.api.clone();
        // SAFETY: 批指针有效。
        unsafe { (api.llama_batch_ext_clear)(self.ctx.batch) };
        let mut pos = 0i32;
        if parts.iter().all(|p| matches!(p, Part::Text(_))) {
            let text: String = parts
                .iter()
                .map(|p| match p {
                    Part::Text(t) => *t,
                    _ => "",
                })
                .collect();
            let toks = self.tokenize(&text)?;
            if toks.len() > MAX_TOKENS as usize {
                return Err(Error::InvalidInput(format!(
                    "输入 {} 个 token，超过 {MAX_TOKENS}",
                    toks.len()
                )));
            }
            for &t in &toks {
                self.add_token(t, &mut pos)?;
            }
        } else {
            self.add_media(parts, max_tokens.min(MAX_TOKENS as usize), &mut pos)?;
        }

        // SAFETY: 上下文与批指针有效；池化结果有 n_embd_out 个 f32，在下一次推理前有效，这里立即复制。
        unsafe {
            let rc =
                (api.llama_process)(self.ctx.ptr, ffi::LLAMA_PROCESS_TYPE_ENCODE, self.ctx.batch);
            if self.aborted() {
                return Err(Error::Aborted);
            }
            if rc != 0 {
                return Err(Error::Inference(format!("llama_process 失败（{rc}）")));
            }
            let pooled = (api.llama_get_embeddings_seq)(self.ctx.ptr, 0);
            if pooled.is_null() {
                return Err(Error::Inference("没有池化后的向量".to_string()));
            }
            Ok(std::slice::from_raw_parts(pooled, self.n_embd_out).to_vec())
        }
    }

    fn add_media(&self, parts: &[Part], limit: usize, pos: &mut i32) -> Result<(), Error> {
        let mtmd = self.mtmd.as_ref().ok_or(Error::NoMmproj)?;
        let api = &*self.api;
        let mut inputs = MediaInputs {
            api,
            bitmaps: Vec::new(),
            chunks: ptr::null_mut(),
        };
        // 每个媒体片段按尺寸推算的软 token 数；音频由 mtmd 决定，记为 None。
        let mut expected = Vec::new();
        let mut text = String::new();
        for p in parts {
            let bm = match p {
                Part::Text(t) => {
                    text.push_str(t);
                    continue;
                }
                Part::Image { width, height, rgb } => {
                    if rgb.len() as u64 != u64::from(*width) * u64::from(*height) * 3 {
                        return Err(Error::InvalidInput(format!(
                            "图像 {width}×{height} 的像素字节数 {} 不符",
                            rgb.len()
                        )));
                    }
                    let tokens = (width / PATCH_PIXELS) * (height / PATCH_PIXELS);
                    if *width == 0
                        || *height == 0
                        || width % PATCH_PIXELS != 0
                        || height % PATCH_PIXELS != 0
                        || !(1..=MAX_IMAGE_TOKENS).contains(&tokens)
                    {
                        return Err(Error::InvalidInput(format!(
                            "图像 {width}×{height} 的宽高须为 {PATCH_PIXELS} 的正整数倍，且视觉 token 数在 1 到 {MAX_IMAGE_TOKENS} 之间"
                        )));
                    }
                    expected.push(Some(tokens as usize));
                    // SAFETY: `rgb` 长度已核对为 width * height * 3；mtmd 复制像素。
                    unsafe { (api.mtmd_bitmap_init)(*width, *height, rgb.as_ptr()) }
                }
                Part::Audio(wav) => {
                    if wav.is_empty() {
                        return Err(Error::InvalidInput("音频为空".to_string()));
                    }
                    expected.push(None);
                    // SAFETY: 指针与长度来自同一切片；mtmd 复制采样。
                    unsafe { (api.mtmd_bitmap_init_from_audio)(wav.len(), wav.as_ptr()) }
                }
            };
            if bm.is_null() {
                return Err(Error::Inference("无法创建 mtmd 位图".to_string()));
            }
            inputs.bitmaps.push(bm);
            text.push_str(&self.marker);
        }

        let text_c =
            CString::new(text).map_err(|_| Error::InvalidInput("文本含有空字符".to_string()))?;
        let it = ffi::MtmdInputText {
            text: text_c.as_ptr(),
            text_len: text_c.as_bytes().len(),
            add_special: true,
            parse_special: true,
        };
        let mut got = Vec::new();
        // SAFETY: mtmd 与批指针有效；位图数组、文本在调用期间有效；输出嵌入有 n_tokens 行、每行 n_embd_inp 个 f32，
        // 在下一次 encode 前有效，加入批时被复制（llama_batch_ext_add_embd 复制数据）。
        unsafe {
            inputs.chunks = (api.mtmd_input_chunks_init)();
            if inputs.chunks.is_null() {
                return Err(Error::Inference("无法创建 mtmd 输入块".to_string()));
            }
            let bms: Vec<*const c_void> = inputs.bitmaps.iter().map(|&b| b.cast_const()).collect();
            let rc = (api.mtmd_tokenize)(mtmd.ptr, inputs.chunks, &it, bms.as_ptr(), bms.len());
            if rc != 0 {
                return Err(Error::Inference(format!("mtmd_tokenize 失败（{rc}）")));
            }
            let n_chunks = (api.mtmd_input_chunks_size)(inputs.chunks);
            // 编码媒体之前就知道确切长度，超限时不白算（含 <bos>、<eos> 与时间戳文字）
            let tokens: usize = (0..n_chunks)
                .map(|k| {
                    (api.mtmd_input_chunk_get_n_tokens)((api.mtmd_input_chunks_get)(
                        inputs.chunks,
                        k,
                    ))
                })
                .sum();
            if tokens > limit {
                return Err(Error::TooManyTokens { tokens, limit });
            }
            for k in 0..n_chunks {
                if self.aborted() {
                    return Err(Error::Aborted);
                }
                let ch = (api.mtmd_input_chunks_get)(inputs.chunks, k);
                if (api.mtmd_input_chunk_get_type)(ch) == ffi::MTMD_INPUT_CHUNK_TYPE_TEXT {
                    let mut n = 0usize;
                    let toks = (api.mtmd_input_chunk_get_tokens_text)(ch, &mut n);
                    if n == 0 {
                        continue;
                    }
                    for &t in std::slice::from_raw_parts(toks, n) {
                        self.add_token(t, pos)?;
                    }
                    continue;
                }
                let rc = (api.mtmd_encode_chunk)(mtmd.ptr, ch);
                // 中止时调度器停在半途，输出不完整
                if self.aborted() {
                    return Err(Error::Aborted);
                }
                if rc != 0 {
                    return Err(Error::Inference("mtmd_encode_chunk 失败".to_string()));
                }
                let rows = (api.mtmd_get_output_embd)(mtmd.ptr);
                if rows.is_null() {
                    return Err(Error::Inference("mtmd 没有输出嵌入".to_string()));
                }
                let n = (api.mtmd_input_chunk_get_n_tokens)(ch);
                got.push(n);
                for r in 0..n {
                    let e = ffi::LlamaEmbd {
                        data: rows.add(r * self.n_embd_inp),
                        n_rows: 1,
                        n_embd: self.n_embd_inp,
                    };
                    let idx = (api.llama_batch_ext_add_embd)(self.ctx.batch, 0, e);
                    self.place(idx, *pos)?;
                    *pos += 1;
                }
            }
        }
        // 只核对图像，音频的软 token 数由 mtmd 决定。
        let img_expected: Vec<usize> = expected.iter().flatten().copied().collect();
        let img_got: Vec<usize> = got
            .iter()
            .zip(&expected)
            .filter(|(_, e)| e.is_some())
            .map(|(g, _)| *g)
            .collect();
        if img_got != img_expected {
            return Err(Error::InvalidInput(format!(
                "图像软 token 数 {img_got:?} 与按尺寸推算的 {img_expected:?} 不符（mtmd 可能又缩放了图像）"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn data(abort: &Abort) -> *mut c_void {
        ptr::from_ref(abort).cast_mut().cast()
    }

    #[test]
    fn abort_callbacks() {
        let abort = Abort {
            flag: Arc::default(),
            nodes: AtomicU32::new(0),
        };
        let d = data(&abort);
        assert!(!abort_requested(d) && keep_loading(0.5, d));
        // 每 ABORT_CHECK_NODES 个节点要一次结果；之后的检查在标志置位前放行
        let asks: Vec<bool> = (0..2 * ABORT_CHECK_NODES)
            .map(|_| eval_check(ptr::null_mut(), true, d))
            .collect();
        assert_eq!(asks.iter().filter(|&&a| a).count(), 2);
        assert!(asks[ABORT_CHECK_NODES as usize - 1]);
        assert!(eval_check(ptr::null_mut(), false, d));
        abort.flag.store(true, Ordering::Relaxed);
        assert!(abort_requested(d) && !keep_loading(0.5, d));
        assert!(!eval_check(ptr::null_mut(), false, d));
    }

    /// 本机测试：CPU 上编码一张 560 档的图像，1 秒后置位中止标志，应在 1 秒内返回 `Aborted`。
    /// 运行库取仓库 `.cache` 中的 Windows CPU 包，模型取开发版数据目录；找不到时跳过
    #[test]
    #[ignore]
    fn abort_stops_image_encoding() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let runtime = root.join(".cache/third-party/llama.cpp/b11461/win-cpu-x64");
        let models = root.join("src-tauri/dev-data/models/embeddinggemma-2-gguf");
        let model = models.join("embeddinggemma-2-Q8_0.gguf");
        let mmproj = models.join("mmproj-embeddinggemma-2-Q8_0.gguf");
        if !runtime.is_dir() || !model.is_file() || !mmproj.is_file() {
            eprintln!("跳过：找不到运行库或模型");
            return;
        }
        let rt = Runtime::load(&runtime).unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let mut emb = Embedder::new(
            &rt,
            &model,
            Some(&mmproj),
            8,
            None,
            false,
            Some(flag.clone()),
        )
        .unwrap();
        // 560 档：中止只在计算节点之间生效，1120 档最慢的单个节点在 i7-12700H 上约 2.4 秒
        let (width, height) = (20 * PATCH_PIXELS, 28 * PATCH_PIXELS);
        let rgb: Vec<u8> = (0..width * height * 3).map(|i| (i % 251) as u8).collect();
        let setter = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(1));
            flag.store(true, Ordering::Relaxed);
            Instant::now()
        });
        let res = emb.embed(&[Part::Image {
            width,
            height,
            rgb: &rgb,
        }]);
        let returned = Instant::now();
        let set_at = setter.join().unwrap();
        assert!(
            matches!(res, Err(Error::Aborted)),
            "{:?}",
            res.map(|v| v.len())
        );
        let late = returned.saturating_duration_since(set_at);
        assert!(late < Duration::from_secs(1), "置位后 {late:?} 才返回");
    }
}
