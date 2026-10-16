//! 核心进程入口的参数解析测试（核心模式）
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
