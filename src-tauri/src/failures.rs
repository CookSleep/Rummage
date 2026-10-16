//! 失败文件列表的命令：转给核心进程。重试或忽略后核心立即广播新的进度，界面与托盘随之更新
use rummage_proto::{ApiError, FailedCursor, FailedFile, Reply, Request};
use serde::Serialize;
use tauri::{AppHandle, Runtime};

use crate::core;
use crate::search::unexpected;

/// 对应前端 `src/types.ts` 的 `FailedPage`
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedPage {
    files: Vec<FailedFile>,
    total: u64,
    retryable: u64,
}

/// 失败的文件，最近失败的在前；`after` 为上一页最后一行的游标，为空时从头取
#[tauri::command]
pub async fn failed_files<R: Runtime>(
    app: AppHandle<R>,
    after: Option<FailedCursor>,
    limit: u32,
) -> Result<FailedPage, ApiError> {
    match core::request(&app, Request::FailedFiles { after, limit }).await? {
        Reply::FailedFiles {
            files,
            total,
            retryable,
        } => Ok(FailedPage {
            files,
            total,
            retryable,
        }),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn retry_failed<R: Runtime>(app: AppHandle<R>, file_id: i64) -> Result<(), ApiError> {
    core::done(&app, Request::RetryFailed { file_id }).await
}

#[tauri::command]
pub async fn retry_all_failed<R: Runtime>(app: AppHandle<R>) -> Result<(), ApiError> {
    core::done(&app, Request::RetryAllFailed).await
}

/// 忽略一个失败的文件：加入所属文件夹的排除规则，并从索引中移除
#[tauri::command]
pub async fn ignore_failed<R: Runtime>(app: AppHandle<R>, file_id: i64) -> Result<(), ApiError> {
    core::done(&app, Request::IgnoreFailed { file_id }).await
}

#[tauri::command]
pub async fn ignore_all_failed<R: Runtime>(app: AppHandle<R>) -> Result<(), ApiError> {
    core::done(&app, Request::IgnoreAllFailed).await
}
