//! 首次使用引导的命令：把请求转给核心进程，拆开回复后返回。
//! - 添加文件夹、导入模型的路径都来自 Rust 侧的系统文件对话框（Android 上为系统的文件选择器，见 `android::storage`），
//!   直接交给核心，前端提交不了路径。
//! - 模型的下载、导入、校验、删除与设备自检在核心进程中进行，界面关闭后仍继续；进度经 `core-event` 推给前端。
//! - 参数的读写（`get_params`、`set_param`）由核心校验，引导中用来开关文件类型，设置页也用它们。
use std::collections::BTreeMap;
#[cfg(any(target_os = "android", test))]
use std::path::{Path, PathBuf};

use rummage_proto::{
    ApiError, Estimate, ModelSource, ParamValue, ProxySetting, Reply, Request, RootInfo,
    SetupStatus,
};
use tauri::{AppHandle, Runtime};

use crate::core;
use crate::search::{error, unexpected};

#[tauri::command]
pub async fn setup_status<R: Runtime>(app: AppHandle<R>) -> Result<SetupStatus, ApiError> {
    match core::request(&app, Request::SetupStatus).await? {
        Reply::Setup(s) => Ok(*s),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn finish_setup<R: Runtime>(app: AppHandle<R>) -> Result<(), ApiError> {
    core::done(&app, Request::FinishSetup).await?;
    // 开始建立索引，进度通知要用通知权限
    #[cfg(target_os = "android")]
    crate::android::background::request_notifications(&app).await;
    Ok(())
}

/// 记下引导进行到第几步，重启后从这一步继续
#[tauri::command]
pub async fn set_onboarding_step<R: Runtime>(app: AppHandle<R>, step: u32) -> Result<(), ApiError> {
    core::done(&app, Request::SetOnboardingStep { step }).await
}

#[tauri::command]
pub async fn list_roots<R: Runtime>(app: AppHandle<R>) -> Result<Vec<RootInfo>, ApiError> {
    match core::request(&app, Request::ListRoots).await? {
        Reply::Roots { roots } => Ok(roots),
        other => Err(unexpected(&other)),
    }
}

/// 选择文件夹并添加；取消选择时返回 false
#[cfg(desktop)]
#[tauri::command]
pub async fn add_root<R: Runtime>(app: AppHandle<R>) -> Result<bool, ApiError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    crate::file_dialog(&app, "选择要索引的文件夹").pick_folder(move |dir| {
        let _ = tx.send(dir);
    });
    let Some(dir) = rx.await.ok().flatten() else {
        return Ok(false);
    };
    let path = dir.into_path().map_err(|err| {
        log::warn!("无法读取所选文件夹的路径：{err}");
        error(
            ApiError::BAD_REQUEST,
            "无法读取所选文件夹的路径，请重新选择。",
        )
    })?;
    let body = Request::AddRoot {
        path: path.to_string_lossy().into_owned(),
        excludes: Vec::new(),
    };
    match core::request(&app, body).await? {
        Reply::Root { .. } => Ok(true),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn remove_root<R: Runtime>(app: AppHandle<R>, id: i64) -> Result<(), ApiError> {
    core::done(&app, Request::RemoveRoot { id }).await
}

#[tauri::command]
pub async fn get_params<R: Runtime>(
    app: AppHandle<R>,
) -> Result<BTreeMap<String, ParamValue>, ApiError> {
    match core::request(&app, Request::GetParams).await? {
        Reply::Params { values } => Ok(values),
        other => Err(unexpected(&other)),
    }
}

/// 保存成功时返回空；参数已保存、但有需要告诉用户的情况时返回提示
#[tauri::command]
pub async fn set_param<R: Runtime>(
    app: AppHandle<R>,
    key: String,
    value: ParamValue,
) -> Result<Option<String>, ApiError> {
    match core::request(&app, Request::SetParam { key, value }).await? {
        Reply::Done => Ok(None),
        Reply::DoneWithWarning { message } => Ok(Some(message)),
        other => Err(unexpected(&other)),
    }
}

/// 下载主模型，`mmproj` 为真时再下载图片与音视频模型
#[tauri::command]
pub async fn download_models<R: Runtime>(
    app: AppHandle<R>,
    source: ModelSource,
    mmproj: bool,
) -> Result<(), ApiError> {
    core::done(&app, Request::DownloadModels { source, mmproj }).await
}

/// 手机存储中可以一键添加的文件夹（Android 的 `add_common_root`）
#[cfg(any(target_os = "android", test))]
const COMMON_ROOTS: &[&str] = &["DCIM", "Pictures", "Download", "Documents"];

/// 手机存储 `storage` 中名为 `name` 的常用文件夹；名称不在 `COMMON_ROOTS` 中或文件夹不存在时返回错误
#[cfg(any(target_os = "android", test))]
pub(crate) fn common_root(storage: &Path, name: &str) -> Result<PathBuf, ApiError> {
    if !COMMON_ROOTS.contains(&name) {
        return Err(error(ApiError::BAD_REQUEST, "不支持添加这个文件夹。"));
    }
    let path = storage.join(name);
    if !path.is_dir() {
        return Err(error(
            ApiError::BAD_REQUEST,
            format!("手机存储中没有 {name} 文件夹。"),
        ));
    }
    Ok(path)
}

/// 选择已下载的模型文件并导入；取消选择时返回 false
#[cfg(not(target_os = "android"))]
#[tauri::command]
pub async fn import_models<R: Runtime>(app: AppHandle<R>) -> Result<bool, ApiError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let dialog = crate::file_dialog(&app, "选择模型文件").add_filter("GGUF 模型", &["gguf"]);
    dialog.pick_files(move |files| {
        let _ = tx.send(files);
    });
    let Some(files) = rx.await.ok().flatten() else {
        return Ok(false);
    };
    let mut paths = Vec::new();
    for f in files {
        match f.into_path() {
            Ok(p) => paths.push(p.to_string_lossy().into_owned()),
            Err(err) => log::warn!("无法读取所选文件的路径：{err}"),
        }
    }
    if paths.is_empty() {
        return Err(error(
            ApiError::BAD_REQUEST,
            "无法读取所选文件的路径，请重新选择。",
        ));
    }
    core::done(&app, Request::ImportModels { paths })
        .await
        .map(|()| true)
}

#[tauri::command]
pub async fn cancel_model_task<R: Runtime>(app: AppHandle<R>) -> Result<(), ApiError> {
    core::done(&app, Request::CancelModelTask).await
}

/// 完整校验一个已安装的模型文件，进度同下载
#[tauri::command]
pub async fn verify_model<R: Runtime>(app: AppHandle<R>, mmproj: bool) -> Result<(), ApiError> {
    core::done(&app, Request::VerifyModel { mmproj }).await
}

/// 依次完整校验已安装、没有校验记录的模型文件（引导中进入下载模型一步时）：文字模型，`mmproj` 为真时再加图片与音视频模型
#[tauri::command]
pub async fn verify_models<R: Runtime>(app: AppHandle<R>, mmproj: bool) -> Result<(), ApiError> {
    core::done(&app, Request::VerifyModels { mmproj }).await
}

/// 删除一个模型文件：`mmproj` 为真时删除图片与音视频模型，否则删除文字模型
#[tauri::command]
pub async fn delete_model<R: Runtime>(app: AppHandle<R>, mmproj: bool) -> Result<(), ApiError> {
    core::done(&app, Request::DeleteModel { mmproj }).await
}

#[tauri::command]
pub async fn set_proxy<R: Runtime>(
    app: AppHandle<R>,
    setting: ProxySetting,
) -> Result<ProxySetting, ApiError> {
    match core::request(&app, Request::SetProxy { setting }).await? {
        Reply::Proxy(saved) => Ok(saved),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn run_selftest<R: Runtime>(app: AppHandle<R>, onboarding: bool) -> Result<(), ApiError> {
    core::done(&app, Request::RunSelftest { onboarding }).await
}

#[tauri::command]
pub async fn set_preferred_device<R: Runtime>(
    app: AppHandle<R>,
    device: String,
) -> Result<(), ApiError> {
    core::done(&app, Request::SetPreferredDevice { device }).await
}

/// 解除屏蔽；没能当场重新检测时返回原因
#[tauri::command]
pub async fn unblock_gpu<R: Runtime>(
    app: AppHandle<R>,
    device: String,
) -> Result<Option<String>, ApiError> {
    match core::request(&app, Request::UnblockGpu { device }).await? {
        Reply::Done => Ok(None),
        Reply::DoneWithWarning { message } => Ok(Some(message)),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn index_estimate<R: Runtime>(app: AppHandle<R>) -> Result<Estimate, ApiError> {
    match core::request(&app, Request::Estimate).await? {
        Reply::Estimate(e) => Ok(e),
        other => Err(unexpected(&other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_roots() {
        let dir = tempfile::tempdir().unwrap();
        for name in COMMON_ROOTS {
            std::fs::create_dir(dir.path().join(name)).unwrap();
            assert_eq!(common_root(dir.path(), name), Ok(dir.path().join(name)));
        }
        // 只接受列出的名称，不接受其他文件夹、大小写不同的名称或路径
        std::fs::create_dir(dir.path().join("Music")).unwrap();
        for name in ["Music", "dcim", "", "..", "DCIM/..", "/DCIM"] {
            assert_eq!(
                common_root(dir.path(), name).unwrap_err().code,
                ApiError::BAD_REQUEST
            );
        }
        let empty = tempfile::tempdir().unwrap();
        let err = common_root(empty.path(), "Download").unwrap_err();
        assert_eq!(err.message, "手机存储中没有 Download 文件夹。");
    }
}
