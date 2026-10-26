//! 搜索页“文件夹筛选”的命令：读取与整体保存筛选（存在核心的索引数据库中），以及在某个已索引的文件夹中选择子文件夹。
//! - 子文件夹的路径来自 Rust 侧的系统文件对话框（Android 上为系统的文件选择器，见 `android::storage`），
//!   前端只拿到相对路径与结果状态，提交不了任意路径。
//! - 选中的位置交给核心定位（`LocateFolder`），按是否在该文件夹中、是否就是它本身、其下有没有已索引的文件报告状态。
use rummage_proto::{ApiError, FolderFilterEntry, Reply, Request};
use serde::Serialize;
use tauri::{AppHandle, Runtime};

use crate::core;
#[cfg(desktop)]
use crate::search::error;
use crate::search::unexpected;

/// 对应前端 `src/types.ts` 的 `FolderFilter`
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderFilter {
    custom: bool,
    entries: Vec<FolderFilterEntry>,
}

/// 选中的子文件夹，对应前端 `src/types.ts` 的 `PickedFolder`。`status`：`root` 就是该文件夹本身，
/// `outside` 不在该文件夹中（含在另一个已索引的文件夹中），`empty` 在其中但没有已索引的文件，`ok` 可以添加；
/// `rel` 为相对该文件夹的路径（`outside` 时为空），`path` 为选中的绝对路径
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PickedFolder {
    status: &'static str,
    rel: String,
    path: String,
}

#[tauri::command]
pub async fn get_folder_filter<R: Runtime>(app: AppHandle<R>) -> Result<FolderFilter, ApiError> {
    match core::request(&app, Request::GetFolderFilter).await? {
        Reply::FolderFilter { custom, entries } => Ok(FolderFilter { custom, entries }),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn set_folder_filter<R: Runtime>(
    app: AppHandle<R>,
    custom: bool,
    entries: Vec<FolderFilterEntry>,
) -> Result<(), ApiError> {
    core::done(&app, Request::SetFolderFilter { custom, entries }).await
}

/// 选中的绝对路径 `path` 相对已索引文件夹 `root_id` 的位置
pub(crate) async fn locate<R: Runtime>(
    app: &AppHandle<R>,
    root_id: i64,
    path: String,
) -> Result<PickedFolder, ApiError> {
    let body = Request::LocateFolder { path: path.clone() };
    let (scope, files) = match core::request(app, body).await? {
        Reply::FolderLocation { scope, files } => (scope, files),
        other => return Err(unexpected(&other)),
    };
    let Some(scope) = scope.filter(|s| s.root_id == root_id) else {
        return Ok(PickedFolder {
            status: "outside",
            rel: String::new(),
            path,
        });
    };
    let status = if scope.rel.is_empty() {
        "root"
    } else if files == 0 {
        "empty"
    } else {
        "ok"
    };
    Ok(PickedFolder {
        status,
        rel: scope.rel,
        path,
    })
}

/// 在已索引的文件夹 `root_id` 中选择子文件夹，对话框从该文件夹打开；取消选择时返回空
#[cfg(desktop)]
#[tauri::command]
pub async fn pick_subfolder<R: Runtime>(
    app: AppHandle<R>,
    root_id: i64,
) -> Result<Option<PickedFolder>, ApiError> {
    let roots = match core::request(&app, Request::ListRoots).await? {
        Reply::Roots { roots } => roots,
        other => return Err(unexpected(&other)),
    };
    let Some(root) = roots.into_iter().find(|r| r.id == root_id) else {
        return Err(error(
            ApiError::BAD_REQUEST,
            "此文件夹已不在索引中，请重新打开搜索页。",
        ));
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    crate::file_dialog(&app, "选择子文件夹")
        .set_directory(&root.path)
        .pick_folder(move |dir| {
            let _ = tx.send(dir);
        });
    let Some(dir) = rx.await.ok().flatten() else {
        return Ok(None);
    };
    let path = dir.into_path().map_err(|err| {
        log::warn!("无法读取所选文件夹的路径：{err}");
        error(
            ApiError::BAD_REQUEST,
            "无法读取所选文件夹的路径，请重新选择。",
        )
    })?;
    locate(&app, root_id, path.to_string_lossy().into_owned())
        .await
        .map(Some)
}
