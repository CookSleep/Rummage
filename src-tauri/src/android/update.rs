//! Android 上的应用内更新（updater 插件不支持 Android）。命令与 Windows 的 `update` 同名、同参数、同返回值，前端不区分平台；
//! Kotlin 部分见 crates/tauri-plugin-rummage-android 的 `ApkInstaller`。
//! - 更新信息：每个 Release 带一份 `android.json`（`{version, url, sha256, size}`，由发布工作流生成），
//!   从 `releases/latest/download/android.json` 读取。APK 是否可信由 Android 自己保证：签名与已安装版本不同的 APK 无法覆盖安装；
//!   SHA-256 与大小只用来确认下载完整。
//! - 下载：`android.json` 的版本须等于前端给的最新版本（Release 列表中的）且高于当前版本。APK 先写到
//!   `<数据目录>/updates/download.part`，边写边算 SHA-256，大小与 SHA-256 都对上后才换成 `update.apk` 并写下记录
//!   （`update_store`），下载失败时原有的安装包仍可用。请求同版本列表一样走网络代理设置（`releases::proxy_route`）。
//! - 安装：还没在“安装未知应用”中允许本应用时先打开该设置页，返回后仍未允许则报错；再用 `PackageInstaller` 会话安装，
//!   系统要求确认时打开系统的确认界面，用户确认后系统结束本进程、换上新版本。
//!   后台服务没有能在进程内停下、取消安装时又能接着运行的方式，安装前不停止索引：进程被系统结束时，
//!   处理中的作业由下次启动的恢复（`Db::recover_jobs`）计一次失败。
//! - 启动时清理版本不高于当前版本的安装包与没下载完的文件。
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use reqwest::Url;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
#[cfg(target_os = "android")]
use {
    super::storage::failed,
    crate::releases::{NETWORK, proxy_route},
    crate::search::error,
    crate::update_store::{Progress, UPDATE, acquire, busy, save_error, updates_dir},
    reqwest::{Client, StatusCode},
    rummage_proto::ApiError,
    std::time::{Duration, Instant},
    tauri::ipc::Channel,
    tauri::{AppHandle, Manager, Runtime},
    tauri_plugin_rummage_android::Android,
    tokio::io::AsyncWriteExt,
};

use crate::update_store::{META_FILE, Meta, clear_stale, parse_version, read_meta};

#[cfg(target_os = "android")]
const INFO_URL: &str = "https://github.com/CookSleep/Rummage/releases/latest/download/android.json";
const APK_FILE: &str = "update.apk";
const PART_FILE: &str = "download.part";
#[cfg(target_os = "android")]
const MAX_INFO: usize = 64 << 10;
#[cfg(target_os = "android")]
const USER_AGENT: &str = concat!("rummage/", env!("CARGO_PKG_VERSION"));

/// `android.json` 的内容
#[derive(Debug, PartialEq, Deserialize)]
struct Info {
    version: String,
    url: String,
    sha256: String,
    size: u64,
}

/// 解析 `android.json`：版本号合法、地址为 https、SHA-256 为 64 位十六进制（统一成小写）、大小不为 0
fn parse_info(body: &[u8]) -> Option<Info> {
    let info: Info = serde_json::from_slice(body).ok()?;
    let ok = parse_version(&info.version).is_some()
        && Url::parse(&info.url).is_ok_and(|u| u.scheme() == "https")
        && info.sha256.len() == 64
        && info.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        && info.size > 0;
    ok.then(|| Info {
        sha256: info.sha256.to_ascii_lowercase(),
        ..info
    })
}

fn hash_file(path: &Path) -> io::Result<String> {
    let mut hasher = Sha256::new();
    io::copy(&mut fs::File::open(path)?, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// 读取版本高于当前运行版本且通过 SHA-256 校验的安装包；版本不高于当前版本时只返回 None（交给启动清理），
/// 校验失败时删除
fn read_saved(dir: &Path, current: &Version) -> Option<(Meta, PathBuf)> {
    let meta =
        read_meta(dir).filter(|meta| parse_version(&meta.version).is_some_and(|v| v > *current))?;
    let apk = dir.join(APK_FILE);
    if hash_file(&apk).is_ok_and(|sha| sha == meta.sha256) {
        return Some((meta, apk));
    }
    if let Err(err) = fs::remove_dir_all(dir) {
        log::warn!("删除校验失败的更新包失败：{err}");
    }
    None
}

/// 下载完成并通过校验的 `download.part` 换成安装包，再写下记录；先删旧记录，中途失败时不会留下与文件不符的记录
fn commit(dir: &Path, version: &str, sha256: &str) -> io::Result<()> {
    match fs::remove_file(dir.join(META_FILE)) {
        Err(err) if err.kind() != ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    fs::rename(dir.join(PART_FILE), dir.join(APK_FILE))?;
    let meta = Meta {
        version: version.to_string(),
        sha256: sha256.to_string(),
    };
    fs::write(dir.join(META_FILE), serde_json::to_vec(&meta)?)
}

/// 启动时清理：已安装或无法识别的安装包，以及上次没下载完的文件
pub fn cleanup(dir: &Path, current: &Version) {
    clear_stale(dir, current);
    match fs::remove_file(dir.join(PART_FILE)) {
        Err(err) if err.kind() != ErrorKind::NotFound => {
            log::warn!("清理没下载完的更新包失败：{err}")
        }
        _ => {}
    }
}

#[cfg(target_os = "android")]
fn network_error(err: reqwest::Error) -> ApiError {
    log::warn!("下载更新包失败：{err:?}");
    if err.is_timeout() {
        return error(NETWORK, "下载更新包超时，请重试。");
    }
    error(NETWORK, "网络连接失败，请检查网络和代理设置后重试。")
}

#[cfg(target_os = "android")]
async fn client<R: Runtime>(app: &AppHandle<R>) -> Result<Client, ApiError> {
    let builder = Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(15));
    proxy_route(app)
        .await?
        .apply(builder)
        .and_then(|b| b.build())
        .map_err(|err| {
            log::error!("无法创建下载更新用的网络客户端：{err}");
            error(NETWORK, "无法创建网络连接，请检查代理设置后重试。")
        })
}

#[cfg(target_os = "android")]
async fn fetch_info(client: &Client) -> Result<Info, ApiError> {
    let mut res = client
        .get(INFO_URL)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(network_error)?;
    let status = res.status();
    if status == StatusCode::NOT_FOUND {
        return Err(error(UPDATE, "未找到更新包信息。"));
    }
    if !status.is_success() {
        return Err(error(
            UPDATE,
            format!("检查更新包失败：HTTP {}", status.as_u16()),
        ));
    }
    let mut body = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(network_error)? {
        if body.len() + chunk.len() > MAX_INFO {
            return Err(error(UPDATE, "更新包信息的格式不正确。"));
        }
        body.extend_from_slice(&chunk);
    }
    parse_info(&body).ok_or_else(|| error(UPDATE, "更新包信息的格式不正确。"))
}

/// 下载 APK 到 `part`，返回 SHA-256；大小超出 `android.json` 中的大小时中止
#[cfg(target_os = "android")]
async fn download(
    client: &Client,
    info: &Info,
    part: &Path,
    on_progress: &Channel<Progress>,
) -> Result<String, ApiError> {
    let mut file = tokio::fs::File::create(part).await.map_err(save_error)?;
    // 与 Windows 相同：下载设总时限，网络卡住时按钮不会一直停在下载中
    let mut res = client
        .get(&info.url)
        .timeout(Duration::from_secs(600))
        .send()
        .await
        .map_err(network_error)?;
    if !res.status().is_success() {
        log::warn!("下载更新包时服务器返回 {}", res.status());
        return Err(error(
            UPDATE,
            format!("下载更新包失败：HTTP {}", res.status().as_u16()),
        ));
    }
    let mut hasher = Sha256::new();
    let mut downloaded = 0u64;
    let mut last = Instant::now();
    while let Some(chunk) = res.chunk().await.map_err(network_error)? {
        downloaded += chunk.len() as u64;
        if downloaded > info.size {
            return Err(error(UPDATE, "更新包校验失败，请重新下载。"));
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(save_error)?;
        // 数据块很小，限制通知频率，避免大量 IPC 消息
        if last.elapsed() >= Duration::from_millis(100) || downloaded == info.size {
            last = Instant::now();
            let _ = on_progress.send(Progress {
                downloaded,
                total: Some(info.size),
            });
        }
    }
    file.sync_all().await.map_err(save_error)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// 本地有效安装包的版本（高于当前运行版本且通过 SHA-256 校验），没有时返回 None
#[cfg(target_os = "android")]
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

/// 下载 version 对应的 APK 并保存（覆盖旧包），下载进度经 on_progress 传给前端。
/// version 为 Release 列表中的最新版本，android.json 尚未同步到该版本时不下载
#[cfg(target_os = "android")]
#[tauri::command]
pub async fn update_download<R: Runtime>(
    app: AppHandle<R>,
    version: String,
    on_progress: Channel<Progress>,
) -> Result<(), ApiError> {
    let _busy = acquire()?;
    let client = client(&app).await?;
    let info = fetch_info(&client).await?;
    let current = app.package_info().version.clone();
    if parse_version(&info.version).is_none_or(|v| v <= current) {
        return Err(error(UPDATE, "更新源中暂无新的版本。"));
    }
    if parse_version(&info.version) != parse_version(&version) {
        return Err(error(UPDATE, "更新包信息尚未同步到最新版本，请稍后重试。"));
    }
    let dir = updates_dir(&app)?;
    tokio::fs::create_dir_all(&dir).await.map_err(save_error)?;
    let part = dir.join(PART_FILE);
    let res = download(&client, &info, &part, &on_progress).await;
    let ok = match res {
        Ok(sha) if sha == info.sha256 => Ok(()),
        Ok(_) => Err(error(UPDATE, "更新包校验失败，请重新下载。")),
        Err(err) => Err(err),
    };
    if let Err(err) = ok {
        if let Err(e) = tokio::fs::remove_file(&part).await
            && e.kind() != ErrorKind::NotFound
        {
            log::warn!("删除没下载完的更新包失败：{e}");
        }
        return Err(err);
    }
    tauri::async_runtime::spawn_blocking(move || commit(&dir, &info.version, &info.sha256))
        .await
        .map_err(|err| error(ApiError::INTERNAL, format!("保存更新包异常结束：{err}")))?
        .map_err(|err| {
            log::warn!("保存更新包失败：{err}");
            save_error(err)
        })
}

/// 校验本地安装包后交给系统安装，安装的始终是已下载的版本（见模块说明）
#[cfg(target_os = "android")]
#[tauri::command]
pub async fn update_install<R: Runtime>(app: AppHandle<R>) -> Result<(), ApiError> {
    let _busy = acquire()?;
    let dir = updates_dir(&app)?;
    let current = app.package_info().version.clone();
    let (_, apk) = tauri::async_runtime::spawn_blocking(move || read_saved(&dir, &current))
        .await
        .map_err(|err| error(ApiError::INTERNAL, format!("校验更新包异常结束：{err}")))?
        .ok_or_else(|| error(UPDATE, "本地安装包已失效，请重新下载。"))?;
    let plugin = app.state::<Android<R>>();
    let allowed = plugin
        .can_install_packages()
        .await
        .map_err(|err| failed("无法读取安装应用的权限，请重试。", err))?;
    let allowed = allowed
        || plugin.request_install_packages().await.map_err(|err| {
            failed(
                "无法打开设置页面，请在系统设置中允许 Rummage 安装未知应用。",
                err,
            )
        })?;
    if !allowed {
        return Err(error(UPDATE, "需要允许 Rummage 安装应用，才能安装更新。"));
    }
    let res = plugin
        .install_apk(&apk)
        .await
        .map_err(|err| failed("安装更新失败，请重试。", err))?;
    match res.status.as_str() {
        "success" => Ok(()),
        "aborted" => Err(error(UPDATE, "已取消安装更新。")),
        _ => {
            log::warn!(
                "安装更新失败：{} {}",
                res.code,
                res.message.unwrap_or_default()
            );
            Err(error(UPDATE, "安装更新失败，请重试。"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "9F86D081884C7D659A2FEAA0C55AD015A3BF4F1B2B0B822CD15D6C15B0F00A08";

    fn body(version: &str, url: &str, sha: &str, size: u64) -> Vec<u8> {
        serde_json::to_vec(
            &serde_json::json!({ "version": version, "url": url, "sha256": sha, "size": size }),
        )
        .unwrap()
    }

    #[test]
    fn info_is_validated() {
        let url = "https://github.com/CookSleep/Rummage/releases/download/v0.2.0/Rummage_0.2.0_android-arm64.apk";
        assert_eq!(
            parse_info(&body("0.2.0", url, SHA, 4)),
            Some(Info {
                version: "0.2.0".into(),
                url: url.into(),
                sha256: SHA.to_ascii_lowercase(),
                size: 4,
            })
        );
        assert!(parse_info(&body("x", url, SHA, 4)).is_none());
        assert!(parse_info(&body("0.2.0", "http://example.com/a.apk", SHA, 4)).is_none());
        assert!(parse_info(&body("0.2.0", url, &SHA[1..], 4)).is_none());
        assert!(parse_info(&body("0.2.0", url, &SHA.replace('9', "g"), 4)).is_none());
        assert!(parse_info(&body("0.2.0", url, SHA, 0)).is_none());
        assert!(parse_info(b"{}").is_none());
    }

    #[test]
    fn saved_package_is_verified_and_stale_ones_are_cleaned() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("updates");
        let current = Version::new(0, 1, 0);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(PART_FILE), b"test").unwrap();
        commit(&dir, "0.2.0", &SHA.to_ascii_lowercase()).unwrap();
        let (meta, apk) = read_saved(&dir, &current).unwrap();
        assert_eq!(meta.version, "0.2.0");
        assert_eq!(fs::read(apk).unwrap(), b"test");
        assert!(!dir.join(PART_FILE).exists());
        // 没下载完的文件在启动时删除，已下载的安装包保留
        fs::write(dir.join(PART_FILE), b"te").unwrap();
        cleanup(&dir, &current);
        assert!(!dir.join(PART_FILE).exists());
        assert!(read_saved(&dir, &current).is_some());
        // 版本不高于当前版本时不安装，由启动清理删除
        assert!(read_saved(&dir, &Version::new(0, 2, 0)).is_none());
        cleanup(&dir, &Version::new(0, 2, 0));
        assert!(!dir.exists());
        // 内容被改动时删除
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(PART_FILE), b"test").unwrap();
        commit(&dir, "0.2.0", &SHA.to_ascii_lowercase()).unwrap();
        fs::write(dir.join(APK_FILE), b"tampered").unwrap();
        assert!(read_saved(&dir, &current).is_none());
        assert!(!dir.exists());
    }
}
