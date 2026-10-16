//! Android 上的存储权限与选择文件夹、文件的命令（Kotlin 部分见 crates/tauri-plugin-rummage-android）：
//! - 读取被索引的文件靠“所有文件访问”权限：`storage_access` 报告是否已授权，`request_storage_access` 打开系统的授权页面。
//! - 添加文件夹只接受能换算成真实路径的位置（手机存储与存储卡），网盘等其他文档提供者的位置报告为不支持；
//!   `add_common_root` 一键添加手机存储中的 DCIM 等常用文件夹。
//! - 导入模型、以图搜图选中的文件换算不出可读的真实路径时，由插件复制到 `<数据目录>/cache/import`、`cache/pick`：
//!   导入的副本在导入任务结束（`ModelTask` 事件不再是进行中）或请求失败时删除；以图搜图只保留最近选的一张，
//!   新选的图片放进新的子目录，选好后删除之前的副本。
//!   两个目录在后台服务启动时清空（`clear_copies`），不留下上次异常退出时的副本
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use rummage_proto::{ApiError, Event, ModelTaskKind, ModelTaskState, Reply, Request};
use tauri::{AppHandle, Listener, Manager, Runtime};
use tauri_plugin_rummage_android::{Android, Error};

use crate::core;
use crate::data::DataDir;
use crate::image_query::{ImageQuery, ImageTokens};
use crate::search::{error, unexpected};
use crate::setup::common_root;

/// 选中文件的副本目录（`<数据目录>/cache/<name>`）
fn copies_dir(data: &Path, name: &str) -> PathBuf {
    data.join("cache").join(name)
}

/// 删除整个副本目录
fn remove_dir(dir: &Path) {
    match fs::remove_dir_all(dir) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            log::warn!("无法清理选择文件时留下的副本：{err}")
        }
        _ => {}
    }
}

/// 清空导入模型与以图搜图的副本；后台服务启动时调用
pub fn clear_copies(data: &Path) {
    for name in ["import", "pick"] {
        remove_dir(&copies_dir(data, name));
    }
}

fn remove_files(files: &[PathBuf]) {
    for f in files {
        if let Err(err) = fs::remove_file(f) {
            log::warn!("无法删除选择文件时复制的副本 {}：{err}", f.display());
        }
    }
}

/// 插件调用失败：记录原因，返回 `message`
pub(super) fn failed(message: &str, err: Error) -> ApiError {
    log::warn!("{message}：{err}");
    error(ApiError::INTERNAL, message)
}

#[tauri::command]
pub async fn storage_access<R: Runtime>(app: AppHandle<R>) -> Result<bool, ApiError> {
    app.state::<Android<R>>()
        .all_files_access()
        .await
        .map_err(|err| failed("无法读取存储权限，请重试。", err))
}

/// 打开系统的“所有文件访问”授权页面，用户返回后报告是否已授权
#[tauri::command]
pub async fn request_storage_access<R: Runtime>(app: AppHandle<R>) -> Result<bool, ApiError> {
    app.state::<Android<R>>()
        .request_all_files_access()
        .await
        .map_err(|err| {
            failed(
                "无法打开授权页面，请在系统设置中为 Rummage 开启“所有文件访问”权限。",
                err,
            )
        })
}

async fn add<R: Runtime>(app: &AppHandle<R>, path: &Path) -> Result<(), ApiError> {
    let body = Request::AddRoot {
        path: path.to_string_lossy().into_owned(),
        excludes: Vec::new(),
    };
    match core::request(app, body).await? {
        Reply::Root { .. } => Ok(()),
        other => Err(unexpected(&other)),
    }
}

/// 选择文件夹并添加；取消选择时返回 false
#[tauri::command]
pub async fn add_root<R: Runtime>(app: AppHandle<R>) -> Result<bool, ApiError> {
    let picked = app
        .state::<Android<R>>()
        .pick_folder()
        .await
        .map_err(|err| failed("无法打开文件夹选择器，请重试。", err))?;
    let Some(path) = picked else {
        return Ok(false);
    };
    let Some(path) = path else {
        return Err(error(
            ApiError::BAD_REQUEST,
            "无法使用此位置：只支持手机存储和存储卡中的文件夹。",
        ));
    };
    add(&app, &path).await.map(|()| true)
}

/// 添加手机存储中的常用文件夹（`setup::common_root`）
#[tauri::command]
pub async fn add_common_root<R: Runtime>(app: AppHandle<R>, name: String) -> Result<(), ApiError> {
    let storage = app
        .state::<Android<R>>()
        .storage_root()
        .await
        .map_err(|err| failed("无法读取手机存储的位置，请重试。", err))?;
    add(&app, &common_root(&storage, &name)?).await
}

/// 导入任务结束后删除 `files`：核心在后台复制，请求返回时还在用它们
fn remove_after_import<R: Runtime>(app: &AppHandle<R>, files: Vec<PathBuf>) -> tauri::EventId {
    let files = Mutex::new(Some(files));
    let id = Arc::new(OnceLock::new());
    let own = id.clone();
    let handle = app.clone();
    let listener = app.listen_any("core-event", move |ev| {
        let Ok(Event::ModelTask(task)) = serde_json::from_str::<Event>(ev.payload()) else {
            return;
        };
        if task.kind != ModelTaskKind::Import || task.state == ModelTaskState::Running {
            return;
        }
        if let Some(files) = files.lock().unwrap_or_else(|err| err.into_inner()).take() {
            remove_files(&files);
        }
        if let Some(id) = own.get() {
            handle.unlisten(*id);
        }
    });
    let _ = id.set(listener);
    listener
}

/// 选择模型文件并导入；取消选择时返回 false。GGUF 没有专门的 MIME 类型，不按类型筛选，由核心按大小与 SHA-256 识别
#[tauri::command]
pub async fn import_models<R: Runtime>(app: AppHandle<R>) -> Result<bool, ApiError> {
    let dir = copies_dir(app.state::<DataDir>().dir()?, "import");
    let picked = app
        .state::<Android<R>>()
        .pick_files(&["*/*"], true, &dir)
        .await
        .map_err(|err| failed("无法读取所选文件，请重新选择。", err))?;
    let Some(paths) = picked else {
        return Ok(false);
    };
    let copies: Vec<PathBuf> = paths
        .iter()
        .filter(|p| p.starts_with(&dir))
        .cloned()
        .collect();
    // 先开始监听再发请求，任务很快结束时也不会错过结束事件
    let listener = (!copies.is_empty()).then(|| remove_after_import(&app, copies.clone()));
    let body = Request::ImportModels {
        paths: paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
    };
    let res = core::done(&app, body).await;
    if res.is_err()
        && let Some(id) = listener
    {
        app.unlisten(id);
        remove_files(&copies);
    }
    res.map(|()| true)
}

/// 选择以图搜图的图片；取消时为空
#[tauri::command]
pub async fn pick_image<R: Runtime>(app: AppHandle<R>) -> Option<ImageQuery> {
    let root = copies_dir(app.state::<DataDir>().dir().ok()?, "pick");
    // 每次复制到新的子目录，选好后才删除之前的副本：取消选择时当前的图片仍可用，副本的名称也不会带序号
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let dir = root.join(millis.to_string());
    let picked = app
        .state::<Android<R>>()
        .pick_files(&["image/*"], false, &dir)
        .await;
    let path = match picked {
        Ok(Some(paths)) => paths.into_iter().next(),
        Ok(None) => None,
        Err(err) => {
            log::warn!("无法读取所选图片：{err}");
            None
        }
    };
    let Some(path) = path else {
        remove_dir(&dir);
        return None;
    };
    if let Ok(entries) = fs::read_dir(&root) {
        for e in entries.flatten().filter(|e| e.path() != dir) {
            remove_dir(&e.path());
        }
    }
    Some(app.state::<ImageTokens>().insert(path))
}
