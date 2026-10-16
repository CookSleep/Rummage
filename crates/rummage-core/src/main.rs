//! 核心进程入口：只在带 `--data-dir` 时以核心模式运行（界面、命令行拉起核心都会带上）；
//! 不带任何参数或带 `-h` / `--help` 时打印命令行的用法。
//! 启动顺序：日志 → 降低进程优先级 → 创建第一个管道实例（当前用户的核心已在运行时以退出码 2 退出，管道名被别人占用时
//! 以退出码 3 退出）→ 写入新令牌 → 打开索引数据库并载入参数（失败时不退出）→ 启动索引流水线（先恢复上次中断的作业）
//! → 启动扫描线程并对全部根目录对账一次 → 启动暂停检测线程，监听睡眠与唤醒 → 接受连接。
//! 唤醒后验证推理引擎用的黄金样本：发布版在 `<exe 目录>\golden`，开发版用仓库的 `assets/golden`。
//! 核心要常驻：不读 stdin，stdin 关闭也不退出；只在 Ctrl+C 或收到 `Shutdown` 请求时正常退出：把处理中的作业放回待处理，
//! 删除 `core.token`。系统关机或注销时同样把处理中的作业放回待处理（`on_session_end`），不算崩溃。
//! 运行库：发布版在 `<exe 目录>\llama\cpu`、`<exe 目录>\llama\vulkan`；开发版（debug 构建）用仓库 `.cache` 中
//! 由 `scripts/fetch-*.ps1` 解压的官方预编译包。模型在 `<数据目录>\models\embeddinggemma-2-gguf\`（`space::MODEL_DIR`）。
//! ffmpeg / ffprobe：发布版在 `<exe 目录>\ffmpeg`，开发版用 `scripts/fetch-ffmpeg.ps1` 取到 `.cache` 中的文件（`ffmpeg_dir`）。
//! 模型下载用的 rustls 在启动时安装进程默认的 ring 加密实现。
//! `--shutdown`：作为客户端请同一数据目录的核心退出并等它结束（开发时重新构建前用），连不上也视为成功；
//! 不带 `--data-dir` 时用 `rummage_datadir::current` 读出当前指向的数据目录（不检查待完成的更改，也不迁移）
//! `worker …`：推理子进程（`rummage_embed::run_worker_stdio`），由核心进程的 `LocalBackend` 拉起；不写 core.log，也不碰管道
//! `selftest …`：开发用的推理后端自检（`dev` 模块）（`rummage_embed::selftest`），把报告打印到 stdout，有可用的后端时退出码为 0；
//! 不写 core.log，也不碰管道。参数用参数表的默认值；带 `--skip-gpu` 时只测 CPU，不加载 Vulkan 运行库
//! `search …`、`status`、`context …`、`folders …`、`settings …`、`pause`、`resume`、`failed …`：命令行（`cli` 模块），
//! 连接核心、未运行时拉起它；
//! `mcp`：以 stdio 运行 MCP 服务（`mcp` 模块）
//! `eval <评测集目录> …`、`bench …`：开发用的评测与性能测试（`eval`、`bench` 模块），在临时数据目录中无界面运行，
//! 结果打印到 stdout；模型与自检记录默认取开发版的数据目录 `src-tauri/dev-data`（`--data-dir` 可改）。
//! 不在安装目录中运行的发布版构建（`cargo run --release`，旁边没有 `llama\`）与开发版一样用仓库中的运行库与黄金样本。
#[macro_use]
mod cli;
mod bench;
mod dev;
mod eval;
mod manage;
mod mcp;

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::Ordering;

use flexi_logger::writers::{FileLogWriter, LogWriter};
use flexi_logger::{
    Cleanup, Criterion, DeferredNow, Duplicate, FileSpec, LogSpecification, Logger, LoggerHandle,
    Naming,
};
use rummage_core::client::{self, ConnectError};
use rummage_core::pipeline::Config;
use rummage_core::{BindError, Power};
use rummage_embed::WorkerArgs;
use rummage_embed::space::{MMPROJ, MODEL, MODEL_DIR};

/// 开发版或不在安装目录中运行（旁边没有 `llama\`），见模块说明
fn in_repo(exe: &Path) -> bool {
    cfg!(debug_assertions)
        || !exe
            .parent()
            .unwrap_or(Path::new("."))
            .join("llama")
            .is_dir()
}

/// 黄金样本目录（唤醒后验证推理引擎用）：发布版为 `<exe 目录>\golden`，开发版为仓库的 `assets/golden`
fn golden_dir(exe: &Path) -> PathBuf {
    if in_repo(exe) {
        return Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/golden");
    }
    exe.parent().unwrap_or(Path::new(".")).join("golden")
}

/// llama.cpp 运行库目录（CPU、Vulkan），见模块说明
fn runtime_dirs(exe: &Path) -> (PathBuf, PathBuf) {
    if in_repo(exe) {
        let base =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cache/third-party/llama.cpp/b11461");
        return (base.join("win-cpu-x64"), base.join("win-vulkan-x64"));
    }
    let dir = exe.parent().unwrap_or(Path::new(".")).join("llama");
    (dir.join("cpu"), dir.join("vulkan"))
}

/// ffmpeg.exe 与 ffprobe.exe 所在的目录，见模块说明
fn ffmpeg_dir(exe: &Path) -> PathBuf {
    if in_repo(exe) {
        return Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.cache/third-party/ffmpeg")
            .join(rummage_media::ffmpeg::FFMPEG_BUILD);
    }
    exe.parent().unwrap_or(Path::new(".")).join("ffmpeg")
}

#[derive(Debug, Default, PartialEq)]
struct Args {
    /// `--data-dir` 指定的目录
    data_dir: Option<PathBuf>,
    shutdown: bool,
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Args, String> {
    let mut args = args.into_iter();
    let mut parsed = Args::default();
    while let Some(arg) = args.next() {
        if arg == "--shutdown" {
            parsed.shutdown = true;
            continue;
        }
        if arg != "--data-dir" {
            return Err(format!("无法识别的参数：{}。", arg.to_string_lossy()));
        }
        let Some(path) = args.next() else {
            return Err("--data-dir 后面缺少路径。".into());
        };
        parsed.data_dir = Some(PathBuf::from(path));
    }
    Ok(parsed)
}

/// core.log 的写入端：参数 `write_log_file` 关闭时丢弃，stderr 的副本照常输出
struct GatedFile(FileLogWriter);

impl LogWriter for GatedFile {
    fn write(&self, now: &mut DeferredNow, record: &log::Record) -> std::io::Result<()> {
        if !rummage_core::LOG_TO_FILE.load(Ordering::Relaxed) {
            return Ok(());
        }
        self.0.write(now, record)
    }

    fn flush(&self) -> std::io::Result<()> {
        self.0.flush()
    }

    fn shutdown(&self) {
        self.0.shutdown()
    }
}

/// 与界面进程的 app.log 规则相同：警告和错误写入 `logs\core.log`，单文件约 5 MB，另外最多保留 2 个历史文件，
/// 同时输出到 stderr；日志文件不能追加写入时只输出到 stderr
fn init_log(data_dir: &Path) -> Option<LoggerHandle> {
    let dir = data_dir.join("logs");
    let file = fs::create_dir_all(&dir)
        .and_then(|()| {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("core.log"))
        })
        .and_then(|_| {
            FileLogWriter::builder(
                FileSpec::default()
                    .directory(&dir)
                    .basename("core")
                    .suppress_timestamp(),
            )
            .append()
            .rotate(
                Criterion::Size(5 * 1024 * 1024),
                Naming::NumbersDirect,
                Cleanup::KeepLogFiles(2),
            )
            .format(flexi_logger::opt_format)
            .try_build()
            .map_err(std::io::Error::other)
        });
    let logger = Logger::with(LogSpecification::warn()).format(flexi_logger::opt_format);
    let logger = match file {
        Ok(writer) => logger
            .log_to_writer(Box::new(GatedFile(writer)))
            .duplicate_to_stderr(Duplicate::Warn),
        Err(err) => {
            eprintln!("[log] 日志文件不可写，日志不会写入文件：{err}");
            logger.log_to_stderr()
        }
    };
    match logger.start() {
        Ok(handle) => Some(handle),
        Err(err) => {
            eprintln!("[log] 无法初始化日志：{err}");
            None
        }
    }
}

async fn run(data_dir: &Path) -> ExitCode {
    rummage_core::models::install_crypto();
    // 后台核心不与前台程序争抢 CPU；推理子进程随之继承（见 windows.rs 模块说明）
    if let Err(err) = rummage_core::lower_priority() {
        log::warn!("无法降低核心进程的优先级：{err}");
    }
    let listener = match rummage_core::pipe_name(data_dir) {
        Ok(name) => rummage_core::bind(&name).await,
        Err(err) => Err(BindError::Io(err)),
    };
    let listener = match listener {
        Ok(listener) => listener,
        Err(BindError::AlreadyRunning) => {
            eprintln!("Rummage 后台服务已在运行。");
            return ExitCode::from(rummage_core::EXIT_ALREADY_RUNNING);
        }
        Err(err @ BindError::Occupied) => {
            log::error!("{err}");
            eprintln!("{err}");
            return ExitCode::from(3);
        }
        Err(err) => {
            log::error!("{err}");
            return ExitCode::FAILURE;
        }
    };
    // 先占住管道再写令牌，第二个实例不会覆盖正在运行的核心的令牌
    let token_path = rummage_core::token_path(data_dir);
    let token = match rummage_core::new_token().and_then(|token| {
        rummage_core::write_private(&token_path, token.as_bytes()).map(|()| token)
    }) {
        Ok(token) => token,
        Err(err) => {
            log::error!("无法写入连接令牌 {}：{err}", token_path.display());
            return ExitCode::FAILURE;
        }
    };
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            log::warn!("无法监听 Ctrl+C：{err}");
            std::future::pending::<()>().await;
        }
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            log::error!("无法确定 rummage-core 的位置：{err}");
            return ExitCode::FAILURE;
        }
    };
    let (cpu_runtime, vulkan_runtime) = runtime_dirs(&exe);
    let golden = golden_dir(&exe);
    let models = data_dir.join("models").join(MODEL_DIR);
    let cfg = Config {
        ffmpeg: ffmpeg_dir(&exe),
        exe,
        cpu_runtime,
        vulkan_runtime,
        model: models.join(MODEL.name),
        mmproj: models.join(MMPROJ.name),
        selftest_cache: data_dir.join("cache").join("selftest.json"),
        golden: golden.clone(),
        embed_tmp: Config::embed_tmp_dir(data_dir),
        gpus: rummage_embed::selftest::gpu_adapters,
    };
    // 打开数据库在占住管道之后，第二个实例不会同时迁移同一个数据库
    let dir = data_dir.to_path_buf();
    let state =
        match tokio::task::spawn_blocking(move || rummage_core::start(token, &dir, cfg)).await {
            Ok(state) => state,
            Err(err) => {
                log::error!("初始化核心状态时出错：{err}");
                return ExitCode::FAILURE;
            }
        };
    // 睡眠前不再领取作业，唤醒后验证推理引擎再继续；验证可能要几秒，不占用窗口线程
    let s = state.clone();
    let on_power: Box<dyn Fn(Power) + Send + Sync> = Box::new(move |event| match event {
        Power::Suspend => s.suspend_indexing(),
        Power::Resume => {
            let (s, golden) = (s.clone(), golden.clone());
            let spawned = std::thread::Builder::new()
                .name("wake-check".into())
                .spawn(move || s.resume_indexing(&golden));
            if let Err(err) = spawned {
                log::error!("无法启动唤醒验证线程：{err}");
            }
        }
    });
    if let Err(err) = rummage_core::on_power(on_power) {
        log::warn!("无法监听睡眠与唤醒，唤醒后不会验证推理引擎：{err}");
    }
    // 关机或注销不算崩溃：处理中的作业放回待处理、不计失败
    let s = state.clone();
    if let Err(err) = rummage_core::on_session_end(Box::new(move || s.stop_pipeline())) {
        log::warn!("无法监听关机与注销，此时正在处理的文件会在下次启动时计一次失败：{err}");
    }
    let code = tokio::select! {
        res = rummage_core::serve(listener, state.clone()) => {
            let Err(err) = res;
            log::error!("IPC 服务停止：{err}");
            ExitCode::FAILURE
        }
        () = ctrl_c => ExitCode::SUCCESS,
        () = state.shutdown_requested() => ExitCode::SUCCESS,
    };
    // 放回处理中的作业并通知各线程退出（不等线程结束）；要等数据库锁，可能阻塞
    let s = state.clone();
    if let Err(err) = tokio::task::spawn_blocking(move || s.stop_pipeline()).await {
        log::error!("停止索引时出错：{err}");
    }
    match fs::remove_file(&token_path) {
        Err(err) if err.kind() != ErrorKind::NotFound => log::warn!("无法删除连接令牌：{err}"),
        _ => {}
    }
    code
}

/// 请核心退出并等它结束；核心没有运行或连不上都算成功，只有核心收到请求后 10 秒内没有退出才失败
async fn shutdown(data_dir: &Path) -> ExitCode {
    let connected = match (
        rummage_core::pipe_name(data_dir),
        rummage_core::read_token(data_dir),
    ) {
        (Ok(pipe), Ok(token)) => client::connect(&pipe, &token).await,
        _ => Err(ConnectError::NotRunning),
    };
    let client = match connected {
        Ok(client) => client,
        Err(ConnectError::NotRunning) => return ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::SUCCESS;
        }
    };
    match client::shutdown(client).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            eprintln!("Rummage 后台服务 10 秒内没有退出。");
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("无法让 Rummage 后台服务退出：{err}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let mut raw = std::env::args_os().skip(1).peekable();
    if raw.peek().is_some_and(|a| a == "worker") {
        raw.next();
        return match WorkerArgs::parse(raw) {
            Ok(args) => {
                ExitCode::from(u8::try_from(rummage_embed::run_worker_stdio(args)).unwrap_or(1))
            }
            Err(msg) => {
                eprintln!("{msg}");
                ExitCode::FAILURE
            }
        };
    }
    if let Some(cmd) = raw
        .peek()
        .and_then(|a| a.to_str())
        .filter(|a| dev::COMMANDS.contains(a))
    {
        let cmd = cmd.to_string();
        raw.next();
        return dev::run(&cmd, raw);
    }
    let cmd = raw.peek().map(|a| a.to_string_lossy().into_owned());
    match cmd.as_deref() {
        Some(c) if cli::COMMANDS.contains(&c) => {
            let c = c.to_string();
            raw.next();
            let raw: Vec<OsString> = raw.collect();
            if raw.iter().any(|a| a == "-h" || a == "--help") {
                let _ = cli::emit(&cli::usage());
                return ExitCode::SUCCESS;
            }
            let args = match cli::parse(&c, raw) {
                Ok(args) => args,
                Err(msg) => {
                    eprintln!("{msg}\n\n{}", cli::usage());
                    return cli::Outcome::Failed.into();
                }
            };
            return match tokio::runtime::Runtime::new() {
                Ok(runtime) => runtime.block_on(cli::run(args)).into(),
                Err(err) => {
                    eprintln!("无法启动异步运行时：{err}");
                    cli::Outcome::Failed.into()
                }
            };
        }
        Some("mcp") => {
            raw.next();
            let raw: Vec<OsString> = raw.collect();
            if raw.iter().any(|a| a == "-h" || a == "--help") {
                let _ = cli::emit(&cli::usage());
                return ExitCode::SUCCESS;
            }
            return match mcp::parse(raw) {
                Ok(dir) => mcp::run(dir),
                Err(msg) => {
                    eprintln!("{msg}");
                    cli::Outcome::Failed.into()
                }
            };
        }
        Some("-h" | "--help") => {
            let _ = cli::emit(&cli::usage());
            return ExitCode::SUCCESS;
        }
        None => {
            eprintln!("{}", cli::usage());
            return cli::Outcome::Failed.into();
        }
        _ => {}
    }
    // 打错子命令、参数不对与命令行一样以退出码 2 结束
    let args = match parse_args(raw) {
        Ok(args) => args,
        Err(msg) => {
            eprintln!("{msg}\n\n{}", cli::usage());
            return cli::Outcome::Failed.into();
        }
    };
    let data_dir = match (args.data_dir, args.shutdown) {
        (Some(dir), _) => dir,
        // 只读出当前指向的目录，不迁移：迁移前也要能请正在运行的核心退出
        (None, true) => match rummage_datadir::current() {
            Ok(dir) => dir,
            Err(err) => {
                eprintln!("无法确定数据目录：{err}");
                return ExitCode::FAILURE;
            }
        },
        // 核心模式只由界面或命令行带 --data-dir 拉起
        (None, false) => {
            eprintln!("{}", cli::usage());
            return cli::Outcome::Failed.into();
        }
    };
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("无法启动异步运行时：{err}");
            return ExitCode::FAILURE;
        }
    };
    // --shutdown 只是客户端，不写核心的日志文件
    if args.shutdown {
        return runtime.block_on(shutdown(&data_dir));
    }
    // 句柄要保留到退出，丢弃时日志停止写入
    let _log = init_log(&data_dir);
    runtime.block_on(run(&data_dir))
}

#[cfg(test)]
#[path = "main/tests.rs"]
mod tests;
