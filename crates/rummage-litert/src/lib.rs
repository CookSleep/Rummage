//! LiteRT-LM 嵌入引擎的最小 FFI 与安全封装：计算 EmbeddingGemma 2 的多模态向量。
//!
//! - 签名以 `third_party/litert-lm/v0.18.0/c/` 中的头文件快照为准（v0.18.0 的函数直接返回指针或值，
//!   与 main 分支的“状态码 + 输出参数”不同，不能对照），只声明用到的函数，手写而不用 bindgen。
//! - 运行库（Windows 为 `litert-lm.dll`，Android 为 `liblitert-lm.so`）用 `libloading` 在运行时加载，
//!   缺文件或缺符号时，错误中写明是哪一个。升级运行库时必须对照新头文件逐项核对本文件。
//! - 后端（`Accel`）：文本与视觉同用主后端（CPU、GPU 或 NPU），音频编码器总用 CPU（官方 Android 示例的做法，
//!   部分 NPU 不支持音频）。GPU 与 NPU 只在 Android 上可用：电脑上 LiteRT 的 WebGPU 后端会让外接显卡驱动
//!   反复超时重置、只能重启，`Engine::new` 在 Android 以外拒绝非 CPU 后端。
//!   GPU 由运行库按文件名从自身所在目录加载加速器库（OpenCL 等）；NPU 从 `Accel::Npu` 的目录加载厂商的分发库。
//! - 缓存目录（`Settings::cache_dir`）：XNNPack 权重缓存与 GPU 程序缓存，加快第二次加载。
//! - 精度：激活类型显式设为 FLOAT32：模型卡要求不用 float16，而 GPU 的默认是 FP16（骁龙 8 Gen 2 上与 CPU 的余弦中位 0.9965、
//!   最低 0.983，FP32 时为 0.9996、0.986）；CPU 与 NPU 不受这个设置影响。最长输入显式设置（引擎默认取模型文件中的 1024）。
//! - 输入：文本为 UTF-8，图像为编码后的文件字节（PNG 等），音频为 WAV 文件字节；`<bos>`、`<eos>` 与图像、音频的
//!   边界 token 由引擎插入。超过最长输入时报错（溢出策略 ERROR），不截断、不分块平均。
//! - 输出：未归一化的向量，归一化与数值检查由 `rummage-embed` 统一做。
//! - 错误信息存在调用线程的线程局部存储中，失败后在同一线程上立即读取。

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;

use libloading::Library;

/// engine.h `LiteRtLmInputDataType`
const INPUT_TEXT: c_int = 0;
const INPUT_IMAGE: c_int = 1;
const INPUT_AUDIO: c_int = 3;
/// engine.h `kLiteRtLmActivationDataTypeFloat32`
const ACTIVATION_FLOAT32: c_int = 0;
/// engine.h `kLiteRtLmLogSeverityWarning`
const LOG_WARNING: c_int = 3;
/// embedding_engine.h `kLiteRtLmInputOverflowStrategyError`
const OVERFLOW_ERROR: c_int = 2;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("找不到运行库文件 \"{0}\"")]
    MissingFile(PathBuf),
    #[error("无法加载运行库 \"{file}\"：{source}")]
    LoadLibrary {
        file: PathBuf,
        source: libloading::Error,
    },
    #[error("运行库缺少符号 {symbol}：{source}")]
    MissingSymbol {
        symbol: String,
        source: libloading::Error,
    },
    #[error("路径不是有效的 UTF-8 或含有空字符：{0}")]
    InvalidPath(PathBuf),
    #[error("输入无效：{0}")]
    InvalidInput(String),
    #[error("无法创建推理引擎：{0}")]
    CreateEngine(String),
    #[error("推理失败：{0}")]
    Inference(String),
}

/// 一个输入片段，按顺序拼成一个序列。
pub enum Part<'a> {
    Text(&'a str),
    /// 编码后的图像文件字节（PNG、JPEG 等）
    Image(&'a [u8]),
    /// WAV 文件字节
    Audio(&'a [u8]),
}

type Ptr = *mut c_void;

/// 用到的全部函数。函数指针取自 `_lib`，它放在最后一个字段，保证最后释放。
struct Api {
    // engine.h
    set_min_log_level: unsafe extern "C" fn(c_int),
    input_data_create: unsafe extern "C" fn(c_int, *const c_void, usize) -> Ptr,
    input_data_delete: unsafe extern "C" fn(Ptr),
    // error_reporter.h
    get_last_error_message: unsafe extern "C" fn() -> *const c_char,
    clear_last_error: unsafe extern "C" fn(),
    // embedding_engine.h
    settings_create:
        unsafe extern "C" fn(*const c_char, *const c_char, *const c_char, *const c_char) -> Ptr,
    settings_delete: unsafe extern "C" fn(Ptr),
    settings_set_num_threads: unsafe extern "C" fn(Ptr, c_int),
    settings_set_audio_num_threads: unsafe extern "C" fn(Ptr, c_int),
    settings_set_max_input_length: unsafe extern "C" fn(Ptr, c_int),
    settings_set_vision_tokens_per_image: unsafe extern "C" fn(Ptr, c_int),
    settings_set_activation_data_type: unsafe extern "C" fn(Ptr, c_int),
    settings_set_cache_dir: unsafe extern "C" fn(Ptr, *const c_char),
    settings_set_dispatch_lib_dir: unsafe extern "C" fn(Ptr, *const c_char),
    settings_set_vision_dispatch_lib_dir: unsafe extern "C" fn(Ptr, *const c_char),
    options_create: unsafe extern "C" fn() -> Ptr,
    options_delete: unsafe extern "C" fn(Ptr),
    options_set_normalize: unsafe extern "C" fn(Ptr, bool),
    options_set_input_overflow_strategy: unsafe extern "C" fn(Ptr, c_int),
    options_set_vision_tokens_per_image: unsafe extern "C" fn(Ptr, c_int),
    engine_create: unsafe extern "C" fn(Ptr) -> Ptr,
    engine_delete: unsafe extern "C" fn(Ptr),
    engine_compute_embedding: unsafe extern "C" fn(Ptr, *const Ptr, usize, Ptr) -> Ptr,
    response_delete: unsafe extern "C" fn(Ptr),
    response_get_size: unsafe extern "C" fn(Ptr) -> usize,
    response_get_values: unsafe extern "C" fn(Ptr) -> *const f32,
    /// 不卸载：库在线程局部存储中登记了析构函数（错误信息等），卸载后线程退出时会调用已卸载的代码而崩溃
    _lib: ManuallyDrop<Library>,
}

/// 从库中取出函数指针。调用方保证 `T` 与头文件快照中的签名一致。
fn sym<T: Copy>(lib: &Library, name: &str) -> Result<T, Error> {
    // SAFETY: `T` 按头文件快照声明（见调用处）；取出的指针只在 `Api` 中使用，而 `Api` 持有该库直到最后释放。
    unsafe { lib.get::<T>(name.as_bytes()) }
        .map(|s| *s)
        .map_err(|err| Error::MissingSymbol {
            symbol: name.to_string(),
            source: err,
        })
}

impl Api {
    fn load(dir: &Path) -> Result<Self, Error> {
        let path = dir.join(libloading::library_filename("litert-lm"));
        if !path.is_file() {
            return Err(Error::MissingFile(path));
        }
        // SAFETY: 加载的是运行库目录中我们随附的 LiteRT-LM 动态库，其初始化代码不依赖调用方状态。
        let lib = unsafe { Library::new(&path) }.map_err(|err| Error::LoadLibrary {
            file: path,
            source: err,
        })?;
        let p = "litert_lm_embedding_";
        Ok(Api {
            set_min_log_level: sym(&lib, "litert_lm_set_min_log_level")?,
            input_data_create: sym(&lib, "litert_lm_input_data_create")?,
            input_data_delete: sym(&lib, "litert_lm_input_data_delete")?,
            get_last_error_message: sym(&lib, "litert_lm_get_last_error_message")?,
            clear_last_error: sym(&lib, "litert_lm_clear_last_error")?,
            settings_create: sym(&lib, &format!("{p}engine_settings_create"))?,
            settings_delete: sym(&lib, &format!("{p}engine_settings_delete"))?,
            settings_set_num_threads: sym(&lib, &format!("{p}engine_settings_set_num_threads"))?,
            settings_set_audio_num_threads: sym(
                &lib,
                &format!("{p}engine_settings_set_audio_num_threads"),
            )?,
            settings_set_max_input_length: sym(
                &lib,
                &format!("{p}engine_settings_set_max_input_length"),
            )?,
            settings_set_vision_tokens_per_image: sym(
                &lib,
                &format!("{p}engine_settings_set_vision_tokens_per_image"),
            )?,
            settings_set_activation_data_type: sym(
                &lib,
                &format!("{p}engine_settings_set_activation_data_type"),
            )?,
            settings_set_cache_dir: sym(&lib, &format!("{p}engine_settings_set_cache_dir"))?,
            settings_set_dispatch_lib_dir: sym(
                &lib,
                &format!("{p}engine_settings_set_litert_dispatch_lib_dir"),
            )?,
            settings_set_vision_dispatch_lib_dir: sym(
                &lib,
                &format!("{p}engine_settings_set_vision_litert_dispatch_lib_dir"),
            )?,
            options_create: sym(&lib, &format!("{p}options_create"))?,
            options_delete: sym(&lib, &format!("{p}options_delete"))?,
            options_set_normalize: sym(&lib, &format!("{p}options_set_normalize"))?,
            options_set_input_overflow_strategy: sym(
                &lib,
                &format!("{p}options_set_input_overflow_strategy"),
            )?,
            options_set_vision_tokens_per_image: sym(
                &lib,
                &format!("{p}options_set_vision_tokens_per_image"),
            )?,
            engine_create: sym(&lib, &format!("{p}engine_create"))?,
            engine_delete: sym(&lib, &format!("{p}engine_delete"))?,
            engine_compute_embedding: sym(&lib, &format!("{p}engine_compute_embedding"))?,
            response_delete: sym(&lib, &format!("{p}response_delete"))?,
            response_get_size: sym(&lib, &format!("{p}response_get_size"))?,
            response_get_values: sym(&lib, &format!("{p}response_get_values"))?,
            _lib: ManuallyDrop::new(lib),
        })
    }

    /// 调用线程上最近一次的错误信息；没有时为“未知错误”
    fn last_error(&self) -> String {
        // SAFETY: 无参数；返回的指针由库持有，在本线程下一次出错或清除前有效，这里立即复制。
        let msg = unsafe { (self.get_last_error_message)() };
        if msg.is_null() {
            return "未知错误".to_string();
        }
        // SAFETY: 非空时是以 NUL 结尾的 UTF-8 字符串（error_reporter.h）。
        unsafe { CStr::from_ptr(msg) }
            .to_string_lossy()
            .into_owned()
    }
}

/// 文本与视觉编码器的后端
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accel<'a> {
    Cpu,
    Gpu,
    /// 厂商的分发库（`libLiteRtDispatch_*.so`）及其依赖所在的目录
    Npu(&'a Path),
}

/// 引擎的设置
pub struct Settings<'a> {
    /// `.litertlm` 模型文件
    pub model: &'a Path,
    pub accel: Accel<'a>,
    /// 权重与程序缓存的目录；为空时不缓存
    pub cache_dir: Option<&'a Path>,
    pub threads: u32,
    /// 音频编码器的线程数
    pub audio_threads: u32,
    /// 文本编码器的最长输入（token）
    pub max_input: u32,
    /// 引擎默认的每张图像视觉 token 档位；每次请求另行指定（`Engine::embed` 的 `vision_tokens`）
    pub vision_tokens: u32,
}

pub struct Engine {
    api: Arc<Api>,
    ptr: Ptr,
}

// SAFETY: 引擎只经由 `&mut self`（`embed`）与 `Drop` 使用，同一时刻只有一个线程调用它；
// LiteRT-LM 的引擎对象不依赖创建它的线程（官方 Kotlin 绑定同样在任意线程上调用）。
unsafe impl Send for Engine {}

impl Engine {
    /// 从运行库目录 `dir` 加载 LiteRT-LM，按 `settings` 创建引擎（加载模型）
    pub fn new(dir: &Path, settings: &Settings) -> Result<Self, Error> {
        if cfg!(not(target_os = "android")) && settings.accel != Accel::Cpu {
            return Err(Error::CreateEngine("电脑上只能使用 CPU 后端。".to_string()));
        }
        let api = Arc::new(Api::load(dir)?);
        let c_path = |p: &Path| {
            p.to_str()
                .and_then(|s| CString::new(s).ok())
                .ok_or_else(|| Error::InvalidPath(p.to_path_buf()))
        };
        let model = c_path(settings.model)?;
        let cache = settings.cache_dir.map(c_path).transpose()?;
        let dispatch = match settings.accel {
            Accel::Npu(d) => Some(c_path(d)?),
            _ => None,
        };
        let backend = match settings.accel {
            Accel::Cpu => c"cpu",
            Accel::Gpu => c"gpu",
            Accel::Npu(_) => c"npu",
        };
        let int = |v: u32| c_int::try_from(v).unwrap_or(c_int::MAX);
        // SAFETY: 参数都是有效的 NUL 结尾字符串或空指针（视觉后端同主后端）；返回的设置在本函数内释放。
        // 设置函数只读写设置对象，引擎创建时复制所需内容。
        unsafe {
            (api.set_min_log_level)(LOG_WARNING);
            (api.clear_last_error)();
            let s = (api.settings_create)(
                model.as_ptr(),
                backend.as_ptr(),
                ptr::null(),
                c"cpu".as_ptr(),
            );
            if s.is_null() {
                return Err(Error::CreateEngine(api.last_error()));
            }
            (api.settings_set_num_threads)(s, int(settings.threads));
            (api.settings_set_audio_num_threads)(s, int(settings.audio_threads));
            (api.settings_set_max_input_length)(s, int(settings.max_input));
            (api.settings_set_vision_tokens_per_image)(s, int(settings.vision_tokens));
            (api.settings_set_activation_data_type)(s, ACTIVATION_FLOAT32);
            if let Some(c) = &cache {
                (api.settings_set_cache_dir)(s, c.as_ptr());
            }
            if let Some(d) = &dispatch {
                (api.settings_set_dispatch_lib_dir)(s, d.as_ptr());
                (api.settings_set_vision_dispatch_lib_dir)(s, d.as_ptr());
            }
            let ptr = (api.engine_create)(s);
            (api.settings_delete)(s);
            if ptr.is_null() {
                return Err(Error::CreateEngine(api.last_error()));
            }
            Ok(Engine { api, ptr })
        }
    }

    /// 计算一个输入的向量（未归一化）；`vision_tokens` 为本次图像的视觉 token 档位
    pub fn embed(&mut self, parts: &[Part], vision_tokens: u32) -> Result<Vec<f32>, Error> {
        let api = &self.api;
        let mut inputs: Vec<Ptr> = Vec::with_capacity(parts.len());
        // 输入数据与选项在任何返回路径上都要释放
        struct Guard<'a> {
            api: &'a Api,
            inputs: &'a mut Vec<Ptr>,
            opts: Ptr,
        }
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                // SAFETY: 这些对象都由本次调用创建、只释放一次；引擎在计算时已复制输入。
                unsafe {
                    for p in self.inputs.drain(..) {
                        (self.api.input_data_delete)(p);
                    }
                    if !self.opts.is_null() {
                        (self.api.options_delete)(self.opts);
                    }
                }
            }
        }
        // SAFETY: 无参数；返回的选项由 `Guard` 释放。
        let opts = unsafe {
            (api.clear_last_error)();
            (api.options_create)()
        };
        let guard = Guard {
            api,
            inputs: &mut inputs,
            opts,
        };
        if opts.is_null() {
            return Err(Error::Inference(api.last_error()));
        }
        // SAFETY: `opts` 非空，由本次调用创建。
        unsafe {
            (api.options_set_normalize)(opts, false);
            (api.options_set_input_overflow_strategy)(opts, OVERFLOW_ERROR);
            (api.options_set_vision_tokens_per_image)(
                opts,
                c_int::try_from(vision_tokens).unwrap_or(c_int::MAX),
            );
        }
        for part in parts {
            let (kind, data) = match part {
                Part::Text(t) => {
                    if t.contains('\0') {
                        return Err(Error::InvalidInput("文本含有空字符。".to_string()));
                    }
                    (INPUT_TEXT, t.as_bytes())
                }
                Part::Image(b) => (INPUT_IMAGE, *b),
                Part::Audio(b) => (INPUT_AUDIO, *b),
            };
            // SAFETY: `data` 在调用期间有效，库会复制它（engine.h：“The data is copied internally”）。
            let p = unsafe { (api.input_data_create)(kind, data.as_ptr().cast(), data.len()) };
            if p.is_null() {
                return Err(Error::InvalidInput(api.last_error()));
            }
            guard.inputs.push(p);
        }
        // SAFETY: 引擎有效；`inputs` 中是本次创建的输入，数组在调用期间有效；返回的响应在本块内释放。
        unsafe {
            let resp = (api.engine_compute_embedding)(
                self.ptr,
                guard.inputs.as_ptr(),
                guard.inputs.len(),
                opts,
            );
            if resp.is_null() {
                return Err(Error::Inference(api.last_error()));
            }
            let n = (api.response_get_size)(resp);
            let values = (api.response_get_values)(resp);
            let out = if values.is_null() || n == 0 {
                Vec::new()
            } else {
                // 数组由响应持有，删除响应前复制出来（embedding_engine.h）。
                std::slice::from_raw_parts(values, n).to_vec()
            };
            (api.response_delete)(resp);
            Ok(out)
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // SAFETY: `ptr` 由 `engine_create` 返回且只释放一次；`api` 中的库此时仍已加载。
        unsafe { (self.api.engine_delete)(self.ptr) }
    }
}
