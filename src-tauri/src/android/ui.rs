//! Android 界面用到的系统功能（Kotlin 部分见 crates/tauri-plugin-rummage-android 的 `StoragePlugin`）：
//! - `open_file`：用其他应用打开文件，同 Windows 只按索引中的文件 ID 打开，路径由后台服务查出；
//!   经 FileProvider 换成 content URI，只给这一次打开授予读取权限。命中位置（`at`）没有通用的传法，忽略。
//! - `share_file`：用系统分享面板分享文件，同样按文件 ID 查出路径，经 FileProvider 只给接收方授予读取权限。
//! - `move_task_to_back`：返回键在界面上已经没有可关闭的内容时把应用退到后台，不退出进程，后台建立索引继续。
//! - `safe_area`：全面屏的安全区，前端启动时取一次（之后的变化由 Kotlin 侧直接写进页面）
use rummage_proto::ApiError;
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_rummage_android::{Android, Insets};

use super::storage::failed;
use crate::search::{error, file_info};

#[tauri::command]
pub async fn open_file<R: Runtime>(app: AppHandle<R>, file_id: i64) -> Result<(), ApiError> {
    let info = file_info(&app, file_id).await?;
    if info.offline {
        return Err(error(
            ApiError::BAD_REQUEST,
            if info.missing {
                "找不到文件所在的文件夹，它可能已被删除、改名或移走。"
            } else {
                "文件所在的设备未连接，请连接后重试。"
            },
        ));
    }
    let opened = app
        .state::<Android<R>>()
        .open_file(info.path.as_ref())
        .await
        .map_err(|err| failed("无法打开这个文件，它可能已被移动或删除。", err))?;
    if opened {
        return Ok(());
    }
    Err(error(ApiError::BAD_REQUEST, "没有可以打开此文件的应用。"))
}

#[tauri::command]
pub async fn share_file<R: Runtime>(app: AppHandle<R>, file_id: i64) -> Result<(), ApiError> {
    let info = file_info(&app, file_id).await?;
    if info.offline {
        return Err(error(
            ApiError::BAD_REQUEST,
            if info.missing {
                "找不到文件所在的文件夹，它可能已被删除、改名或移走。"
            } else {
                "文件所在的设备未连接，请连接后重试。"
            },
        ));
    }
    app.state::<Android<R>>()
        .share_file(info.path.as_ref())
        .await
        .map_err(|err| failed("无法分享这个文件，它可能已被移动或删除。", err))
}

#[tauri::command]
pub async fn move_task_to_back<R: Runtime>(app: AppHandle<R>) -> Result<(), ApiError> {
    app.state::<Android<R>>()
        .move_to_back()
        .await
        .map_err(|err| failed("无法把应用退到后台。", err))
}

#[tauri::command]
pub async fn safe_area<R: Runtime>(app: AppHandle<R>) -> Result<Insets, ApiError> {
    app.state::<Android<R>>()
        .safe_area()
        .await
        .map_err(|err| failed("无法读取安全区。", err))
}
