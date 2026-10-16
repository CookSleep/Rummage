//! 应用内更新在 Windows（`update`）与 Android（`android::update`）共用的部分：
//! - 已下载的更新包放在 `<数据目录>/updates/`，同目录的 `update.json` 记录版本号与 SHA-256，最后写入，
//!   中途失败时不会留下看似完整的记录。
//! - 同一时间只进行一个下载或安装任务（`acquire`）；进行中时校验命令不触碰正在写入的文件。
//! - 启动时删除版本不高于当前版本或无法识别的更新包（`clear_stale`）。
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rummage_proto::ApiError;
use semver::Version;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

use crate::data::DataDir;
use crate::search::error;

pub const UPDATE: &str = "update";
pub const META_FILE: &str = "update.json";

static BUSY: AtomicBool = AtomicBool::new(false);

pub struct BusyGuard;

impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

/// 开始下载或安装；已有任务在进行中时报错
pub fn acquire() -> Result<BusyGuard, ApiError> {
    if BUSY.swap(true, Ordering::AcqRel) {
        return Err(error(UPDATE, "已有更新任务在进行中。"));
    }
    Ok(BusyGuard)
}

pub fn busy() -> bool {
    BUSY.load(Ordering::Acquire)
}

#[derive(Serialize, Deserialize)]
pub struct Meta {
    pub version: String,
    pub sha256: String,
}

/// 对应前端 `src/lib/clientUpdate.ts` 的 `UpdateProgress`
#[derive(Clone, Serialize)]
pub struct Progress {
    pub downloaded: u64,
    pub total: Option<u64>,
}

pub fn updates_dir<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, ApiError> {
    Ok(app.state::<DataDir>().dir()?.join("updates"))
}

pub fn parse_version(raw: &str) -> Option<Version> {
    Version::parse(raw.trim().trim_start_matches('v')).ok()
}

pub fn read_meta(dir: &Path) -> Option<Meta> {
    serde_json::from_slice(&fs::read(dir.join(META_FILE)).ok()?).ok()
}

/// 删除已安装（版本不高于当前版本）或无法识别的更新包
pub fn clear_stale(dir: &Path, current: &Version) {
    let stale = dir.exists()
        && read_meta(dir)
            .and_then(|meta| parse_version(&meta.version))
            .is_none_or(|v| v <= *current);
    if stale && let Err(err) = fs::remove_dir_all(dir) {
        log::warn!("清理旧的更新包失败：{err}");
    }
}

/// Windows 上文件被其他进程占用（ERROR_SHARING_VIOLATION / ERROR_LOCK_VIOLATION）
pub fn is_locked(err: &io::Error) -> bool {
    cfg!(windows) && matches!(err.raw_os_error(), Some(32 | 33))
}

/// 保存失败中用户能自行处理的原因给出整句
pub fn save_error(err: io::Error) -> ApiError {
    if is_locked(&err) {
        return error(UPDATE, "更新包文件被占用，请稍后重试。");
    }
    match err.kind() {
        ErrorKind::StorageFull => error(UPDATE, "磁盘空间不足，无法保存更新包。"),
        ErrorKind::PermissionDenied => error(UPDATE, "没有写入权限，无法保存更新包。"),
        _ => error(UPDATE, format!("保存更新包失败：{err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_errors_explain_fixable_causes() {
        assert_eq!(
            save_error(ErrorKind::StorageFull.into()).message,
            "磁盘空间不足，无法保存更新包。"
        );
        assert_eq!(
            save_error(ErrorKind::PermissionDenied.into()).message,
            "没有写入权限，无法保存更新包。"
        );
        assert!(
            save_error(io::Error::other("x"))
                .message
                .starts_with("保存更新包失败：")
        );
    }

    #[cfg(windows)]
    #[test]
    fn save_errors_detect_locked_files() {
        for code in [32, 33] {
            assert_eq!(
                save_error(io::Error::from_raw_os_error(code)).message,
                "更新包文件被占用，请稍后重试。"
            );
        }
    }

    #[test]
    fn stale_or_unknown_packages_are_cleared() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("updates");
        fs::create_dir_all(&dir).unwrap();
        // 没有记录：无法识别
        clear_stale(&dir, &Version::new(0, 1, 0));
        assert!(!dir.exists());
        fs::create_dir_all(&dir).unwrap();
        let meta = Meta {
            version: "0.2.0".into(),
            sha256: String::new(),
        };
        fs::write(dir.join(META_FILE), serde_json::to_vec(&meta).unwrap()).unwrap();
        clear_stale(&dir, &Version::new(0, 1, 0));
        assert!(dir.exists());
        clear_stale(&dir, &Version::new(0, 2, 0));
        assert!(!dir.exists());
    }
}
