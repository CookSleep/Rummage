//! 设置页的命令：参数定义、根目录排除规则、外观与通知。除参数定义外都转给核心进程，
//! 拆开回复后返回。参数值的读写沿用引导的 `get_params`、`set_param`（`setup`）。
//! 排除规则只按根目录 ID 修改，前端提交不了路径。
use rummage_proto::{ApiError, Appearance, Notice, ParamDef, Reply, Request, params};
use tauri::{AppHandle, Runtime};

use crate::core;
use crate::search::unexpected;

/// 参数表（`rummage_proto::params::PARAMS`），设置页据此生成；界面与核心由同一版本编译，不经 IPC
#[tauri::command]
pub fn param_defs() -> Vec<ParamDef> {
    params::PARAMS.to_vec()
}

#[tauri::command]
pub async fn set_root_excludes<R: Runtime>(
    app: AppHandle<R>,
    id: i64,
    excludes: Vec<String>,
    skip_nomedia: Option<bool>,
) -> Result<(), ApiError> {
    let body = Request::SetRootExcludes {
        id,
        excludes,
        skip_nomedia,
    };
    core::done(&app, body).await
}

#[tauri::command]
pub async fn get_appearance<R: Runtime>(app: AppHandle<R>) -> Result<Appearance, ApiError> {
    match core::request(&app, Request::GetAppearance).await? {
        Reply::Appearance(a) => Ok(a),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn set_appearance<R: Runtime>(
    app: AppHandle<R>,
    appearance: Appearance,
) -> Result<(), ApiError> {
    core::done(&app, Request::SetAppearance { appearance }).await
}

#[tauri::command]
pub async fn get_notices<R: Runtime>(app: AppHandle<R>) -> Result<Vec<Notice>, ApiError> {
    match core::request(&app, Request::GetNotices).await? {
        Reply::Notices { notices } => Ok(notices),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn dismiss_notice<R: Runtime>(app: AppHandle<R>, notice: Notice) -> Result<(), ApiError> {
    core::done(&app, Request::DismissNotice { notice }).await
}
