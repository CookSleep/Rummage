//! 开发用的子命令：`selftest`（推理后端自检）、`eval`（评测）、`bench`（性能测试），见 main 的模块说明。
//! 输出用 `emitln!`，读取方关闭管道时不 panic
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use flexi_logger::{LogSpecification, Logger};
use rummage_embed::selftest::{self, Candidate, Modality, SelftestReport, Status};
use rummage_embed::space::{MMPROJ, MODEL, MODEL_DIR};
use rummage_embed::{Backend, Limits};
use rummage_proto::{ParamValue, params};

use crate::{bench, eval, ffmpeg_dir, golden_dir, runtime_dirs};

pub const COMMANDS: [&str; 3] = ["selftest", "eval", "bench"];

/// `cmd` 为 `COMMANDS` 之一，`raw` 为其后的参数
pub fn run(cmd: &str, raw: impl IntoIterator<Item = OsString>) -> ExitCode {
    match cmd {
        "selftest" => match parse_selftest_args(raw) {
            Ok(args) => run_selftest(args),
            Err(msg) => {
                eprintln!("{msg}");
                ExitCode::FAILURE
            }
        },
        "eval" => run_dev(|| {
            let (data_dir, args) = parse_eval_args(raw)?;
            eval::run(&dev_env(data_dir)?, &args)
        }),
        _ => run_dev(|| {
            let (data_dir, args) = parse_bench_args(raw)?;
            bench::run(&dev_env(data_dir)?, &args)
        }),
    }
}

/// `eval`、`bench` 的环境：运行库与黄金样本同核心，数据目录默认为开发版的 `src-tauri/dev-data`
fn dev_env(data_dir: Option<PathBuf>) -> Result<eval::Env, String> {
    let exe =
        std::env::current_exe().map_err(|err| format!("无法确定 rummage-core 的位置：{err}"))?;
    let (cpu_runtime, vulkan_runtime) = runtime_dirs(&exe);
    Ok(eval::Env {
        golden: golden_dir(&exe),
        ffmpeg: ffmpeg_dir(&exe),
        exe,
        cpu_runtime,
        vulkan_runtime,
        data_dir: data_dir.unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/dev-data")
        }),
        // 由 scripts/fetch-litert.ps1 取出，只用于评测
        litert_runtime: Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.cache/third-party/litert-lm/0.18.0/windows_x86_64"),
    })
}

/// `eval` 与 `bench` 的公共参数，以及各自的其他参数（`--名称 值` 的列表）
struct DevArgs {
    data_dir: Option<PathBuf>,
    device: eval::Device,
    work: PathBuf,
    positional: Vec<OsString>,
    rest: Vec<(String, OsString)>,
}

fn parse_dev_args(cmd: &str, args: impl IntoIterator<Item = OsString>) -> Result<DevArgs, String> {
    let mut args = args.into_iter();
    let mut parsed = DevArgs {
        data_dir: None,
        device: eval::Device::Auto,
        work: std::env::temp_dir().join(format!("rummage-{cmd}-{}", std::process::id())),
        positional: Vec::new(),
        rest: Vec::new(),
    };
    while let Some(arg) = args.next() {
        let flag = arg.to_string_lossy().into_owned();
        if !flag.starts_with("--") {
            parsed.positional.push(arg);
            continue;
        }
        let value = args.next().ok_or_else(|| format!("{flag} 后面缺少值。"))?;
        match flag.as_str() {
            "--data-dir" => parsed.data_dir = Some(PathBuf::from(value)),
            "--work-dir" => parsed.work = PathBuf::from(value),
            "--device" => parsed.device = eval::Device::parse(&value.to_string_lossy()),
            _ => parsed.rest.push((flag, value)),
        }
    }
    Ok(parsed)
}

/// 返回 `--data-dir` 与 `eval` 的参数
fn parse_eval_args(
    args: impl IntoIterator<Item = OsString>,
) -> Result<(Option<PathBuf>, eval::Args), String> {
    let d = parse_dev_args("eval", args)?;
    let mut out = None;
    let mut subsets = Vec::new();
    let mut values = Vec::new();
    let mut litert = false;
    for (flag, value) in d.rest {
        let value = value.to_string_lossy().into_owned();
        match flag.as_str() {
            "--out" => out = Some(PathBuf::from(value)),
            "--subset" => subsets.push(value),
            "--param" => values.push(parse_param(&value)?),
            "--runtime" => {
                litert = match value.as_str() {
                    "llama" => false,
                    "litert" => true,
                    _ => return Err(format!("--runtime 只能是 llama 或 litert，不是 {value}。")),
                }
            }
            _ => return Err(format!("无法识别的参数：{flag}。")),
        }
    }
    let [set]: [OsString; 1] = d.positional.try_into().map_err(|_| {
        "用法：rummage-core eval <评测集目录> [--out <CSV>] [--subset <子集>]… [--param <名称>=<值>]… [--device cpu|<显卡名称>] [--runtime llama|litert] [--work-dir <目录>] [--data-dir <目录>]"
    })?;
    let args = eval::Args {
        set: PathBuf::from(set),
        out,
        device: d.device,
        work: d.work,
        subsets,
        params: values,
        litert,
    };
    Ok((d.data_dir, args))
}

/// `名称=值`：值为 true / false、整数或小数，按参数表校验
fn parse_param(s: &str) -> Result<(String, ParamValue), String> {
    let (key, value) = s
        .split_once('=')
        .ok_or_else(|| format!("--param {s} 应写成 名称=值。"))?;
    let value = match value {
        "true" => ParamValue::Bool(true),
        "false" => ParamValue::Bool(false),
        v => match (v.parse::<i64>(), v.parse::<f64>()) {
            (Ok(n), _) => ParamValue::Int(n),
            (_, Ok(f)) => ParamValue::Float(f),
            _ => return Err(format!("--param {s} 的值无法识别。")),
        },
    };
    let value = params::validate(key, &value).map_err(|err| err.to_string())?;
    Ok((key.to_string(), value))
}

/// 返回 `--data-dir` 与 `bench` 的参数
fn parse_bench_args(
    args: impl IntoIterator<Item = OsString>,
) -> Result<(Option<PathBuf>, bench::Args), String> {
    let d = parse_dev_args("bench", args)?;
    if !d.positional.is_empty() {
        return Err("用法：rummage-core bench [--vectors <条数>] [--queries <条数>] [--samples <图片目录>] [--device cpu|<显卡名称>] [--work-dir <目录>] [--data-dir <目录>]".into());
    }
    let mut args = bench::Args {
        vectors: 300_000,
        queries: 100,
        samples: None,
        device: d.device,
        work: d.work,
    };
    for (flag, value) in d.rest {
        let count = || {
            value
                .to_str()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|&n| n > 0)
                .ok_or(format!("{flag} 应为正整数。"))
        };
        match flag.as_str() {
            "--vectors" => args.vectors = count()?,
            "--queries" => args.queries = count()?,
            "--samples" => args.samples = Some(PathBuf::from(&value)),
            _ => return Err(format!("无法识别的参数：{flag}。")),
        }
    }
    Ok((d.data_dir, args))
}

/// 运行 `eval` 或 `bench`：后端的警告输出到 stderr，出错时打印原因并以退出码 1 结束
fn run_dev(f: impl FnOnce() -> Result<(), String>) -> ExitCode {
    let _log = Logger::with(LogSpecification::warn())
        .log_to_stderr()
        .start()
        .ok();
    match f() {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, Default, PartialEq)]
struct SelftestArgs {
    data_dir: PathBuf,
    golden: PathBuf,
    runtime_dir: PathBuf,
    vulkan_dir: Option<PathBuf>,
    /// 默认为 `<数据目录>\models\embeddinggemma-2-gguf`
    model_dir: Option<PathBuf>,
    skip_gpu: bool,
    no_cache: bool,
}

/// `selftest` 子命令的参数，`selftest` 本身已去掉
fn parse_selftest_args(args: impl IntoIterator<Item = OsString>) -> Result<SelftestArgs, String> {
    let mut args = args.into_iter();
    let mut data_dir = None;
    let mut golden = None;
    let mut runtime_dir = None;
    let mut parsed = SelftestArgs::default();
    while let Some(arg) = args.next() {
        let flag = arg.to_string_lossy().into_owned();
        match flag.as_str() {
            "--skip-gpu" => {
                parsed.skip_gpu = true;
                continue;
            }
            "--no-cache" => {
                parsed.no_cache = true;
                continue;
            }
            _ => {}
        }
        let value = PathBuf::from(
            args.next()
                .ok_or_else(|| format!("{flag} 后面缺少路径。"))?,
        );
        match flag.as_str() {
            "--data-dir" => data_dir = Some(value),
            "--golden" => golden = Some(value),
            "--runtime-dir" => runtime_dir = Some(value),
            "--vulkan-dir" => parsed.vulkan_dir = Some(value),
            "--model-dir" => parsed.model_dir = Some(value),
            _ => return Err(format!("无法识别的参数：{flag}。")),
        }
    }
    parsed.data_dir = data_dir.ok_or("缺少 --data-dir。")?;
    parsed.golden = golden.ok_or("缺少 --golden。")?;
    parsed.runtime_dir = runtime_dir.ok_or("缺少 --runtime-dir。")?;
    if !parsed.skip_gpu && parsed.vulkan_dir.is_none() {
        return Err("缺少 --vulkan-dir；只测 CPU 时加 --skip-gpu。".into());
    }
    Ok(parsed)
}

fn print_report(report: &SelftestReport) {
    if report.from_cache {
        emitln!("推理后端自检：命中缓存，未重新测试。");
    } else {
        emitln!("推理后端自检：");
    }
    for c in &report.candidates {
        let status = match c.status {
            Status::Passed => "通过",
            Status::Failed => "未通过",
            Status::Blacklisted => "已屏蔽",
            Status::Skipped => "已跳过",
        };
        let backend = match c.backend {
            Backend::Cpu => "CPU",
            Backend::Vulkan => "Vulkan",
        };
        let mut extra = Vec::new();
        if let Some(d) = &c.driver {
            extra.push(format!("驱动 {d}"));
        }
        if let Some(p) = &c.pci_id {
            extra.push(format!("PCI {p}"));
        }
        let extra: String = extra.iter().map(|e| format!("，{e}")).collect();
        emitln!(
            "\n[{status}] {backend}：{}（{}{extra}）",
            c.name,
            c.description
        );
        let ms = |v: Option<f64>| v.map_or("—".to_string(), |v| format!("{v:.1} 毫秒"));
        emitln!(
            "  初始化 {}；单条耗时中位数：文本 {}，图像 {}，音频 {}，视频窗口 {}",
            ms(c.init_ms),
            ms(c.text_ms),
            ms(c.image_ms),
            ms(c.audio_ms),
            ms(c.video_ms)
        );
        for s in &c.samples {
            let kind = match s.modality {
                Modality::Text => "文本",
                Modality::Image => "图像",
                Modality::Audio => "音频",
                Modality::Video => "视频",
            };
            emitln!("  {:<20} {kind} 余弦 {:.9}", s.id, s.cosine);
        }
        if let Some(r) = &c.reason {
            emitln!("  原因：{r}");
        }
    }
    match report.selected() {
        Some(c) => emitln!("\n选中：{}（{}）", c.name, c.description),
        None => emitln!("\n没有可用的推理后端。"),
    }
}

fn run_selftest(args: SelftestArgs) -> ExitCode {
    // 后端的警告（推理进程的 stderr 末尾等）输出到 stderr
    let _log = Logger::with(LogSpecification::warn())
        .log_to_stderr()
        .start()
        .ok();
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            eprintln!("无法确定 rummage-core 的位置：{err}");
            return ExitCode::FAILURE;
        }
    };
    let model_dir = args
        .model_dir
        .unwrap_or_else(|| args.data_dir.join("models").join(MODEL_DIR));
    // 开发工具不读数据目录中的设置，用参数表的默认值
    let values = BTreeMap::new();
    let mut candidates = vec![Candidate::Cpu];
    let vulkan = if args.skip_gpu { None } else { args.vulkan_dir };
    if let Some(dir) = &vulkan {
        match rummage_embed::list_devices(&exe, dir, &Limits::from_params(&values)) {
            Ok(gpus) => candidates.extend(gpus.into_iter().map(Candidate::Gpu)),
            Err(err) => emitln!("无法列出显卡，只测试 CPU：{err}"),
        }
    }
    let opts = selftest::Options {
        exe,
        golden_dir: args.golden,
        cpu_runtime: args.runtime_dir,
        vulkan_runtime: vulkan,
        model: model_dir.join(MODEL.name),
        mmproj: model_dir.join(MMPROJ.name),
        cache_file: args.data_dir.join("cache").join("selftest.json"),
        use_cache: !args.no_cache,
        save: true,
        auto: false,
        cancel: Default::default(),
        images: true,
        only: None,
        video: rummage_core::pipeline::speed_video(&values),
    };
    match selftest::run(&opts, &candidates, &values, &[], &mut |_, _| {}) {
        Ok(report) => {
            print_report(&report);
            if report.selected.is_some() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}
