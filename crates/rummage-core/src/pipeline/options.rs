//! 索引流水线的配置：运行库与模型的位置（`Config`）、创建推理后端的方式（`Factory`）与其他选项（`Options`）。
//! 推理后端默认为 llama.cpp 推理子进程（`Options::local`）；评测可改用 LiteRT-LM（`Options::litert`，只用 CPU），
//! 向量空间与所需的模型文件随之改变（`Options::space`、`Options::model_files`）。
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rummage_embed::space::{self, ModelFile};
use rummage_embed::{
    Backend, EmbeddingBackend, Error as EmbedError, Gpu, Limits, LiteRtBackend, LiteRtSpec,
    LocalBackend, SpaceConfig, WorkerSpec, selftest,
};
use rummage_proto::ParamValue;

/// 记录用户选择的设备的 `meta` 键：值为 `cpu` 或某块显卡的标识（`selftest::CandidateReport::device_id`），
/// 没有这一项时按自检记录自动选择
pub const PREFERRED_DEVICE_KEY: &str = "preferred_device";
/// 记录引导是否完成的 `meta` 键，值为 `1` 或 `0`（`server::setup`）
pub(crate) const ONBOARDED_KEY: &str = "onboarded";
/// `meta` 表中引导进行到的步骤，与 `ONBOARDED_KEY` 一样存在索引数据库中，随数据目录迁移；完成引导时删除
pub(crate) const ONBOARDING_STEP_KEY: &str = "onboarding_step";

/// 本次运行中显卡出的问题，由核心的 `State` 持有，与流水线共用
#[derive(Debug, Default)]
pub struct Disabled {
    /// 本次运行停用的显卡的键：选设备、重新检测时跳过
    pub keys: Vec<selftest::DeviceKey>,
    /// 出现了无法排除驱动原因的错误：本次运行所有显卡都停用，只用 CPU，也不再自动检测设备；
    /// 用户手动检测或解除屏蔽后的重测通过时恢复（见 `server::setup`）
    pub all: bool,
    /// 运行中最近一次出问题的设备（`CandidateReport::device_id`），界面据此说明改用了哪个设备
    pub failed: Option<String>,
}

pub type DisabledGpus = Arc<Mutex<Disabled>>;

/// 运行库与模型的位置（`main.rs` 按发布版或开发版给出）
#[derive(Debug, Clone)]
pub struct Config {
    /// rummage-core 可执行文件，推理进程以它的 `worker` 子命令启动
    pub exe: PathBuf,
    pub cpu_runtime: PathBuf,
    pub vulkan_runtime: PathBuf,
    pub model: PathBuf,
    pub mmproj: PathBuf,
    /// `<数据目录>\cache\selftest.json`
    pub selftest_cache: PathBuf,
    /// 黄金样本目录（自检与唤醒验证用）
    pub golden: PathBuf,
    /// ffmpeg.exe 与 ffprobe.exe 所在的目录（音频）
    pub ffmpeg: PathBuf,
    /// `<数据目录>\tmp\embed`：压缩存放的内嵌音视频解压到这里，解码完即删除
    pub embed_tmp: PathBuf,
    /// 列出系统中的显卡（`selftest::gpu_adapters`），判断检测结果是否过期、选中的显卡是否还在；测试中换成假的
    pub gpus: fn() -> Vec<String>,
}

impl Config {
    /// 数据目录中内嵌音视频的临时目录（`embed_tmp`），各处都从 `Config::embed_tmp` 取，只在这里拼接
    pub fn embed_tmp_dir(data_dir: &Path) -> PathBuf {
        data_dir.join("tmp").join("embed")
    }
}

/// 创建推理后端所需的选择
pub struct EngineSpec {
    pub gpu: Option<Gpu>,
    /// 是否加载 mmproj
    pub mmproj: bool,
    pub threads: u32,
    pub params: BTreeMap<String, ParamValue>,
}

pub type Factory =
    Box<dyn Fn(&EngineSpec) -> Result<Arc<dyn EmbeddingBackend>, EmbedError> + Send + Sync>;

/// 查询从给定时刻到现在系统日志中的显卡驱动事件（`selftest::driver_events`）；查询失败时返回错误
pub type DriverEvents = Box<dyn Fn(Instant) -> io::Result<Vec<String>> + Send + Sync>;

pub struct Options {
    pub model: PathBuf,
    pub mmproj: PathBuf,
    pub selftest_cache: PathBuf,
    /// ffmpeg.exe 与 ffprobe.exe 所在的目录
    pub ffmpeg: PathBuf,
    /// 内嵌音视频的临时目录，见 `Config::embed_tmp`
    pub embed_tmp: PathBuf,
    /// 模型文件是否可用；生产中核对大小（`ModelFile::check`），大小不对按缺失处理
    pub available: fn(&Path, &ModelFile) -> bool,
    pub factory: Factory,
    /// GPU 后端失效时用来判断是否持久屏蔽
    pub driver_events: DriverEvents,
    /// 覆盖参数表的 `file_timeout_s`，测试用
    pub file_timeout: Option<Duration>,
    /// 见 `Config::gpus`
    pub gpus: fn() -> Vec<String>,
    /// 推理后端为 LiteRT-LM（`Options::litert`）：`model` 与 `mmproj` 都是 `.litertlm` 文件
    pub litert: bool,
}

impl Options {
    /// 本机推理子进程（`LocalBackend`）
    pub fn local(cfg: Config) -> Options {
        let (model, mmproj) = (cfg.model.clone(), cfg.mmproj.clone());
        let selftest_cache = cfg.selftest_cache.clone();
        let ffmpeg = cfg.ffmpeg.clone();
        let embed_tmp = cfg.embed_tmp.clone();
        let gpus = cfg.gpus;
        let factory: Factory = Box::new(move |spec: &EngineSpec| {
            let runtime = match spec.gpu {
                Some(_) => &cfg.vulkan_runtime,
                None => &cfg.cpu_runtime,
            };
            let worker = WorkerSpec {
                exe: cfg.exe.clone(),
                runtime_dir: runtime.clone(),
                model: cfg.model.clone(),
                mmproj: spec.mmproj.then(|| cfg.mmproj.clone()),
                threads: spec.threads,
                gpu: spec.gpu.clone(),
            };
            let backend = LocalBackend::new(worker, Limits::from_params(&spec.params))?;
            Ok(Arc::new(backend) as Arc<dyn EmbeddingBackend>)
        });
        Options {
            model,
            mmproj,
            selftest_cache,
            ffmpeg,
            embed_tmp,
            available: |path, file| file.check(path).is_ok(),
            factory,
            // 先等系统日志写入可能的 TDR 事件，做法与自检相同
            driver_events: Box::new(|since| {
                thread::sleep(selftest::TDR_LOG_WAIT);
                selftest::driver_events(since.elapsed() + selftest::TDR_WINDOW_MARGIN)
            }),
            file_timeout: None,
            gpus,
            litert: false,
        }
    }

    /// LiteRT-LM 的 CPU 后端（评测用）：`cfg.model` 与 `cfg.mmproj` 都应是 `.litertlm` 文件，运行库在 `runtime_dir`，
    /// 分词器为 `tokenizer`。不使用显卡，调用方应指定只用 CPU
    pub fn litert(cfg: Config, runtime_dir: PathBuf, tokenizer: PathBuf) -> Options {
        let model = cfg.model.clone();
        let mut opts = Options::local(cfg);
        opts.factory = Box::new(move |spec: &EngineSpec| {
            let backend = LiteRtBackend::new(&LiteRtSpec {
                runtime_dir: runtime_dir.clone(),
                model: model.clone(),
                tokenizer: tokenizer.clone(),
                threads: spec.threads,
            })?;
            Ok(Arc::new(backend) as Arc<dyn EmbeddingBackend>)
        });
        opts.litert = true;
        opts
    }

    /// 后端 `backend` 的向量空间配置
    pub fn space(&self, backend: Backend) -> SpaceConfig {
        if self.litert {
            return SpaceConfig::litert();
        }
        SpaceConfig::current(backend)
    }

    /// 主模型与多模态模型（`model`、`mmproj`）应有的文件
    pub fn model_files(&self) -> (&'static ModelFile, &'static ModelFile) {
        if self.litert {
            return (&space::LITERT_MODEL, &space::LITERT_MODEL);
        }
        (&space::MODEL, &space::MMPROJ)
    }
}
