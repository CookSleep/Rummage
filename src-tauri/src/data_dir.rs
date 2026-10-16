//! 启动时确定数据目录：
//! 指向的目录无法使用（例如数据目录在移动硬盘上而硬盘没插）时，在创建窗口前弹出对话框，
//! 让用户重试、改用默认目录（再确认一次）或退出。无法显示对话框时本次使用默认目录，指向文件不动
use std::path::Path;

use rummage_datadir::{Error, Fallback, Startup};

/// 指向的目录无法使用时的选择
#[derive(Debug, Clone, Copy, PartialEq)]
enum Choice {
    Retry,
    /// 删除指向文件，改用默认目录
    UseDefault,
    /// 无法弹出对话框时：本次使用默认目录，指向文件保持不动
    Skip,
    Exit,
}

/// 指向的目录无法使用：重试、改用默认目录（再确认一次）或退出
fn ask(err: &Error, default: &Path) -> Choice {
    let (heading, content, footer) = match err {
        Error::Missing(dir) => (
            "找不到数据目录",
            "存储设备可能未连接。".to_string(),
            format!("数据目录：{}", dir.display()),
        ),
        Error::Invalid {
            reason,
            label,
            path,
        } => (
            "数据目录设置无效",
            reason.clone(),
            format!("{label}：{}", path.display()),
        ),
        _ => return Choice::Skip,
    };
    loop {
        match rummage_datadir::choose(
            heading,
            &content,
            &footer,
            &["重试", "改用默认目录", "退出"],
        ) {
            // 无法显示对话框：本次使用默认目录，指向文件不动
            None if !rummage_datadir::can_choose() => return Choice::Skip,
            Some(0) => return Choice::Retry,
            Some(1) => {
                let old = match err {
                    Error::Missing(dir) => Some(dir),
                    Error::Invalid {
                        label: "数据目录",
                        path,
                        ..
                    } => Some(path),
                    _ => None,
                };
                let footer = match old {
                    Some(old) => format!(
                        "原数据目录：{}\n默认目录：{}",
                        old.display(),
                        default.display()
                    ),
                    None => format!("默认目录：{}", default.display()),
                };
                let confirm = rummage_datadir::choose(
                    "改用默认目录？",
                    "原目录中的索引和模型不会转移，需要重新设置文件夹并下载模型。",
                    &footer,
                    &["改用默认目录", "返回"],
                );
                if confirm == Some(0) {
                    return Choice::UseDefault;
                }
            }
            _ => return Choice::Exit,
        }
    }
}

/// 确定数据目录，有待迁移的更改时先完成迁移；指向的目录无法使用时询问用户，选择退出时直接退出进程
pub fn prepare() -> Startup {
    let mut fallback = None;
    loop {
        let startup = rummage_datadir::prepare(fallback);
        let err = match &startup.dir {
            Err(err @ (Error::Missing(_) | Error::Invalid { .. })) => err,
            _ => return startup,
        };
        let default = rummage_datadir::default_location().unwrap_or_default();
        match ask(err, &default) {
            Choice::Retry => {}
            Choice::UseDefault => fallback = Some(Fallback::UseDefault),
            Choice::Skip => fallback = Some(Fallback::Skip),
            Choice::Exit => std::process::exit(0),
        }
    }
}
