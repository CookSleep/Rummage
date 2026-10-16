//! 手动暂停与恢复建索引，界面与托盘共用：把“暂停 1 小时 / 到明天 / 直到手动恢复”
//! 换算成到期时间（“明天”为本地时间次日 0 点）后转发给核心。暂停状态以核心的 `IndexStatus` 为准，
//! 状态变化由核心以 `IndexProgress` 广播给界面；完成后再刷新一次托盘
use std::time::{SystemTime, UNIX_EPOCH};

use rummage_proto::{ApiError, Request};
use serde::Deserialize;
use tauri::{AppHandle, Runtime};

use crate::search::error;
use crate::{core, platform, tray};

/// 对应前端 `src/types.ts` 的 `PauseFor`
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PauseFor {
    Hour,
    Tomorrow,
    Manual,
}

async fn send<R: Runtime>(app: &AppHandle<R>, body: Request) -> Result<(), ApiError> {
    let res = core::done(app, body).await;
    // 暂停原因没有变化（或请求失败）时核心不广播进度，托盘自己取一次
    tray::refresh(app).await;
    res
}

pub async fn pause<R: Runtime>(app: &AppHandle<R>, until: PauseFor) -> Result<(), ApiError> {
    let until = match until {
        PauseFor::Hour => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as i64);
            Some(now + 3_600_000)
        }
        PauseFor::Tomorrow => Some(platform::next_midnight().map_err(|err| {
            log::error!("无法计算明天 0 点的时间：{err}");
            error(
                ApiError::INTERNAL,
                "无法计算明天 0 点的时间，请改用其他暂停方式。",
            )
        })?),
        PauseFor::Manual => None,
    };
    send(app, Request::PauseIndexing { until }).await
}

pub async fn resume<R: Runtime>(app: &AppHandle<R>) -> Result<(), ApiError> {
    send(app, Request::ResumeIndexing).await
}

#[tauri::command]
pub async fn pause_indexing<R: Runtime>(
    app: AppHandle<R>,
    until: PauseFor,
) -> Result<(), ApiError> {
    pause(&app, until).await
}

#[tauri::command]
pub async fn resume_indexing<R: Runtime>(app: AppHandle<R>) -> Result<(), ApiError> {
    resume(&app).await
}
