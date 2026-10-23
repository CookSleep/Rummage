//! 应用内更新（只有桌面平台）：
//! - 下载：updater 插件按 tauri.conf.json 中的更新地址检查并下载更新包，下载时校验签名；通过校验的安装包保存到
//!   `<数据目录>\updates\`，并记录版本号和 SHA-256。之后无论远端是否已有更新的版本，都安装这个已下载的版本，重启应用后仍可安装。
//! - 安装：先写下窗口状态，请后台服务保存进度并退出、等它的进程结束（不按时退出就强制结束），再启动安装程序，
//!   插件随即结束界面进程，由安装程序重新打开。安装程序没能启动时重新拉起后台服务。
//! - 启动时清理版本不高于当前版本的安装包，以及插件写到系统临时目录的残留安装程序。
//! - 检查更新包同版本列表一样走网络代理设置（`releases::proxy_route`）。
//! - 开发版可用环境变量 `RUMMAGE_UPDATER_ENDPOINT` 指向本地模拟更新源（插件在 debug 构建下放行 http），发布版始终使用配置中的地址。
use std::fs;
use std::io::{self, ErrorKind};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rummage_core::proxy::Route;
use rummage_proto::ApiError;
use semver::Version;
use sha2::{Digest, Sha256};
use tauri::ipc::Channel;
use tauri::{AppHandle, Runtime};
use tauri_plugin_updater::{Error, Update, UpdaterExt};
use tauri_plugin_window_state::AppHandleExt;

use crate::releases::{NETWORK, proxy_route};
use crate::search::error;
use crate::update_store::{
    META_FILE, Meta, Progress, UPDATE, acquire, busy, clear_stale, is_locked, parse_version,
    read_meta, save_error, updates_dir,
};
use crate::{core, tray};

const PACKAGE_FILE: &str = "update.bin";
const LOCK: &str = "持锁期间只做读写字段，不会 panic";

/// 本次运行中下载完成的更新信息，安装时直接使用，不必再联网检查
static DOWNLOADED: Mutex<Option<Update>> = Mutex::new(None);

/// 读取版本高于当前运行版本且通过 SHA-256 校验的安装包；版本不高于当前版本时只返回 None（交给启动清理），
/// 校验失败时删除
fn read_saved(dir: &Path, current: &Version) -> Option<(Meta, Vec<u8>)> {
    let meta =
        read_meta(dir).filter(|meta| parse_version(&meta.version).is_some_and(|v| v > *current))?;
    let bytes = fs::read(dir.join(PACKAGE_FILE))
        .ok()
        .filter(|bytes| format!("{:x}", Sha256::digest(bytes)) == meta.sha256);
    let Some(bytes) = bytes else {
        if let Err(err) = fs::remove_dir_all(dir) {
            log::warn!("删除校验失败的更新包失败：{err}");
        }
        return None;
    };
    Some((meta, bytes))
}

fn save(dir: &Path, version: &str, bytes: &[u8]) -> io::Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    fs::create_dir_all(dir)?;
    fs::write(dir.join(PACKAGE_FILE), bytes)?;
    // 元数据最后写入，中途失败时不会留下看似完整的记录
    let meta = Meta {
        version: version.to_string(),
        sha256: format!("{:x}", Sha256::digest(bytes)),
    };
    fs::write(dir.join(META_FILE), serde_json::to_vec(&meta)?)
}

/// 安装失败的常见原因：写入临时安装程序失败，或 ShellExecute 启动安装程序失败
fn install_error(err: Error) -> ApiError {
    let Error::Io(err) = err else {
        return match err {
            Error::InvalidUpdaterFormat | Error::BinaryNotFoundInArchive => {
                error(UPDATE, "安装包格式无效，请重新下载。")
            }
            err => error(UPDATE, format!("安装更新失败：{err}")),
        };
    };
    if is_locked(&err) {
        return error(UPDATE, "安装程序文件被占用，请稍后重试。");
    }
    match err.kind() {
        ErrorKind::StorageFull => error(UPDATE, "磁盘空间不足，无法写入安装程序。"),
        ErrorKind::PermissionDenied => {
            error(UPDATE, "没有权限写入或启动安装程序，可能被安全软件拦截。")
        }
        ErrorKind::NotFound => error(UPDATE, "找不到安装程序，可能被安全软件删除，请重新下载。"),
        _ => error(UPDATE, format!("安装更新失败：{err}")),
    }
}

/// 启动时清理：删除已安装（版本不高于当前版本）或无法识别的更新包；
/// 以及插件安装时写到系统临时目录的安装程序。插件运行安装程序后直接结束进程，来不及删除，
/// 目录名为“<应用名>-<版本>-updater-<6 位随机字符>”，只删除名称完全符合且版本不高于当前版本的目录
pub fn cleanup(updates: Option<&Path>, app_name: &str, current: &Version) {
    if let Some(dir) = updates {
        clear_stale(dir, current);
    }
    let Ok(entries) = fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let prefix = format!("{app_name}-");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some((version, suffix)) = name
            .to_str()
            .and_then(|name| name.strip_prefix(&prefix))
            .and_then(|rest| rest.rsplit_once("-updater-"))
        else {
            continue;
        };
        let installed = parse_version(version).is_some_and(|v| v <= *current);
        let random = suffix.len() == 6 && suffix.chars().all(|c| c.is_ascii_alphanumeric());
        if !installed || !random || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        // 安装程序可能仍在收尾而占用文件，失败时留到下次启动再删
        if let Err(err) = fs::remove_dir_all(entry.path()) {
            log::warn!("清理临时安装程序失败：{err}");
        }
    }
}

/// any 为 true 时不比较版本，只为取得安装所需的 Update 句柄
async fn check<R: Runtime>(app: &AppHandle<R>, any: bool) -> Result<Update, ApiError> {
    let builder = app.updater_builder().timeout(Duration::from_secs(30));
    // 检查得到的 Update 会带上这里的代理设置，下载时沿用
    let builder = match proxy_route(app).await? {
        Route::System => builder,
        Route::Direct => builder.no_proxy(),
        Route::Via(url) => builder.proxy(url),
    };
    let builder = if any {
        builder.version_comparator(|_, _| true)
    } else {
        builder
    };
    #[cfg(debug_assertions)]
    let builder = match std::env::var("RUMMAGE_UPDATER_ENDPOINT") {
        Ok(url) => {
            let invalid = |err: String| error(UPDATE, format!("更新源地址无效：{err}"));
            let url = reqwest::Url::parse(&url).map_err(|err| invalid(err.to_string()))?;
            builder
                .endpoints(vec![url])
                .map_err(|err| invalid(err.to_string()))?
        }
        Err(_) => builder,
    };
    let updater = builder
        .build()
        .map_err(|err| error(UPDATE, format!("初始化更新失败：{err}")))?;
    match updater.check().await {
        Ok(Some(update)) => Ok(update),
        Ok(None) => Err(error(UPDATE, "更新源中暂无新的版本。")),
        Err(Error::ReleaseNotFound) => Err(error(UPDATE, "未找到更新包信息。")),
        Err(Error::TargetNotFound(_) | Error::TargetsNotFound(_)) => {
            Err(error(UPDATE, "更新包信息中没有适用于当前系统的安装包。"))
        }
        Err(err @ (Error::Reqwest(_) | Error::Network(_))) => {
            log::warn!("检查更新包失败：{err}");
            Err(error(NETWORK, "网络连接失败，请检查网络和代理设置后重试。"))
        }
        Err(err) => Err(error(UPDATE, format!("检查更新包失败：{err}"))),
    }
}

/// 本地有效安装包的版本（高于当前运行版本且通过 SHA-256 校验），没有时返回 None
#[tauri::command]
pub async fn update_ready<R: Runtime>(app: AppHandle<R>) -> Result<Option<String>, ApiError> {
    if busy() {
        return Ok(None);
    }
    let dir = updates_dir(&app)?;
    let current = app.package_info().version.clone();
    tauri::async_runtime::spawn_blocking(move || {
        read_saved(&dir, &current).map(|(meta, _)| meta.version)
    })
    .await
    .map_err(|err| error(ApiError::INTERNAL, format!("校验更新包异常结束：{err}")))
}

/// 下载 version 对应的更新包并保存（覆盖旧包），下载进度经 on_progress 传给前端。
/// version 为 Release 列表中的最新版本，latest.json 尚未同步到该版本时不下载
#[tauri::command]
pub async fn update_download<R: Runtime>(
    app: AppHandle<R>,
    version: String,
    on_progress: Channel<Progress>,
) -> Result<(), ApiError> {
    let _busy = acquire()?;
    let mut update = check(&app, false).await?;
    if parse_version(&update.version) != parse_version(&version) {
        return Err(error(UPDATE, "更新包信息尚未同步到最新版本，请稍后重试。"));
    }
    // 插件下载默认不设超时，网络卡住时按钮会一直停在下载中
    update.timeout = Some(Duration::from_secs(600));
    let mut downloaded = 0u64;
    let mut last = Instant::now();
    let bytes = update
        .download(
            |len, total| {
                downloaded += len as u64;
                // 数据块很小，限制通知频率，避免大量 IPC 消息
                if last.elapsed() < Duration::from_millis(100) && total != Some(downloaded) {
                    return;
                }
                last = Instant::now();
                let _ = on_progress.send(Progress { downloaded, total });
            },
            || {},
        )
        .await
        .map_err(|err| {
            log::warn!("下载更新包失败：{err}");
            match err {
                Error::Reqwest(err) if err.is_timeout() => {
                    error(NETWORK, "下载更新包超时，请重试。")
                }
                Error::Reqwest(_) | Error::Network(_) => {
                    error(NETWORK, "网络连接失败，请检查网络和代理设置后重试。")
                }
                Error::Minisign(_)
                | Error::Base64(_)
                | Error::SignatureUtf8(_)
                | Error::MissingSignedVersion
                | Error::SignedVersionMismatch { .. } => error(UPDATE, "更新包签名校验失败。"),
                err => error(UPDATE, format!("下载更新包失败：{err}")),
            }
        })?;
    let dir = updates_dir(&app)?;
    let saved = update.version.clone();
    tauri::async_runtime::spawn_blocking(move || save(&dir, &saved, &bytes))
        .await
        .map_err(|err| error(ApiError::INTERNAL, format!("保存更新包异常结束：{err}")))?
        .map_err(|err| {
            log::warn!("保存更新包失败：{err}");
            save_error(err)
        })?;
    *DOWNLOADED.lock().expect(LOCK) = Some(update);
    Ok(())
}

/// 校验本地安装包后安装，安装的始终是已下载的版本；Windows 上插件启动安装程序后直接结束进程，由安装程序重新打开应用。
/// 插件的 install 只能通过 Update 调用（Windows 上按安装包内容判断类型，版本只用于临时文件名），安装时不读取下载地址和签名：
/// 本次运行中下载的直接使用；重启后没有时联网检查取得句柄，只需把版本改为已下载的版本。
/// 句柄要在停后台服务之前取得，检查要从后台服务读代理设置
#[tauri::command]
pub async fn update_install<R: Runtime>(app: AppHandle<R>) -> Result<(), ApiError> {
    let _busy = acquire()?;
    let dir = updates_dir(&app)?;
    let current = app.package_info().version.clone();
    let (meta, bytes) = tauri::async_runtime::spawn_blocking(move || read_saved(&dir, &current))
        .await
        .map_err(|err| error(ApiError::INTERNAL, format!("校验更新包异常结束：{err}")))?
        .ok_or_else(|| error(UPDATE, "本地安装包已失效，请重新下载。"))?;
    let cached = DOWNLOADED
        .lock()
        .expect(LOCK)
        .clone()
        .filter(|update| parse_version(&update.version) == parse_version(&meta.version));
    let update = match cached {
        Some(update) => update,
        None => {
            let mut update = check(&app, true).await?;
            update.version = meta.version;
            update
        }
    };
    // 插件结束进程时不经过关窗，窗口状态要先写下
    if let Err(err) = app.save_window_state(crate::WINDOW_STATE) {
        log::warn!("无法保存窗口状态：{err}");
    }
    // 插件直接结束进程，托盘图标来不及移除，会留下空图标
    tray::hide(&app);
    // 安装程序要覆盖 rummage-core.exe，后台服务必须先退出
    core::shutdown(&app).await;
    let result = tauri::async_runtime::spawn_blocking(move || update.install(bytes))
        .await
        .map_err(|err| error(ApiError::INTERNAL, format!("安装更新异常结束：{err}")))
        .and_then(|res| {
            res.map_err(|err| {
                log::warn!("安装更新失败：{err}");
                install_error(err)
            })
        });
    if let Err(err) = result {
        tray::show(&app);
        core::resume(&app).await;
        return Err(err);
    }
    app.restart()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn install_errors_detect_locked_files() {
        assert_eq!(
            install_error(Error::Io(io::Error::from_raw_os_error(32))).message,
            "安装程序文件被占用，请稍后重试。"
        );
        // ERROR_ACCESS_DENIED / ERROR_FILE_NOT_FOUND 经 ErrorKind 映射
        assert_eq!(
            install_error(Error::Io(io::Error::from_raw_os_error(5))).message,
            "没有权限写入或启动安装程序，可能被安全软件拦截。"
        );
        assert_eq!(
            install_error(Error::Io(io::Error::from_raw_os_error(2))).message,
            "找不到安装程序，可能被安全软件删除，请重新下载。"
        );
    }

    #[test]
    fn install_errors_explain_fixable_causes() {
        assert_eq!(
            install_error(Error::Io(ErrorKind::StorageFull.into())).message,
            "磁盘空间不足，无法写入安装程序。"
        );
        assert_eq!(
            install_error(Error::InvalidUpdaterFormat).message,
            "安装包格式无效，请重新下载。"
        );
        assert!(
            install_error(Error::TempDirNotFound)
                .message
                .starts_with("安装更新失败：")
        );
    }

    #[test]
    fn saved_package_is_verified_and_stale_ones_are_cleaned() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("updates");
        let current = Version::new(0, 1, 0);
        save(&dir, "0.2.0", b"package").unwrap();
        let (meta, bytes) = read_saved(&dir, &current).unwrap();
        assert_eq!(
            (meta.version.as_str(), bytes.as_slice()),
            ("0.2.0", &b"package"[..])
        );
        // 版本不高于当前版本时不安装，由启动清理删除
        assert!(read_saved(&dir, &Version::new(0, 2, 0)).is_none());
        cleanup(Some(&dir), "RummageTest", &Version::new(0, 2, 0));
        assert!(!dir.exists());
        // 内容被改动时删除
        save(&dir, "0.2.0", b"package").unwrap();
        fs::write(dir.join(PACKAGE_FILE), b"tampered").unwrap();
        assert!(read_saved(&dir, &current).is_none());
        assert!(!dir.exists());
    }
}
