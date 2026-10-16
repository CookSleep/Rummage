//! 核心进程入口的参数解析测试（核心模式与 `worker`）
use super::*;

fn parse(args: &[&str]) -> Result<Args, String> {
    parse_args(args.iter().map(OsString::from))
}

#[test]
fn args() {
    assert_eq!(parse(&[]), Ok(Args::default()));
    assert_eq!(
        parse(&["--data-dir", r"D:\数据 目录"]),
        Ok(Args {
            data_dir: Some(PathBuf::from(r"D:\数据 目录")),
            shutdown: false,
        })
    );
    assert_eq!(
        parse(&["--shutdown", "--data-dir", "a"]),
        Ok(Args {
            data_dir: Some(PathBuf::from("a")),
            shutdown: true,
        })
    );
    assert!(parse(&["--data-dir"]).is_err());
    assert!(parse(&["--verbose"]).is_err());
    assert!(parse(&["--data-dir", "a", "extra"]).is_err());
}

fn worker(args: &[&str]) -> Result<WorkerArgs, String> {
    parse_worker_args(args.iter().map(OsString::from))
}

#[test]
fn worker_args() {
    assert_eq!(
        worker(&["--runtime-dir", "rt", "--model", "m", "--threads", "6"]),
        Ok(WorkerArgs::Embed {
            runtime_dir: PathBuf::from("rt"),
            model: PathBuf::from("m"),
            mmproj: None,
            threads: 6,
            gpu: None,
            gpu_pci: None,
        })
    );
    assert_eq!(
        worker(&[
            "--runtime-dir",
            "rt",
            "--model",
            "m",
            "--mmproj",
            "p",
            "--threads",
            "2",
            "--gpu",
            "NVIDIA GeForce RTX 4080 SUPER",
            "--gpu-pci",
            "0000:06:00.0",
        ]),
        Ok(WorkerArgs::Embed {
            runtime_dir: PathBuf::from("rt"),
            model: PathBuf::from("m"),
            mmproj: Some(PathBuf::from("p")),
            threads: 2,
            gpu: Some("NVIDIA GeForce RTX 4080 SUPER".to_string()),
            gpu_pci: Some("0000:06:00.0".to_string()),
        })
    );
    assert_eq!(
        worker(&["--list-devices", "--runtime-dir", "rt"]),
        Ok(WorkerArgs::ListDevices {
            runtime_dir: PathBuf::from("rt"),
        })
    );
    assert!(worker(&["--runtime-dir", "rt", "--threads", "6"]).is_err());
    assert!(worker(&["--runtime-dir", "rt", "--model", "m", "--threads", "0"]).is_err());
    assert!(
        worker(&[
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
    assert!(worker(&["--model", "m", "--threads", "6"]).is_err());
    assert!(worker(&["--runtime-dir"]).is_err());
    assert!(worker(&["--runtime-dir", "rt", "--verbose"]).is_err());
}
