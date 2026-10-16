//! 版本信息与应用内更新：
//! - 版本列表：启动时的自动检查、设置“关于”页的检查更新与更新日志从 GitHub Releases API 读取版本列表，
//!   解析、缓存与比较版本都在前端（`src/lib/releaseUpdates.ts`）。请求由 Rust 侧发出，CSP 的 `connect-src` 不必放开外部地址。
//!   前端只给页码，地址由这里的常量拼出；重定向只跟随到 api.github.com 的仓库地址（仓库改名后 GitHub 会跳到 `/repositories/<id>/…`）。
//!   正文最多读 2 MB；仓库不存在（404）按没有发布的版本返回空列表。连接失败与 GitHub 的错误状态转换成中文整句。
//! - 下载：updater 插件按 tauri.conf.json 中的更新地址检查并下载更新包，下载时校验签名；通过校验的安装包保存到
//!   `<数据目录>\updates\`，并记录版本号和 SHA-256。之后无论远端是否已有更新的版本，都安装这个已下载的版本，重启应用后仍可安装。
//! - 安装：先写下窗口状态，请后台服务保存进度并退出、等它的进程结束（不按时退出就强制结束），再启动安装程序，
//!   插件随即结束界面进程，由安装程序重新打开。安装程序没能启动时重新拉起后台服务。
//! - 启动时清理版本不高于当前版本的安装包，以及插件写到系统临时目录的残留安装程序。
//! - 所有请求都走网络代理设置：每次从后台服务读取；读不到时报错，不绕过用户的代理设置直接联网。
//! - 开发版可用环境变量 `RUMMAGE_UPDATER_ENDPOINT` 指向本地模拟更新源（插件在 debug 构建下放行 http），发布版始终使用配置中的地址。
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use reqwest::{Client, StatusCode, Url, redirect};
use rummage_core::proxy::Route;
use rummage_proto::{ApiError, Reply, Request};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_updater::{Error, Update, UpdaterExt};
use tauri_plugin_window_state::{AppHandleExt, StateFlags};

use crate::data::DataDir;
use crate::search::{error, unexpected};
use crate::{core, tray};

/// 与 `src/lib/releaseUpdates.ts` 的 `RELEASE_REPO` 保持一致
const RELEASE_REPO: &str = "CookSleep/Rummage";
/// 与 `src/lib/releaseUpdates.ts` 的 `RELEASES_PER_PAGE` 保持一致，前端据此判断是否还有下一页
const PER_PAGE: u32 = 10;
const MAX_BODY: usize = 2 << 20;
const USER_AGENT: &str = concat!("rummage/", env!("CARGO_PKG_VERSION"));
const NETWORK: &str = "network";
const UPDATE: &str = "update";
const META_FILE: &str = "update.json";
const PACKAGE_FILE: &str = "update.bin";
const LOCK: &str = "持锁期间只做读写字段，不会 panic";

/// 下载或安装进行中；重复调用直接报错，校验命令也不触碰正在写入的文件
static BUSY: AtomicBool = AtomicBool::new(false);

struct BusyGuard;

impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

fn acquire() -> Result<BusyGuard, ApiError> {
    if BUSY.swap(true, Ordering::AcqRel) {
        return Err(error(UPDATE, "已有更新任务在进行中。"));
    }
    Ok(BusyGuard)
}

/// 本次运行中下载完成的更新信息，安装时直接使用，不必再联网检查
static DOWNLOADED: Mutex<Option<Update>> = Mutex::new(None);

#[derive(Serialize, Deserialize)]
struct Meta {
    version: String,
    sha256: String,
}

/// 对应前端 `src/lib/clientUpdate.ts` 的 `UpdateProgress`
#[derive(Clone, Serialize)]
pub struct Progress {
    downloaded: u64,
    total: Option<u64>,
}

/// 读取后台服务中的代理设置
async fn proxy_route<R: Runtime>(app: &AppHandle<R>) -> Result<Route, ApiError> {
    match core::request(app, Request::SetupStatus).await {
        Ok(Reply::Setup(s)) => Ok(Route::from_setting(&s.proxy)),
        Ok(other) => Err(unexpected(&other)),
        Err(err) if err == core::unavailable() => Err(error(
            ApiError::UNAVAILABLE,
            "后台服务未连接，无法读取代理设置，请稍后再试。",
        )),
        Err(err) => Err(err),
    }
}

fn updates_dir<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, ApiError> {
    Ok(app.state::<DataDir>().dir()?.join("updates"))
}

fn parse_version(raw: &str) -> Option<Version> {
    Version::parse(raw.trim().trim_start_matches('v')).ok()
}

fn read_meta(dir: &Path) -> Option<Meta> {
    serde_json::from_slice(&fs::read(dir.join(META_FILE)).ok()?).ok()
}

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

/// Windows 上文件被其他进程占用（ERROR_SHARING_VIOLATION / ERROR_LOCK_VIOLATION）
fn is_locked(err: &io::Error) -> bool {
    cfg!(windows) && matches!(err.raw_os_error(), Some(32 | 33))
}

/// 保存失败中用户能自行处理的原因给出整句
fn save_error(err: io::Error) -> ApiError {
    if is_locked(&err) {
        return error(UPDATE, "更新包文件被占用，请稍后重试。");
    }
    match err.kind() {
        ErrorKind::StorageFull => error(UPDATE, "磁盘空间不足，无法保存更新包。"),
        ErrorKind::PermissionDenied => error(UPDATE, "没有写入权限，无法保存更新包。"),
        _ => error(UPDATE, format!("保存更新包失败：{err}")),
    }
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
        let stale = dir.exists()
            && read_meta(dir)
                .and_then(|meta| parse_version(&meta.version))
                .is_none_or(|v| v <= *current);
        if stale && let Err(err) = fs::remove_dir_all(dir) {
            log::warn!("清理旧的更新包失败：{err}");
        }
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
            let url = Url::parse(&url).map_err(|err| invalid(err.to_string()))?;
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
    if BUSY.load(Ordering::Acquire) {
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
    if let Err(err) = app.save_window_state(StateFlags::all()) {
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

/// 读取 GitHub Releases API 的第 `page` 页（从 1 开始），返回响应正文
#[tauri::command]
pub async fn fetch_releases<R: Runtime>(app: AppHandle<R>, page: u32) -> Result<String, ApiError> {
    let url = Url::parse(&format!(
        "https://api.github.com/repos/{RELEASE_REPO}/releases?per_page={PER_PAGE}&page={}",
        page.max(1)
    ))
    .expect("常量拼出的地址总是合法的");
    let route = proxy_route(&app).await?;
    let policy = redirect::Policy::custom(|attempt| {
        let u = attempt.url();
        let ok = attempt.previous().len() < 5
            && u.scheme() == "https"
            && u.host_str() == Some("api.github.com")
            && (u.path().starts_with("/repos/") || u.path().starts_with("/repositories/"));
        if ok { attempt.follow() } else { attempt.stop() }
    });
    let builder = Client::builder()
        .user_agent(USER_AGENT)
        .redirect(policy)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30));
    let client = route
        .apply(builder)
        .and_then(|b| b.build())
        .map_err(|err| {
            log::error!("无法创建检查更新用的网络客户端：{err}");
            error(NETWORK, "无法创建网络连接，请检查代理设置后重试。")
        })?;
    let connect_error = |err: reqwest::Error| {
        log::warn!("检查更新时网络出错：{err:?}");
        let msg = if route.proxies(&url) && err.is_connect() {
            "无法连接代理服务器，请检查代理设置。"
        } else if err.is_timeout() {
            "连接 GitHub 超时，请检查网络或代理设置。"
        } else if err.is_body() || err.is_decode() {
            "读取版本信息时连接中断，请重试。"
        } else {
            "无法连接 GitHub，请检查网络或代理设置。"
        };
        error(NETWORK, msg)
    };
    let mut res = client
        .get(url.clone())
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(connect_error)?;
    let status = res.status();
    // 仓库还不存在或尚未公开时没有任何发布的版本
    if status == StatusCode::NOT_FOUND {
        return Ok("[]".into());
    }
    // 403 也可能是其他原因（如被拒绝访问），只有剩余次数为 0 时才是限流
    let exhausted = res
        .headers()
        .get("x-ratelimit-remaining")
        .is_some_and(|v| v.as_bytes() == b"0");
    if status == StatusCode::TOO_MANY_REQUESTS || (status == StatusCode::FORBIDDEN && exhausted) {
        return Err(error(NETWORK, "GitHub 请求过于频繁，请稍后再试。"));
    }
    if !status.is_success() {
        log::warn!("检查更新时 GitHub 返回 {status}");
        return Err(error(
            NETWORK,
            format!("GitHub 返回错误（HTTP {}），请稍后再试。", status.as_u16()),
        ));
    }
    let too_large = || {
        log::warn!("版本信息超过 {MAX_BODY} 字节，已放弃读取");
        error(NETWORK, "版本信息过大，无法读取。")
    };
    if res.content_length().is_some_and(|n| n > MAX_BODY as u64) {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(connect_error)? {
        if body.len() + chunk.len() > MAX_BODY {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| error(NETWORK, "版本信息的格式不正确。"))
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
    fn save_and_install_errors_detect_locked_files() {
        for code in [32, 33] {
            assert_eq!(
                save_error(io::Error::from_raw_os_error(code)).message,
                "更新包文件被占用，请稍后重试。"
            );
        }
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
