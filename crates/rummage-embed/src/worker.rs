//! 推理子进程 `rummage-core worker` 的主体，参数格式见 `WorkerArgs::parse`。
//! Android 上不启动子进程，同一主体在应用进程内的线程中运行（`platform` 的 `spawn`），收发同样的帧。
//!
//! 作为子进程时先保护 stdout（`platform::protocol_stdout`，见 `run_worker_stdio`），再等父进程发来 `Start`（父进程已把本进程放进
//! Job Object）才加载运行库；推理、列出设备两种模式都如此。
//!
//! 推理模式：加载运行库 → 选定设备 → 加载模型 → 发 `Ready` → 逐个处理 stdin 上的请求（推理或计数），stdin 关闭即退出。
//! - 指定 GPU 时（`--gpu <描述>`，有 PCI 号时同时给 `--gpu-pci`）：
//!   1. 加载运行库之前，用 Vulkan 加载器按原始顺序列出物理设备（`rummage_llama::vulkan`），按描述与 PCI 号
//!      （没有 PCI 号的只按描述）找到这块卡，须唯一（同一块卡重复出现时取第一个，见 `raw_index`）；
//!      把它的原始序号设为本进程的 `GGML_VK_VISIBLE_DEVICES`
//!      （`platform::set_runtime_env`），ggml 只看到这一块卡。不能不设：b11461 的锁页主机缓冲区总在
//!      `vk_instance.devices[0]` 上分配（`ggml_vk_host_malloc`，llama 的输出缓冲区与调度器都用它），
//!      不隔离时排在第一的卡即使没被选中也会建出逻辑设备（选核显时 4080 SUPER 上约 830 MB），
//!      也违反未经自检的显卡不初始化。序号不由父进程传：枚举顺序不保证稳定，在本进程里现算才不会过时。
//!      找不到时发 `Error`（`not_found`）后退出：显卡不在了，不是显卡出错。
//!   2. 加载运行库后核对：可见的 GPU 只有一块，描述与 PCI 号相符；把它的设备句柄同时交给主模型与 mmproj
//!      （`rummage_llama::Embedder`）。不符（两次枚举之间顺序变了）时发 `Error`（`mismatch`），父进程换一个
//!      推理进程重试一次。
//! - 不写 core.log、不碰核心进程的管道；出错时先发一帧 `Error` 再以非零状态退出，详细日志在 stderr。
//!
//! LiteRT（`--litert <cpu|gpu|npu>`，另给 `--tokenizer`，可选 `--cache-dir`，NPU 还要 `--dispatch-dir`）：不碰 Vulkan，
//! 用 `litert::LiteRt` 加载模型后发 `Ready`（设备名称为后端名，描述为处理器型号），收发与 llama.cpp 相同的帧；
//! 模型只有一个文件，`--mmproj` 不用。GPU、NPU 只在 Android 上可用（`rummage_litert`）。
//!
//! 列出设备模式（`--list-devices`）：不加载模型，输出每块 GPU 的名称、描述、PCI 号与显存。
//! 加载 ggml-vulkan 只创建 Vulkan 实例、查询物理设备属性，不创建逻辑设备，不在 GPU 上计算；默认只列出独显和核显，
//! 并按 UUID / LUID 去重（b11461 `ggml_vk_instance_init`）。

use std::ffi::{CString, OsString};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use rummage_llama::vulkan::{self, PhysicalDevice};
use rummage_llama::{Device, DeviceKind, Embedder, Part};

use rummage_media::Rgb;
use rummage_media::video::VideoPart;

use crate::frame::{self, DeviceInfo, FromWorker, GpuInfo, MediaPart, ToWorker};
use crate::litert::{Accel, LiteRt, LiteRtSpec};
use crate::local::VK_VISIBLE_ENV;
use crate::{Error, platform};

/// 推理子进程用 LiteRT 时的参数（`--litert` 等）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiteRtArgs {
    pub accel: Accel,
    pub tokenizer: PathBuf,
    pub cache_dir: Option<PathBuf>,
}

// 每个推理进程只解析一次，不必为变体大小装箱
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerArgs {
    Embed {
        runtime_dir: PathBuf,
        model: PathBuf,
        mmproj: Option<PathBuf>,
        threads: i32,
        /// 要使用的 GPU 的 ggml 设备描述；为空时只用 CPU。
        gpu: Option<String>,
        /// 该 GPU 的 PCI 总线号；后端不提供时为空。
        gpu_pci: Option<String>,
        /// 为空时用 llama.cpp
        litert: Option<LiteRtArgs>,
    },
    ListDevices {
        runtime_dir: PathBuf,
    },
}

impl WorkerArgs {
    /// 解析 `worker` 子命令的参数（`worker` 本身已去掉）；格式与 `local` 中拼出的参数对应
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut args = args.into_iter();
        let mut runtime_dir = None;
        let mut model = None;
        let mut mmproj = None;
        let mut threads = None;
        let mut gpu = None;
        let mut gpu_pci = None;
        let mut litert = None;
        let mut tokenizer = None;
        let mut cache_dir = None;
        let mut dispatch_dir = None;
        let mut list_devices = false;
        while let Some(arg) = args.next() {
            let flag = arg.to_string_lossy().into_owned();
            if flag == "--list-devices" {
                list_devices = true;
                continue;
            }
            let mut value = || args.next().ok_or_else(|| format!("{flag} 后面缺少值。"));
            match flag.as_str() {
                "--runtime-dir" => runtime_dir = Some(PathBuf::from(value()?)),
                "--model" => model = Some(PathBuf::from(value()?)),
                "--mmproj" => mmproj = Some(PathBuf::from(value()?)),
                "--threads" => {
                    let v = value()?;
                    let n = v
                        .to_str()
                        .and_then(|v| v.parse::<i32>().ok())
                        .filter(|&n| n > 0);
                    threads = Some(n.ok_or("--threads 应为正整数。")?);
                }
                "--gpu" => gpu = Some(value()?.to_string_lossy().into_owned()),
                "--gpu-pci" => gpu_pci = Some(value()?.to_string_lossy().into_owned()),
                "--litert" => litert = Some(value()?.to_string_lossy().into_owned()),
                "--tokenizer" => tokenizer = Some(PathBuf::from(value()?)),
                "--cache-dir" => cache_dir = Some(PathBuf::from(value()?)),
                "--dispatch-dir" => dispatch_dir = Some(PathBuf::from(value()?)),
                _ => return Err(format!("无法识别的参数：{flag}。")),
            }
        }
        let runtime_dir = runtime_dir.ok_or("缺少 --runtime-dir。")?;
        if list_devices {
            return Ok(WorkerArgs::ListDevices { runtime_dir });
        }
        let litert = match litert.as_deref() {
            None => None,
            Some(kind) => {
                let accel = match kind {
                    "cpu" => Accel::Cpu,
                    "gpu" => Accel::Gpu,
                    "npu" => Accel::Npu(dispatch_dir.ok_or("--litert npu 需要 --dispatch-dir。")?),
                    other => {
                        return Err(format!("--litert 只能是 cpu、gpu 或 npu，不是 {other}。"));
                    }
                };
                Some(LiteRtArgs {
                    accel,
                    tokenizer: tokenizer.ok_or("--litert 需要 --tokenizer。")?,
                    cache_dir,
                })
            }
        };
        Ok(WorkerArgs::Embed {
            runtime_dir,
            model: model.ok_or("缺少 --model。")?,
            mmproj,
            threads: threads.ok_or("缺少 --threads。")?,
            gpu,
            gpu_pci,
            litert,
        })
    }
}

/// 作为推理子进程运行：协议走 stdin 与受保护的 stdout。返回进程退出码
pub fn run_worker_stdio(args: WorkerArgs) -> i32 {
    let mut out = match platform::protocol_stdout() {
        Ok(out) => out,
        Err(err) => {
            eprintln!("无法准备协议输出：{err}");
            return 1;
        }
    };
    run_worker(args, None, &mut io::stdin().lock(), &mut out)
}

/// 从 `inp` 读请求、向 `out` 写回复，直到 `inp` 关闭；返回退出码。
/// `abort` 置位时正在进行的加载或推理尽快停下（`rummage_llama::Embedder::new`），推理在应用进程内运行时用；子进程传 `None`
pub fn run_worker(
    args: WorkerArgs,
    abort: Option<Arc<AtomicBool>>,
    inp: &mut impl Read,
    out: &mut impl Write,
) -> i32 {
    let res = wait_start(inp)
        .map_err(Failure::from)
        .and_then(|()| match &args {
            WorkerArgs::Embed {
                runtime_dir,
                model,
                threads,
                litert: Some(litert),
                ..
            } => serve_litert(inp, out, runtime_dir, model, *threads, litert),
            WorkerArgs::Embed {
                runtime_dir,
                model,
                mmproj,
                threads,
                gpu,
                gpu_pci,
                litert: None,
            } => serve(
                inp,
                out,
                abort.clone(),
                runtime_dir,
                model,
                mmproj.as_deref(),
                *threads,
                gpu.as_deref(),
                gpu_pci.as_deref(),
            ),
            WorkerArgs::ListDevices { runtime_dir } => {
                list(out, runtime_dir).map_err(Failure::from)
            }
        });
    let Err(err) = res else {
        return 0;
    };
    eprintln!("{}", err.message);
    // 父进程可能已经断开，这时只能靠 stderr 与退出码。
    let _ = frame::write(
        out,
        &FromWorker::Error {
            message: err.message,
            input: false,
            not_found: err.not_found,
            mismatch: err.mismatch,
            too_long: false,
        },
        &[],
    );
    1
}

/// 出错退出的原因；`not_found`、`mismatch` 见 `FromWorker::Error`
#[derive(Debug, Default)]
struct Failure {
    message: String,
    not_found: bool,
    mismatch: bool,
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Failure {
            message,
            ..Failure::default()
        }
    }
}

impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

fn io_err(err: io::Error) -> String {
    format!("与父进程通信失败：{err}")
}

/// 等父进程把本进程放进 Job Object 后发来的 `Start`。
fn wait_start(inp: &mut impl Read) -> Result<(), String> {
    match frame::read::<ToWorker>(inp).map_err(io_err)? {
        Some((ToWorker::Start, _)) => Ok(()),
        Some((other, _)) => Err(format!("应先收到 Start，实际收到 {other:?}。")),
        None => Err("父进程没有发来 Start 就关闭了输入。".to_string()),
    }
}

fn is_gpu(kind: DeviceKind) -> bool {
    matches!(kind, DeviceKind::Gpu | DeviceKind::Igpu)
}

#[allow(clippy::too_many_arguments)]
fn serve(
    inp: &mut impl Read,
    out: &mut impl Write,
    abort: Option<Arc<AtomicBool>>,
    runtime_dir: &Path,
    model: &Path,
    mmproj: Option<&Path>,
    threads: i32,
    gpu: Option<&str>,
    gpu_pci: Option<&str>,
) -> Result<(), Failure> {
    if let Some(want) = gpu {
        let raw = vulkan::physical_devices().map_err(|err| err.to_string())?;
        let k = raw_index(&raw, want, gpu_pci)?;
        let names: Vec<&str> = raw.iter().map(|d| d.name.as_str()).collect();
        eprintln!(
            "{want} 的 Vulkan 原始序号为 {k}（物理设备：{}）",
            names.join("、")
        );
        let value = CString::new(k.to_string()).expect("数字不含空字符");
        platform::set_runtime_env(VK_VISIBLE_ENV, &value)
            .map_err(|err| format!("无法设置 GGML_VK_VISIBLE_DEVICES：{err}"))?;
    }
    let rt = platform::load_runtime(runtime_dir).map_err(|err| err.to_string())?;
    let used = match gpu {
        Some(want) => verify(rt.devices(), want, gpu_pci)?,
        None => rt
            .devices()
            .iter()
            .find(|d| d.kind == DeviceKind::Cpu)
            .ok_or("运行库中没有 CPU 设备。")?,
    };
    let ready = FromWorker::Ready {
        device: DeviceInfo {
            name: used.name.clone(),
            description: used.description.clone(),
        },
    };
    eprintln!(
        "使用设备 {}（{}，PCI {}）",
        used.name,
        used.description,
        used.pci_id.as_deref().unwrap_or("无")
    );
    let device = gpu.map(|_| used.index);
    // Flash Attention 显式设置：CPU 关闭，打开会把 K、V 转成 F16，与黄金向量（按关闭计算）的余弦下降；
    // GPU 打开，不打开时 1120 档的注意力矩阵超过 4 GiB、Vulkan 静默算错，且更慢、更占显存
    let flash_attn = device.is_some();
    let mut emb = Embedder::new(&rt, model, mmproj, threads, device, flash_attn, abort)
        .map_err(|err| err.to_string())?;

    frame::write(out, &ready, &[]).map_err(io_err)?;
    while let Some((req, payload)) = frame::read::<ToWorker>(inp).map_err(io_err)? {
        let vector = |v: Vec<f32>| (FromWorker::Vector, frame::f32_bytes(&v));
        let res = match &req {
            ToWorker::Start => return Err("重复收到 Start。".into()),
            ToWorker::Text { text } => emb.embed(&[Part::Text(text)]).map(vector),
            ToWorker::Count { text } => emb
                .count_tokens(text)
                .map(|tokens| (FromWorker::Counted { tokens }, Vec::new())),
            ToWorker::Image { width, height, .. } => emb
                .embed(&[Part::Image {
                    width: *width,
                    height: *height,
                    rgb: &payload,
                }])
                .map(vector),
            ToWorker::Audio => match frame::f32_from_bytes(&payload) {
                Some(samples) => emb.embed(&[Part::Audio(&samples)]).map(vector),
                None => Err(rummage_llama::Error::InvalidInput(
                    "音频数据的字节数不是 4 的倍数".to_string(),
                )),
            },
            ToWorker::Video {
                parts, max_tokens, ..
            } => match split_media(parts, &payload) {
                Ok(owned) => {
                    let parts: Vec<Part> = owned.iter().map(Owned::part).collect();
                    emb.embed_limited(&parts, *max_tokens).map(vector)
                }
                Err(msg) => Err(rummage_llama::Error::InvalidInput(msg)),
            },
        };
        match res {
            Ok((msg, bytes)) => frame::write(out, &msg, &bytes),
            Err(err) => {
                let too_long = matches!(err, rummage_llama::Error::TooManyTokens { .. });
                let input = too_long
                    || matches!(
                        err,
                        rummage_llama::Error::InvalidInput(_) | rummage_llama::Error::NoMmproj
                    );
                let msg = FromWorker::Error {
                    message: err.to_string(),
                    input,
                    not_found: false,
                    mismatch: false,
                    too_long,
                };
                frame::write(out, &msg, &[])
            }
        }
        .map_err(io_err)?;
    }
    Ok(())
}

/// 用 LiteRT 处理请求，帧与 `serve` 相同（见模块说明）
fn serve_litert(
    inp: &mut impl Read,
    out: &mut impl Write,
    runtime_dir: &Path,
    model: &Path,
    threads: i32,
    args: &LiteRtArgs,
) -> Result<(), Failure> {
    if let Accel::Npu(dir) = &args.accel {
        platform::preload_npu(dir);
    }
    // 引擎不会自己创建缓存目录
    if let Some(dir) = &args.cache_dir {
        std::fs::create_dir_all(dir)
            .map_err(|err| format!("无法创建缓存目录 \"{}\"：{err}", dir.display()))?;
    }
    let mut rt = LiteRt::new(&LiteRtSpec {
        runtime_dir,
        model,
        tokenizer: &args.tokenizer,
        threads: threads as u32,
        accel: &args.accel,
        cache_dir: args.cache_dir.as_deref(),
    })
    .map_err(|err| err.to_string())?;
    let ready = FromWorker::Ready {
        device: DeviceInfo {
            name: args.accel.as_str().to_uppercase(),
            description: crate::cpu_name().to_string(),
        },
    };
    eprintln!("LiteRT 使用 {} 后端", args.accel.as_str());
    frame::write(out, &ready, &[]).map_err(io_err)?;
    while let Some((req, payload)) = frame::read::<ToWorker>(inp).map_err(io_err)? {
        let vector = |v: Vec<f32>| (FromWorker::Vector, frame::f32_bytes(&v));
        let res = match &req {
            ToWorker::Start => return Err("重复收到 Start。".into()),
            ToWorker::Text { text } => rt.text(text).map(vector),
            ToWorker::Count { text } => rt
                .count(text)
                .map(|tokens| (FromWorker::Counted { tokens }, Vec::new())),
            ToWorker::Image {
                width,
                height,
                tokens,
            } => rgb(*width, *height, payload)
                .and_then(|img| rt.image(&img, *tokens))
                .map(vector),
            ToWorker::Audio => match frame::f32_from_bytes(&payload) {
                Some(samples) => rt.audio(&samples).map(vector),
                None => Err(Error::InvalidInput(
                    "音频数据的字节数不是 4 的倍数".to_string(),
                )),
            },
            ToWorker::Video {
                parts,
                max_tokens,
                min_tier,
            } => split_media(parts, &payload)
                .map_err(Error::InvalidInput)
                .and_then(|owned| owned.into_iter().map(Owned::video_part).collect())
                .and_then(|parts: Vec<VideoPart>| rt.interleaved(&parts, *max_tokens, *min_tier))
                .map(vector),
        };
        match res {
            Ok((msg, bytes)) => frame::write(out, &msg, &bytes),
            Err(err) => {
                let too_long = matches!(err, Error::TooLong(_));
                let input = too_long || matches!(err, Error::InvalidInput(_));
                let message = match err {
                    Error::InvalidInput(m) | Error::TooLong(m) | Error::Inference(m) => m,
                    err => err.to_string(),
                };
                let msg = FromWorker::Error {
                    message,
                    input,
                    not_found: false,
                    mismatch: false,
                    too_long,
                };
                frame::write(out, &msg, &[])
            }
        }
        .map_err(io_err)?;
    }
    Ok(())
}

/// 负载按行存放的 RGB 像素；字节数须与尺寸相符
fn rgb(width: u32, height: u32, data: Vec<u8>) -> Result<Rgb, Error> {
    if data.len() as u64 != u64::from(width) * u64::from(height) * 3 {
        return Err(Error::InvalidInput("图像数据的长度与尺寸不符".to_string()));
    }
    Ok(Rgb {
        width,
        height,
        data,
    })
}

/// 视频窗口中按负载切出的一段
enum Owned<'a> {
    Text(&'a str),
    Image {
        width: u32,
        height: u32,
        rgb: &'a [u8],
    },
    Audio(Vec<f32>),
}

impl Owned<'_> {
    fn video_part(self) -> Result<VideoPart, Error> {
        Ok(match self {
            Owned::Text(t) => VideoPart::Text(t.to_string()),
            Owned::Image { width, height, rgb } => {
                VideoPart::Frame(Arc::new(self::rgb(width, height, rgb.to_vec())?))
            }
            Owned::Audio(a) => VideoPart::Audio(a),
        })
    }

    fn part(&self) -> Part<'_> {
        match self {
            Owned::Text(t) => Part::Text(t),
            Owned::Image { width, height, rgb } => Part::Image {
                width: *width,
                height: *height,
                rgb,
            },
            Owned::Audio(a) => Part::Audio(a),
        }
    }
}

/// 按 `parts` 中的长度把负载切成各段；长度对不上时返回原因
fn split_media<'a>(parts: &'a [MediaPart], payload: &'a [u8]) -> Result<Vec<Owned<'a>>, String> {
    let mut rest = payload;
    let mut take = |n: usize| -> Result<&'a [u8], String> {
        if n > rest.len() {
            return Err("视频窗口的数据长度与说明不符".to_string());
        }
        let (head, tail) = rest.split_at(n);
        rest = tail;
        Ok(head)
    };
    let mut out = Vec::with_capacity(parts.len());
    for p in parts {
        out.push(match p {
            MediaPart::Text { text } => Owned::Text(text),
            MediaPart::Image { width, height } => Owned::Image {
                width: *width,
                height: *height,
                rgb: take(
                    (*width as usize)
                        .saturating_mul(*height as usize)
                        .saturating_mul(3),
                )?,
            },
            MediaPart::Audio { samples } => Owned::Audio(
                frame::f32_from_bytes(take(samples.saturating_mul(4))?).expect("长度是 4 的倍数"),
            ),
        });
    }
    if !rest.is_empty() {
        return Err("视频窗口的数据长度与说明不符".to_string());
    }
    Ok(out)
}

fn pci_text(pci: Option<&str>) -> &str {
    pci.unwrap_or("无")
}

/// 按描述与 PCI 号（没有 PCI 号的只按描述）在原始顺序的物理设备中找到要用的 GPU，须唯一（见模块说明）。
fn raw_index(devices: &[PhysicalDevice], want: &str, pci: Option<&str>) -> Result<u32, Failure> {
    let same: Vec<usize> = (0..devices.len())
        .filter(|&i| devices[i].name == want && devices[i].pci_id.as_deref() == pci)
        .collect();
    // 同一块卡重复出现（deviceUUID 相同，例如驱动注册了两次）时取第一个：ggml 默认列表去重时，
    // 驱动优先级相同就保留先出现的那个（b11461 `ggml_vk_instance_init`）。
    let one_gpu = same
        .first()
        .and_then(|&i| devices[i].uuid)
        .is_some_and(|u| same.iter().all(|&i| devices[i].uuid == Some(u)));
    match same[..] {
        [i, ..] if one_gpu => Ok(i as u32),
        [i] => Ok(i as u32),
        [] => {
            let seen: Vec<String> = devices
                .iter()
                .map(|d| format!("{}（PCI {}）", d.name, pci_text(d.pci_id.as_deref())))
                .collect();
            Err(Failure {
                message: format!(
                    "找不到 {want}（PCI {}），当前的 Vulkan 设备：{}。",
                    pci_text(pci),
                    if seen.is_empty() {
                        "无".to_string()
                    } else {
                        seen.join("、")
                    }
                ),
                not_found: true,
                ..Failure::default()
            })
        }
        _ => Err(format!(
            "有 {} 块不同的 Vulkan 设备都是 {want}，无法确定用哪一块。",
            same.len()
        )
        .into()),
    }
}

/// 加载运行库后核对：可见的 GPU 只有一块，且就是要用的那块（见模块说明）。
fn verify<'a>(devices: &'a [Device], want: &str, pci: Option<&str>) -> Result<&'a Device, Failure> {
    let gpus: Vec<&Device> = devices.iter().filter(|d| is_gpu(d.kind)).collect();
    if let [d] = gpus[..]
        && d.description == want
        && d.pci_id.as_deref() == pci
    {
        return Ok(d);
    }
    let seen: Vec<String> = gpus
        .iter()
        .map(|d| format!("{}（PCI {}）", d.description, pci_text(d.pci_id.as_deref())))
        .collect();
    Err(Failure {
        message: format!(
            "可见的 GPU 是 {}，与要求的 {want}（PCI {}）不符。",
            if seen.is_empty() {
                "无".to_string()
            } else {
                seen.join("、")
            },
            pci_text(pci)
        ),
        mismatch: true,
        ..Failure::default()
    })
}

fn list(out: &mut impl Write, runtime_dir: &Path) -> Result<(), String> {
    let rt = platform::load_runtime(runtime_dir).map_err(|err| err.to_string())?;
    let gpus: Vec<GpuInfo> = rt
        .devices()
        .iter()
        .filter(|d| is_gpu(d.kind))
        .map(|d| GpuInfo {
            ggml_index: d.index,
            name: d.name.clone(),
            description: d.description.clone(),
            pci_id: d.pci_id.clone(),
            memory_total: d.memory_total,
            integrated: d.kind == DeviceKind::Igpu,
        })
        .collect();
    frame::write(out, &FromWorker::Devices { devices: gpus }, &[]).map_err(io_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(index: usize, kind: DeviceKind, description: &str, pci: Option<&str>) -> Device {
        Device {
            index,
            name: format!("Vulkan{index}"),
            description: description.to_string(),
            kind,
            pci_id: pci.map(str::to_string),
            memory_total: 0,
        }
    }

    const NV: &str = "NVIDIA GeForce RTX 4080 SUPER";
    const IGPU: &str = "Intel(R) Iris(R) Xe Graphics";

    fn raw(name: &str, pci: Option<&str>) -> PhysicalDevice {
        PhysicalDevice {
            name: name.to_string(),
            pci_id: pci.map(str::to_string),
            uuid: Some([name.len() as u8; 16]),
        }
    }

    fn parse(args: &[&str]) -> Result<WorkerArgs, String> {
        WorkerArgs::parse(args.iter().map(OsString::from))
    }

    #[test]
    fn parse_args() {
        assert_eq!(
            parse(&["--runtime-dir", "rt", "--model", "m", "--threads", "6"]),
            Ok(WorkerArgs::Embed {
                runtime_dir: PathBuf::from("rt"),
                model: PathBuf::from("m"),
                mmproj: None,
                threads: 6,
                gpu: None,
                gpu_pci: None,
                litert: None,
            })
        );
        assert_eq!(
            parse(&[
                "--runtime-dir",
                "rt",
                "--model",
                "m",
                "--mmproj",
                "p",
                "--threads",
                "2",
                "--gpu",
                NV,
                "--gpu-pci",
                "0000:06:00.0",
            ]),
            Ok(WorkerArgs::Embed {
                runtime_dir: PathBuf::from("rt"),
                model: PathBuf::from("m"),
                mmproj: Some(PathBuf::from("p")),
                threads: 2,
                gpu: Some(NV.to_string()),
                gpu_pci: Some("0000:06:00.0".to_string()),
                litert: None,
            })
        );
        assert_eq!(
            parse(&["--list-devices", "--runtime-dir", "rt"]),
            Ok(WorkerArgs::ListDevices {
                runtime_dir: PathBuf::from("rt"),
            })
        );
        assert!(parse(&["--runtime-dir", "rt", "--threads", "6"]).is_err());
        assert!(parse(&["--runtime-dir", "rt", "--model", "m", "--threads", "0"]).is_err());
        assert!(
            parse(&[
                "--runtime-dir",
                "rt",
                "--model",
                "m",
                "--threads",
                "6",
                "--device",
                "x"
            ])
            .is_err()
        );
        assert!(parse(&["--model", "m", "--threads", "6"]).is_err());
        assert!(parse(&["--runtime-dir"]).is_err());
        assert!(parse(&["--runtime-dir", "rt", "--verbose"]).is_err());
    }

    /// 在内存中的输入输出上运行：先等 `Start`，运行库加载失败时回一帧 `Error` 并以 1 结束
    #[test]
    fn run_worker_on_any_stream() {
        let dir = std::env::temp_dir().join("rummage-no-such-runtime");
        let args = WorkerArgs::ListDevices {
            runtime_dir: dir.clone(),
        };
        let mut inp = Vec::new();
        frame::write(&mut inp, &ToWorker::Start, &[]).unwrap();
        let mut out = Vec::new();
        assert_eq!(
            run_worker(args.clone(), None, &mut inp.as_slice(), &mut out),
            1
        );
        let Some((FromWorker::Error { message, .. }, _)) =
            frame::read::<FromWorker>(&mut out.as_slice()).unwrap()
        else {
            panic!("应回一帧 Error");
        };
        assert!(message.contains("rummage-no-such-runtime"), "{message}");

        // 没有收到 Start 就关闭了输入：不加载运行库
        let mut out = Vec::new();
        assert_eq!(run_worker(args, None, &mut io::empty(), &mut out), 1);
        let Some((FromWorker::Error { message, .. }, _)) =
            frame::read::<FromWorker>(&mut out.as_slice()).unwrap()
        else {
            panic!("应回一帧 Error");
        };
        assert!(message.contains("Start"), "{message}");
    }

    #[test]
    fn raw_index_by_pci_and_name() {
        // 原始顺序中夹着不参与 ggml 默认列表的设备（虚拟适配器等），顺序与自检时也不同
        let devices = [
            raw("Microsoft Basic Render Driver", None),
            raw(IGPU, None),
            raw(NV, Some("0000:06:00.0")),
        ];
        assert_eq!(raw_index(&devices, NV, Some("0000:06:00.0")).unwrap(), 2);
        assert_eq!(raw_index(&devices, IGPU, None).unwrap(), 1);
        // 同型号、另一个插槽的卡不算；不见了时标为 not_found
        let err = raw_index(&devices, NV, Some("0000:01:00.0")).unwrap_err();
        assert!(err.not_found && !err.mismatch, "{}", err.message);
        let err = raw_index(&devices[..2], NV, Some("0000:06:00.0")).unwrap_err();
        assert!(
            err.not_found && err.message.contains(IGPU),
            "{}",
            err.message
        );
        // 同一块卡出现两次（i7-12700H 的核显即如此）：取第一个
        let twice = [
            raw(NV, Some("0000:06:00.0")),
            raw(IGPU, None),
            raw(IGPU, None),
        ];
        assert_eq!(raw_index(&twice, IGPU, None).unwrap(), 1);
        // 两块不同的卡（UUID 不同）无法区分：不是“找不到”
        let mut other = raw(IGPU, None);
        other.uuid = Some([9; 16]);
        let twins = [raw(IGPU, None), other];
        assert!(!raw_index(&twins, IGPU, None).unwrap_err().not_found);
        let mut old = [raw(IGPU, None), raw(IGPU, None)];
        old[1].uuid = None;
        assert!(raw_index(&old, IGPU, None).is_err());
    }

    #[test]
    fn verify_single_visible_gpu() {
        let nv = dev(0, DeviceKind::Gpu, NV, Some("0000:06:00.0"));
        let cpu = dev(1, DeviceKind::Cpu, "CPU", None);
        let igpu = dev(1, DeviceKind::Igpu, IGPU, None);
        let ok = [nv.clone(), cpu];
        assert_eq!(verify(&ok, NV, Some("0000:06:00.0")).unwrap().index, 0);
        // 看到的是别的卡、不止一块或一块都没有：都是 mismatch，父进程重试
        for devices in [vec![igpu.clone()], vec![nv.clone(), igpu.clone()], vec![]] {
            let err = verify(&devices, NV, Some("0000:06:00.0")).unwrap_err();
            assert!(err.mismatch && !err.not_found, "{}", err.message);
        }
        assert!(
            verify(&[nv], NV, Some("0000:01:00.0"))
                .unwrap_err()
                .mismatch
        );
        assert!(verify(&[igpu], IGPU, None).is_ok());
    }
}
