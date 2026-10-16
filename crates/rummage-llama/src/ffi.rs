//! llama.cpp / ggml / mtmd 的 C 接口声明与运行时加载。
//!
//! 签名和结构体布局以 `third_party/llama.cpp/b11461/` 中的头文件快照为准，只声明用到的函数，手写而不用 bindgen。
//! 运行库按 `GGML_BACKEND_DL` 构建：先用 `libloading` 按依赖顺序加载 ggml-base、ggml、llama、mtmd，
//! 后端模块（ggml-cpu-*.dll 等）再由 `ggml_backend_load_all_from_path` 加载。缺文件或缺符号时，错误中写明是哪一个。
//! 升级运行库时必须对照新头文件逐项核对本文件。

use std::ffi::{c_char, c_int, c_void};
use std::path::Path;

use libloading::Library;

use crate::Error;

pub type LlamaToken = i32;
pub type LlamaPos = i32;
pub type LlamaSeqId = i32;
/// `ggml_backend_dev_t`
pub type DevT = *mut c_void;
/// `ggml_log_callback`
pub type LogCallback = extern "C" fn(level: c_int, text: *const c_char, user_data: *mut c_void);

// llama.h / ggml.h / ggml-backend.h / mtmd.h 中的枚举值。
pub const LLAMA_POOLING_TYPE_MEAN: c_int = 1;
pub const LLAMA_ATTENTION_TYPE_NON_CAUSAL: c_int = 1;
pub const LLAMA_FLASH_ATTN_TYPE_DISABLED: c_int = 0;
pub const LLAMA_FLASH_ATTN_TYPE_ENABLED: c_int = 1;
pub const LLAMA_PROCESS_TYPE_ENCODE: c_int = 0;
pub const GGML_BACKEND_DEVICE_TYPE_CPU: c_int = 0;
pub const GGML_BACKEND_DEVICE_TYPE_GPU: c_int = 1;
pub const GGML_BACKEND_DEVICE_TYPE_IGPU: c_int = 2;
pub const LLAMA_SPLIT_MODE_NONE: c_int = 0;
pub const GGML_LOG_LEVEL_WARN: c_int = 3;
#[cfg(target_os = "android")]
pub const GGML_LOG_LEVEL_ERROR: c_int = 4;
pub const GGML_LOG_LEVEL_CONT: c_int = 5;
pub const MTMD_INPUT_CHUNK_TYPE_TEXT: c_int = 0;

/// llama.h `llama_progress_callback`、mtmd.h `mtmd_progress_callback`：返回假时中止加载
pub type ProgressCallback = extern "C" fn(f32, *mut c_void) -> bool;
/// ggml.h `ggml_abort_callback`：返回真时中止计算
pub type AbortCallback = extern "C" fn(*mut c_void) -> bool;
/// ggml-backend.h `ggml_backend_sched_eval_callback`，第一个参数是 `struct ggml_tensor *`
pub type EvalCallback = extern "C" fn(*mut c_void, bool, *mut c_void) -> bool;

/// llama.h `struct llama_model_params`
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LlamaModelParams {
    pub devices: *mut DevT,
    pub tensor_buft_overrides: *const c_void,
    pub n_gpu_layers: i32,
    pub split_mode: c_int,
    pub load_mode: c_int,
    pub lazy_mode: c_int,
    pub main_gpu: i32,
    pub tensor_split: *const f32,
    pub progress_callback: Option<ProgressCallback>,
    pub progress_callback_user_data: *mut c_void,
    pub kv_overrides: *const c_void,
    pub vocab_only: bool,
    pub check_tensors: bool,
    pub use_extra_bufts: bool,
    pub no_host: bool,
    pub no_alloc: bool,
    pub load_mtp: bool,
}

/// llama.h `struct llama_context_params`
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LlamaContextParams {
    pub n_ctx: u32,
    pub n_batch: u32,
    pub n_ubatch: u32,
    pub n_seq_max: u32,
    pub n_rs_seq: u32,
    pub n_outputs_max: u32,
    pub n_outputs_max_per_seq: u32,
    pub n_threads: i32,
    pub n_threads_batch: i32,
    pub ctx_type: c_int,
    pub rope_scaling_type: c_int,
    pub pooling_type: c_int,
    pub attention_type: c_int,
    pub flash_attn_type: c_int,
    pub rope_freq_base: f32,
    pub rope_freq_scale: f32,
    pub yarn_ext_factor: f32,
    pub yarn_attn_factor: f32,
    pub yarn_beta_fast: f32,
    pub yarn_beta_slow: f32,
    pub yarn_orig_ctx: u32,
    pub defrag_thold: f32,
    pub cb_eval: *mut c_void,
    pub cb_eval_user_data: *mut c_void,
    pub type_k: c_int,
    pub type_v: c_int,
    pub abort_callback: Option<AbortCallback>,
    pub abort_callback_data: *mut c_void,
    pub embeddings: bool,
    pub offload_kqv: bool,
    pub no_perf: bool,
    pub op_offload: bool,
    pub swa_full: bool,
    pub kv_unified: bool,
    pub samplers: *mut c_void,
    pub n_samplers: usize,
    pub ctx_other: *mut c_void,
}

/// llama.h `struct llama_embd`
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LlamaEmbd {
    pub data: *const f32,
    pub n_rows: usize,
    pub n_embd: usize,
}

/// ggml-backend.h `struct ggml_backend_dev_caps`
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DevCaps {
    pub async_: bool,
    pub host_buffer: bool,
    pub buffer_from_host_ptr: bool,
    pub events: bool,
    pub mmap_support: bool,
}

/// ggml-backend.h `struct ggml_backend_dev_props`
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DevProps {
    pub name: *const c_char,
    pub description: *const c_char,
    pub memory_free: usize,
    pub memory_total: usize,
    pub type_: c_int,
    /// PCI 总线号，形如 "0000:c1:00.0"；未知时为空指针。
    pub device_id: *const c_char,
    pub caps: DevCaps,
}

/// mtmd.h `struct mtmd_context_params`
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MtmdContextParams {
    pub use_gpu: bool,
    pub device: DevT,
    pub print_timings: bool,
    pub n_threads: c_int,
    pub image_marker: *const c_char,
    pub media_marker: *const c_char,
    pub flash_attn_type: c_int,
    pub warmup: bool,
    pub image_min_tokens: c_int,
    pub image_max_tokens: c_int,
    pub cb_eval: Option<EvalCallback>,
    pub cb_eval_user_data: *mut c_void,
    pub batch_max_tokens: i32,
    pub progress_callback: Option<ProgressCallback>,
    pub progress_callback_user_data: *mut c_void,
}

/// mtmd.h `struct mtmd_input_text`
#[repr(C)]
pub struct MtmdInputText {
    pub text: *const c_char,
    pub text_len: usize,
    pub add_special: bool,
    pub parse_special: bool,
}

/// 用到的全部函数。函数指针取自 `libs` 中的库，`libs` 放在最后一个字段，保证最后释放。
pub struct Api {
    // ggml.dll
    pub ggml_backend_load_all_from_path: unsafe extern "C" fn(*const c_char),
    pub ggml_backend_dev_count: unsafe extern "C" fn() -> usize,
    pub ggml_backend_dev_get: unsafe extern "C" fn(usize) -> DevT,
    // ggml-base.dll
    pub ggml_backend_dev_get_props: unsafe extern "C" fn(DevT, *mut DevProps),
    // llama.dll
    pub llama_log_set: unsafe extern "C" fn(LogCallback, *mut c_void),
    pub llama_backend_init: unsafe extern "C" fn(),
    pub llama_model_default_params: unsafe extern "C" fn() -> LlamaModelParams,
    pub llama_context_default_params: unsafe extern "C" fn() -> LlamaContextParams,
    pub llama_model_load_from_file:
        unsafe extern "C" fn(*const c_char, LlamaModelParams) -> *mut c_void,
    pub llama_model_free: unsafe extern "C" fn(*mut c_void),
    pub llama_init_from_model: unsafe extern "C" fn(*mut c_void, LlamaContextParams) -> *mut c_void,
    pub llama_free: unsafe extern "C" fn(*mut c_void),
    pub llama_model_get_vocab: unsafe extern "C" fn(*const c_void) -> *const c_void,
    pub llama_model_n_embd_inp: unsafe extern "C" fn(*const c_void) -> i32,
    pub llama_model_n_embd_out: unsafe extern "C" fn(*const c_void) -> i32,
    pub llama_tokenize: unsafe extern "C" fn(
        *const c_void,
        *const c_char,
        i32,
        *mut LlamaToken,
        i32,
        bool,
        bool,
    ) -> i32,
    pub llama_batch_ext_init: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    pub llama_batch_ext_free: unsafe extern "C" fn(*mut c_void),
    pub llama_batch_ext_clear: unsafe extern "C" fn(*mut c_void),
    pub llama_batch_ext_add_token: unsafe extern "C" fn(*mut c_void, LlamaSeqId, LlamaToken) -> i32,
    pub llama_batch_ext_add_embd: unsafe extern "C" fn(*mut c_void, LlamaSeqId, LlamaEmbd) -> i32,
    pub llama_batch_ext_set_output_embd: unsafe extern "C" fn(*mut c_void, i32, bool) -> bool,
    pub llama_batch_ext_set_pos: unsafe extern "C" fn(*mut c_void, i32, *const LlamaPos) -> bool,
    pub llama_process: unsafe extern "C" fn(*mut c_void, c_int, *mut c_void) -> i32,
    pub llama_get_embeddings_seq: unsafe extern "C" fn(*mut c_void, LlamaSeqId) -> *mut f32,
    // mtmd.dll
    pub mtmd_log_set: unsafe extern "C" fn(LogCallback, *mut c_void),
    pub mtmd_default_marker: unsafe extern "C" fn() -> *const c_char,
    pub mtmd_context_params_default: unsafe extern "C" fn() -> MtmdContextParams,
    pub mtmd_init_from_file:
        unsafe extern "C" fn(*const c_char, *const c_void, MtmdContextParams) -> *mut c_void,
    pub mtmd_free: unsafe extern "C" fn(*mut c_void),
    pub mtmd_support_vision: unsafe extern "C" fn(*const c_void) -> bool,
    pub mtmd_support_audio: unsafe extern "C" fn(*const c_void) -> bool,
    pub mtmd_get_audio_sample_rate: unsafe extern "C" fn(*const c_void) -> c_int,
    pub mtmd_bitmap_init: unsafe extern "C" fn(u32, u32, *const u8) -> *mut c_void,
    pub mtmd_bitmap_init_from_audio: unsafe extern "C" fn(usize, *const f32) -> *mut c_void,
    pub mtmd_bitmap_free: unsafe extern "C" fn(*mut c_void),
    pub mtmd_input_chunks_init: unsafe extern "C" fn() -> *mut c_void,
    pub mtmd_input_chunks_size: unsafe extern "C" fn(*const c_void) -> usize,
    pub mtmd_input_chunks_get: unsafe extern "C" fn(*const c_void, usize) -> *const c_void,
    pub mtmd_input_chunks_free: unsafe extern "C" fn(*mut c_void),
    pub mtmd_input_chunk_get_type: unsafe extern "C" fn(*const c_void) -> c_int,
    pub mtmd_input_chunk_get_tokens_text:
        unsafe extern "C" fn(*const c_void, *mut usize) -> *const LlamaToken,
    pub mtmd_input_chunk_get_n_tokens: unsafe extern "C" fn(*const c_void) -> usize,
    pub mtmd_tokenize: unsafe extern "C" fn(
        *const c_void,
        *mut c_void,
        *const MtmdInputText,
        *const *const c_void,
        usize,
    ) -> i32,
    pub mtmd_encode_chunk: unsafe extern "C" fn(*mut c_void, *const c_void) -> i32,
    pub mtmd_get_output_embd: unsafe extern "C" fn(*mut c_void) -> *mut f32,
    _libs: Vec<Library>,
}

fn open(dir: &Path, name: &str) -> Result<Library, Error> {
    let file = libloading::library_filename(name);
    let path = dir.join(&file);
    if !path.is_file() {
        return Err(Error::MissingFile(path));
    }
    // SAFETY: 加载的是运行库目录中我们随附的 llama.cpp 动态库，其初始化代码不依赖调用方状态。
    unsafe { Library::new(&path) }.map_err(|err| Error::LoadLibrary {
        file: path,
        source: err,
    })
}

/// 从库中取出函数指针。调用方保证 `T` 与头文件快照中的签名一致。
fn sym<T: Copy>(lib: &Library, file: &str, name: &str) -> Result<T, Error> {
    // SAFETY: `T` 按头文件快照声明（见调用处）；取出的指针只在 `Api` 中使用，而 `Api` 持有该库直到最后释放。
    unsafe { lib.get::<T>(name.as_bytes()) }
        .map(|s| *s)
        .map_err(|err| Error::MissingSymbol {
            file: file.to_string(),
            symbol: name.to_string(),
            source: err,
        })
}

impl Api {
    pub fn load(dir: &Path) -> Result<Self, Error> {
        let mut libs = Vec::new();
        // 预编译包的 ggml-cpu-*.dll 依赖同目录的 libomp.dll，而 ggml 用 LoadLibraryW 加载后端（只按默认搜索顺序找依赖），
        // 所以先把它加载进进程。Vulkan 包等没有这个文件时跳过。
        #[cfg(target_os = "windows")]
        if dir.join("libomp.dll").is_file() {
            libs.push(open(dir, "libomp")?);
        }
        // 按依赖顺序加载，后加载的库能在已加载模块中找到前面的库。
        let base = open(dir, "ggml-base")?;
        let ggml = open(dir, "ggml")?;
        let llama = open(dir, "llama")?;
        let mtmd = open(dir, "mtmd")?;
        let name = |n: &str| {
            libloading::library_filename(n)
                .to_string_lossy()
                .into_owned()
        };
        let (fb, fg, fl, fm) = (name("ggml-base"), name("ggml"), name("llama"), name("mtmd"));
        let api = Api {
            ggml_backend_load_all_from_path: sym(&ggml, &fg, "ggml_backend_load_all_from_path")?,
            ggml_backend_dev_count: sym(&ggml, &fg, "ggml_backend_dev_count")?,
            ggml_backend_dev_get: sym(&ggml, &fg, "ggml_backend_dev_get")?,
            ggml_backend_dev_get_props: sym(&base, &fb, "ggml_backend_dev_get_props")?,
            llama_log_set: sym(&llama, &fl, "llama_log_set")?,
            llama_backend_init: sym(&llama, &fl, "llama_backend_init")?,
            llama_model_default_params: sym(&llama, &fl, "llama_model_default_params")?,
            llama_context_default_params: sym(&llama, &fl, "llama_context_default_params")?,
            llama_model_load_from_file: sym(&llama, &fl, "llama_model_load_from_file")?,
            llama_model_free: sym(&llama, &fl, "llama_model_free")?,
            llama_init_from_model: sym(&llama, &fl, "llama_init_from_model")?,
            llama_free: sym(&llama, &fl, "llama_free")?,
            llama_model_get_vocab: sym(&llama, &fl, "llama_model_get_vocab")?,
            llama_model_n_embd_inp: sym(&llama, &fl, "llama_model_n_embd_inp")?,
            llama_model_n_embd_out: sym(&llama, &fl, "llama_model_n_embd_out")?,
            llama_tokenize: sym(&llama, &fl, "llama_tokenize")?,
            llama_batch_ext_init: sym(&llama, &fl, "llama_batch_ext_init")?,
            llama_batch_ext_free: sym(&llama, &fl, "llama_batch_ext_free")?,
            llama_batch_ext_clear: sym(&llama, &fl, "llama_batch_ext_clear")?,
            llama_batch_ext_add_token: sym(&llama, &fl, "llama_batch_ext_add_token")?,
            llama_batch_ext_add_embd: sym(&llama, &fl, "llama_batch_ext_add_embd")?,
            llama_batch_ext_set_output_embd: sym(&llama, &fl, "llama_batch_ext_set_output_embd")?,
            llama_batch_ext_set_pos: sym(&llama, &fl, "llama_batch_ext_set_pos")?,
            llama_process: sym(&llama, &fl, "llama_process")?,
            llama_get_embeddings_seq: sym(&llama, &fl, "llama_get_embeddings_seq")?,
            mtmd_log_set: sym(&mtmd, &fm, "mtmd_log_set")?,
            mtmd_default_marker: sym(&mtmd, &fm, "mtmd_default_marker")?,
            mtmd_context_params_default: sym(&mtmd, &fm, "mtmd_context_params_default")?,
            mtmd_init_from_file: sym(&mtmd, &fm, "mtmd_init_from_file")?,
            mtmd_free: sym(&mtmd, &fm, "mtmd_free")?,
            mtmd_support_vision: sym(&mtmd, &fm, "mtmd_support_vision")?,
            mtmd_support_audio: sym(&mtmd, &fm, "mtmd_support_audio")?,
            mtmd_get_audio_sample_rate: sym(&mtmd, &fm, "mtmd_get_audio_sample_rate")?,
            mtmd_bitmap_init: sym(&mtmd, &fm, "mtmd_bitmap_init")?,
            mtmd_bitmap_init_from_audio: sym(&mtmd, &fm, "mtmd_bitmap_init_from_audio")?,
            mtmd_bitmap_free: sym(&mtmd, &fm, "mtmd_bitmap_free")?,
            mtmd_input_chunks_init: sym(&mtmd, &fm, "mtmd_input_chunks_init")?,
            mtmd_input_chunks_size: sym(&mtmd, &fm, "mtmd_input_chunks_size")?,
            mtmd_input_chunks_get: sym(&mtmd, &fm, "mtmd_input_chunks_get")?,
            mtmd_input_chunks_free: sym(&mtmd, &fm, "mtmd_input_chunks_free")?,
            mtmd_input_chunk_get_type: sym(&mtmd, &fm, "mtmd_input_chunk_get_type")?,
            mtmd_input_chunk_get_tokens_text: sym(&mtmd, &fm, "mtmd_input_chunk_get_tokens_text")?,
            mtmd_input_chunk_get_n_tokens: sym(&mtmd, &fm, "mtmd_input_chunk_get_n_tokens")?,
            mtmd_tokenize: sym(&mtmd, &fm, "mtmd_tokenize")?,
            mtmd_encode_chunk: sym(&mtmd, &fm, "mtmd_encode_chunk")?,
            mtmd_get_output_embd: sym(&mtmd, &fm, "mtmd_get_output_embd")?,
            // 上面的函数指针都已取出，这里才把库移入 `Api`。
            _libs: {
                libs.extend([base, ggml, llama, mtmd]);
                libs
            },
        };
        Ok(api)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 按 b11461 头文件在 64 位平台上的布局手算的大小，防止字段漏写或类型写错。
    #[test]
    fn struct_sizes() {
        assert_eq!(size_of::<LlamaModelParams>(), 80);
        assert_eq!(size_of::<LlamaContextParams>(), 160);
        assert_eq!(size_of::<MtmdContextParams>(), 96);
        assert_eq!(size_of::<LlamaEmbd>(), 24);
        assert_eq!(size_of::<DevProps>(), 56);
        assert_eq!(size_of::<MtmdInputText>(), 24);
    }
}
