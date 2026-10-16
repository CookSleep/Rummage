//! 设置页“数据”的命令：
//! - 占用统计在阻塞线程中遍历数据目录：索引数据库（`index.db` 及 WAL、迁移备份）、缩略图、模型、日志，其余算作“其他”。
//! - 打开目录只打开数据目录本身。
//! - 更改位置：`data_dir_pick` 用 Rust 侧的目录选择框选择新目录，用 `rummage_datadir::check` 校验（被索引的文件夹由
//!   核心给出，核心未连接时不检查这一条，扫描本来也会跳过数据目录），通过后记下所选目录并返回数据大小；
//!   前端确认后 `data_dir_change` 写入待迁移标记、请核心退出并重启，由下次启动完成迁移（进度框见 `rummage_datadir`）。
//!   前端只能确认已选的目录，提交不了路径。迁移结果在前端加载后由 `data_dir_notices` 取一次。
//! - “索引数据库无法打开”：`db_status` 报告原因与清除能否解决；`reset_index` 只在核心报告可以重建时，删除数据目录中的
//!   `index.db` 与 WAL、共享内存文件（数据库打不开时核心不持有这些文件），迁移备份保留，可以事后手动替换回去；
//!   再请核心退出，由守护重新拉起并建立新库。新库没有文件夹和设置，前端随之回到首次使用引导；模型、缩略图与自检记录不受影响。
//! - 清空索引转给核心。
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rummage_datadir::{Notice, NoticeKind};
use rummage_proto::{ApiError, Reply, Request};
use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime, State};

use crate::core;
use crate::search::{error, unexpected};

pub struct DataDir {
    /// 本次运行使用的数据目录；无法确定时为空
    dir: Option<PathBuf>,
    /// 启动时确定数据目录的结果，前端取走后清空
    notices: Mutex<Vec<Notice>>,
    /// 已选择、待确认的新数据目录
    picked: Mutex<Option<PathBuf>>,
}

const LOCK: &str = "持锁期间只做读写字段，不会 panic";

impl DataDir {
    pub fn new(dir: Option<PathBuf>, notices: Vec<Notice>) -> Self {
        Self {
            dir,
            notices: Mutex::new(notices),
            picked: Mutex::new(None),
        }
    }

    pub fn dir(&self) -> Result<&Path, ApiError> {
        self.dir.as_deref().ok_or_else(|| {
            error(
                ApiError::UNAVAILABLE,
                "数据目录不可用，请重新启动 Rummage。",
            )
        })
    }
}

/// 对应前端 `src/types.ts` 的 `StorageInfo`，单位为字节
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageInfo {
    data_dir: String,
    db: u64,
    thumbs: u64,
    models: u64,
    logs: u64,
    other: u64,
}

/// 对应前端 `src/types.ts` 的 `DataDirChange`
#[cfg(desktop)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataDirChange {
    from: String,
    to: String,
    files: u64,
    bytes: u64,
    /// 不能使用所选目录的原因
    blocked: Option<String>,
}

/// 对应前端 `src/types.ts` 的 `StartupNotice`
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupNotice {
    success: bool,
    title: String,
    message: String,
    paths: Vec<(String, String)>,
    details: Vec<String>,
}

/// 对应前端 `src/types.ts` 的 `DbStatus`
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbStatus {
    error: Option<String>,
    rebuildable: bool,
}

/// 索引数据库的主文件与 WAL、共享内存文件；清除并重建时只删这些
fn is_db_file(name: &str) -> bool {
    name == "index.db" || name.starts_with("index.db-")
}

/// 迁移备份（`index.db.v<N>.bak`），占用统计中算作索引数据库，清除时保留
fn is_db_backup(name: &str) -> bool {
    name.starts_with("index.db.v") && name.ends_with(".bak")
}

fn blocking_error(err: impl std::fmt::Display) -> ApiError {
    log::error!("操作异常结束：{err}");
    error(ApiError::INTERNAL, "操作未能完成，请重试。")
}

#[tauri::command]
pub async fn storage_info(data: State<'_, DataDir>) -> Result<StorageInfo, ApiError> {
    let dir = data.dir()?.to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        let size = |name: &str| rummage_datadir::dir_stats(&dir.join(name)).1;
        let db: u64 = fs::read_dir(&dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        is_db_file(&name) || is_db_backup(&name)
                    })
                    .filter_map(|e| e.metadata().ok())
                    .filter(|m| m.is_file())
                    .map(|m| m.len())
                    .sum()
            })
            .unwrap_or(0);
        let (thumbs, models, logs) = (size("thumbs"), size("models"), size("logs"));
        let total = rummage_datadir::dir_stats(&dir).1;
        StorageInfo {
            data_dir: dir.display().to_string(),
            db,
            thumbs,
            models,
            logs,
            other: total.saturating_sub(db + thumbs + models + logs),
        }
    })
    .await
    .map_err(blocking_error)
}

#[tauri::command]
pub async fn open_data_dir(data: State<'_, DataDir>) -> Result<(), ApiError> {
    let dir = data.dir()?.to_path_buf();
    tauri::async_runtime::spawn_blocking(move || tauri_plugin_opener::open_path(&dir, None::<&str>))
        .await
        .map_err(blocking_error)?
        .map_err(|err| {
            log::warn!("无法打开数据目录：{err}");
            error(ApiError::INTERNAL, "无法打开数据目录，请稍后再试。")
        })
}

/// 被索引的文件夹，更改数据目录时不能选在其中；核心未连接时为空
async fn indexed<R: Runtime>(app: &AppHandle<R>) -> Vec<PathBuf> {
    match core::request(app, Request::ListRoots).await {
        Ok(Reply::Roots { roots }) => roots.into_iter().map(|r| PathBuf::from(r.path)).collect(),
        other => {
            log::warn!("更改数据目录时无法取得被索引的文件夹，不检查这一条：{other:?}");
            Vec::new()
        }
    }
}

/// 选择新数据目录：所选目录不能使用时 `blocked` 为原因；取消选择时返回空
#[cfg(desktop)]
#[tauri::command]
pub async fn data_dir_pick<R: Runtime>(
    app: AppHandle<R>,
) -> Result<Option<DataDirChange>, ApiError> {
    let from = app.state::<DataDir>().dir()?.to_path_buf();
    app.state::<DataDir>().picked.lock().expect(LOCK).take();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = crate::file_dialog(&app, "选择新的数据目录");
    if let Some(parent) = from.parent() {
        dialog = dialog.set_directory(parent);
    }
    dialog.pick_folder(move |dir| {
        let _ = tx.send(dir);
    });
    let Some(to) = rx.await.ok().flatten() else {
        return Ok(None);
    };
    let to = to.into_path().map_err(|err| {
        log::warn!("无法读取所选文件夹的路径：{err}");
        error(
            ApiError::BAD_REQUEST,
            "无法读取所选文件夹的路径，请重新选择。",
        )
    })?;
    let indexed = indexed(&app).await;
    let (dir, target) = (from.clone(), to.clone());
    let checked = tauri::async_runtime::spawn_blocking(move || {
        rummage_datadir::check(&target, &dir, &indexed).map(|()| rummage_datadir::dir_stats(&dir))
    })
    .await
    .map_err(blocking_error)?;
    let ((files, bytes), blocked) = match checked {
        Ok(stats) => {
            *app.state::<DataDir>().picked.lock().expect(LOCK) = Some(to.clone());
            (stats, None)
        }
        Err(err) => ((0, 0), Some(err.to_string())),
    };
    Ok(Some(DataDirChange {
        from: from.display().to_string(),
        to: to.display().to_string(),
        files,
        bytes,
        blocked,
    }))
}

/// 确认更改：写入待迁移标记，请核心退出后重启，由下次启动完成迁移
#[tauri::command]
pub async fn data_dir_change<R: Runtime>(app: AppHandle<R>) -> Result<(), ApiError> {
    let from = app.state::<DataDir>().dir()?.to_path_buf();
    let to = app
        .state::<DataDir>()
        .picked
        .lock()
        .expect(LOCK)
        .take()
        .ok_or_else(|| {
            error(
                ApiError::BAD_REQUEST,
                "没有待确认的新数据目录，请重新选择。",
            )
        })?;
    let indexed = indexed(&app).await;
    tauri::async_runtime::spawn_blocking(move || {
        rummage_datadir::request_change(&to, &from, &indexed)
    })
    .await
    .map_err(blocking_error)?
    .map_err(|err| error(ApiError::BAD_REQUEST, err.to_string()))?;
    core::stop(&app).await;
    app.request_restart();
    Ok(())
}

/// 启动时确定数据目录的结果（迁移成功或失败、改用默认目录等），只返回一次
#[tauri::command]
pub fn data_dir_notices(data: State<'_, DataDir>) -> Vec<StartupNotice> {
    std::mem::take(&mut *data.notices.lock().expect(LOCK))
        .into_iter()
        .map(|n| StartupNotice {
            success: n.kind == NoticeKind::Success,
            title: n.title,
            message: n.message,
            paths: n.paths,
            details: n.details,
        })
        .collect()
}

async fn db_state<R: Runtime>(app: &AppHandle<R>) -> Result<DbStatus, ApiError> {
    match core::request(app, Request::DbStatus).await? {
        Reply::DbStatus { error, rebuildable } => Ok(DbStatus { error, rebuildable }),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn db_status<R: Runtime>(app: AppHandle<R>) -> Result<DbStatus, ApiError> {
    db_state(&app).await
}

/// 清除无法打开的索引数据库并重建；数据库可用或清除也无济于事时拒绝
#[tauri::command]
pub async fn reset_index<R: Runtime>(app: AppHandle<R>) -> Result<(), ApiError> {
    let status = db_state(&app).await?;
    if status.error.is_none() || !status.rebuildable {
        return Err(error(
            ApiError::BAD_REQUEST,
            "索引数据库的问题无法通过清除解决，没有删除任何数据。",
        ));
    }
    let dir = app.state::<DataDir>().dir()?.to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        for entry in fs::read_dir(&dir)?.flatten() {
            if is_db_file(&entry.file_name().to_string_lossy()) {
                fs::remove_file(entry.path())?;
            }
        }
        Ok::<_, std::io::Error>(())
    })
    .await
    .map_err(blocking_error)?
    .map_err(|err| {
        log::error!("无法删除索引数据库：{err}");
        error(
            ApiError::UNAVAILABLE,
            "无法删除索引数据库，它可能被其他程序占用，请关闭相关程序后重试。",
        )
    })?;
    log::warn!("用户清除了无法打开的索引数据库，重启核心进程后重建。");
    // 核心退出后由守护重新拉起，新核心打开的是新建的空库
    if let Err(err) = core::request(&app, Request::Shutdown).await {
        log::warn!("请核心进程重启时出错：{}", err.message);
    }
    Ok(())
}

#[tauri::command]
pub async fn clear_index<R: Runtime>(app: AppHandle<R>) -> Result<(), ApiError> {
    match core::request(&app, Request::ClearIndex).await? {
        Reply::Done => Ok(()),
        other => Err(unexpected(&other)),
    }
}

/// 连同核心一起退出（`core::quit`）：“索引数据库无法打开”时（用户手动替换数据库后再次打开时由新核心重新打开），
/// 以及设置为关闭窗口时退出的关窗
#[tauri::command]
pub async fn exit_app<R: Runtime>(app: AppHandle<R>) {
    core::quit(&app).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_files_recognized() {
        for name in ["index.db", "index.db-wal", "index.db-shm"] {
            assert!(is_db_file(name) && !is_db_backup(name), "{name}");
        }
        assert!(is_db_backup("index.db.v2.bak") && !is_db_file("index.db.v2.bak"));
        for name in ["index.dbx", "core.token", "index.db.v2.bak.tmp", "thumbs"] {
            assert!(!is_db_file(name) && !is_db_backup(name), "{name}");
        }
    }
}
