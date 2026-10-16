//! 开发用子命令（`selftest`、`eval`、`bench`）的参数解析测试
use super::*;

fn selftest(args: &[&str]) -> Result<SelftestArgs, String> {
    parse_selftest_args(args.iter().map(OsString::from))
}

#[test]
fn selftest_args() {
    let base = ["--data-dir", "d", "--golden", "g", "--runtime-dir", "rt"];
    assert_eq!(
        selftest(&[&base[..], &["--skip-gpu", "--no-cache"]].concat()),
        Ok(SelftestArgs {
            data_dir: PathBuf::from("d"),
            golden: PathBuf::from("g"),
            runtime_dir: PathBuf::from("rt"),
            vulkan_dir: None,
            model_dir: None,
            skip_gpu: true,
            no_cache: true,
        })
    );
    assert_eq!(
        selftest(&[&base[..], &["--vulkan-dir", "vk", "--model-dir", "m"]].concat()),
        Ok(SelftestArgs {
            data_dir: PathBuf::from("d"),
            golden: PathBuf::from("g"),
            runtime_dir: PathBuf::from("rt"),
            vulkan_dir: Some(PathBuf::from("vk")),
            model_dir: Some(PathBuf::from("m")),
            skip_gpu: false,
            no_cache: false,
        })
    );
    // 不跳过 GPU 时必须给出 Vulkan 运行库
    assert!(selftest(&base).is_err());
    assert!(selftest(&["--golden", "g", "--runtime-dir", "rt", "--skip-gpu"]).is_err());
    assert!(selftest(&[&base[..], &["--skip-gpu", "--golden"]].concat()).is_err());
    assert!(selftest(&[&base[..], &["--skip-gpu", "--verbose", "x"]].concat()).is_err());
}

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[test]
fn eval_args() {
    let (data, args) = parse_eval_args(os(&[
        "E",
        "--out",
        "r.csv",
        "--device",
        "CPU",
        "--work-dir",
        "w",
        "--data-dir",
        "d",
    ]))
    .unwrap();
    assert_eq!(data, Some(PathBuf::from("d")));
    assert_eq!(
        args,
        eval::Args {
            set: PathBuf::from("E"),
            out: Some(PathBuf::from("r.csv")),
            device: eval::Device::Cpu,
            work: PathBuf::from("w"),
            subsets: Vec::new(),
            params: Vec::new(),
        }
    );
    let (_, args) = parse_eval_args(os(&[
        "E",
        "--subset",
        "a",
        "--subset",
        "b",
        "--param",
        "long_image_ratio=2",
        "--param",
        "image_tokens=280",
    ]))
    .unwrap();
    assert_eq!(args.subsets, ["a", "b"]);
    assert_eq!(
        args.params,
        [
            ("long_image_ratio".to_string(), ParamValue::Float(2.0)),
            ("image_tokens".to_string(), ParamValue::Int(280)),
        ]
    );
    assert!(parse_eval_args(os(&["E", "--param", "image_tokens=300"])).is_err());
    assert!(parse_eval_args(os(&["E", "--param", "no_such=1"])).is_err());
    assert!(parse_eval_args(os(&["E", "--param", "long_image_ratio"])).is_err());
    let (_, args) = parse_eval_args(os(&["E", "--device", "4080 SUPER"])).unwrap();
    assert_eq!(args.device, eval::Device::Gpu("4080 SUPER".into()));
    assert!(parse_eval_args(os(&[])).is_err());
    assert!(parse_eval_args(os(&["E", "F"])).is_err());
    assert!(parse_eval_args(os(&["E", "--out"])).is_err());
    assert!(parse_eval_args(os(&["E", "--vectors", "1"])).is_err());
}

#[test]
fn bench_args() {
    let (data, args) = parse_bench_args(os(&["--vectors", "1000", "--samples", "s"])).unwrap();
    assert_eq!(data, None);
    assert_eq!(args.vectors, 1000);
    assert_eq!(args.queries, 100);
    assert_eq!(args.samples, Some(PathBuf::from("s")));
    assert_eq!(args.device, eval::Device::Auto);
    assert!(parse_bench_args(os(&["--vectors", "0"])).is_err());
    assert!(parse_bench_args(os(&["--queries", "x"])).is_err());
    assert!(parse_bench_args(os(&["extra"])).is_err());
    assert!(parse_bench_args(os(&["--out", "r.csv"])).is_err());
}
