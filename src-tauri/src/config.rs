//! 配置的导出与导入（文件格式与参数校验见 `rummage_proto::config`）。
//! - 导出：取当前的文件夹与参数生成配置，Windows 上用系统的“另存为”对话框保存，Android 上用系统的“创建文件”写入
//!   （`android::storage`）。
//! - 导入分两步：`import_config` 由 Rust 侧弹出选择框并读取所选文件（前端不能传路径），校验后记下待导入的内容，
//!   返回会改变的参数，界面据此决定是否先确认重建索引；`apply_config` 再保存参数、添加文件夹，
//!   都走核心原有的 `SetParam`、`AddRoot`，校验与添加后的扫描照旧。参数先于文件夹保存，新文件夹的首次扫描就按导入的文件类型。
//! - 文件夹：不存在的、无法访问的（Android 上没有“所有文件访问”权限时一律算无法访问）丢弃，已添加的跳过，只新增不删除。
//!   检查时先读属性，网盘占位文件夹不打开，交给核心按原有规则拒绝。
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use rummage_index::FsError;
use rummage_proto::config::{self, ConfigFolder, Imported};
use rummage_proto::{ApiError, ParamValue, Reply, Request};
use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime};

use crate::core;
use crate::platform;
use crate::search::{error, unexpected};

/// 配置文件大小的上限：远大于正常的配置，防止误选大文件时整个读进内存
const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;

/// `import_config` 校验过、等待 `apply_config` 的配置；参数只含会改变的
#[derive(Default)]
pub struct PendingConfig(Mutex<Option<Imported>>);

/// 导入前的预览，对应前端 `src/types.ts` 的 `ConfigPreview`
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    /// 会改变的参数
    params: BTreeMap<String, ParamValue>,
}

/// 导入的结果，对应前端 `src/types.ts` 的 `ConfigImport`
#[derive(Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    added: u32,
    /// 不存在而丢弃的文件夹
    missing: u32,
    /// 无法访问而丢弃的文件夹
    inaccessible: u32,
    /// 核心拒绝添加的文件夹（网盘文件夹、与已添加的文件夹重叠、排除规则写法不对等）
    failed: u32,
    /// 丢弃的参数：文件中不合法的，以及核心拒绝保存的
    dropped_params: u32,
    /// 参数已保存、但需要告诉用户的情况（没能安排重建索引）
    warning: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Access {
    Ok,
    Missing,
    Inaccessible,
}

/// 文件夹是否存在、能否访问。先读属性：占位文件夹不打开，算作存在，由核心拒绝
fn access(path: &Path) -> Access {
    if !path.is_absolute() {
        return Access::Missing;
    }
    match rummage_index::ensure_local(path) {
        Ok(()) => {}
        Err(FsError::Placeholder(_)) => return Access::Ok,
        Err(_) => return Access::Missing,
    }
    match fs::read_dir(path) {
        Ok(_) => Access::Ok,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Access::Missing,
        Err(_) => Access::Inaccessible,
    }
}

/// 要添加的文件夹：丢弃不存在、无法访问的，跳过已添加的（不区分大小写，同核心的比较）
fn plan(
    folders: Vec<ConfigFolder>,
    existing: &[String],
    check: impl Fn(&Path) -> Access,
) -> (Vec<ConfigFolder>, Outcome) {
    let same = |a: &str, b: &str| Path::new(&a.to_lowercase()) == Path::new(&b.to_lowercase());
    let mut out = Outcome::default();
    let mut add: Vec<ConfigFolder> = Vec::new();
    for f in folders {
        if existing.iter().any(|e| same(e, &f.path)) || add.iter().any(|a| same(&a.path, &f.path)) {
            continue;
        }
        match check(Path::new(&f.path)) {
            Access::Ok => add.push(f),
            Access::Missing => out.missing += 1,
            Access::Inaccessible => out.inaccessible += 1,
        }
    }
    (add, out)
}

/// 读出配置文件的内容；读之前检查占位文件属性，不触发网盘下载
pub(crate) fn read(path: &Path) -> Result<String, ApiError> {
    rummage_index::ensure_local(path)
        .map_err(|err| error(ApiError::BAD_REQUEST, err.to_string()))?;
    let too_large = fs::metadata(path).is_ok_and(|m| m.len() > MAX_CONFIG_BYTES);
    if too_large {
        return Err(error(ApiError::BAD_REQUEST, "这不是 Rummage 的配置文件。"));
    }
    let bytes = fs::read(path).map_err(|err| {
        log::warn!("无法读取配置文件 {}：{err}", path.display());
        error(ApiError::BAD_REQUEST, "无法读取所选文件，请重新选择。")
    })?;
    String::from_utf8(bytes)
        .map_err(|_| error(ApiError::BAD_REQUEST, "这不是 Rummage 的配置文件。"))
}

async fn current_params<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<BTreeMap<String, ParamValue>, ApiError> {
    match core::request(app, Request::GetParams).await? {
        Reply::Params { values } => Ok(values),
        other => Err(unexpected(&other)),
    }
}

async fn root_paths<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<Vec<(String, Vec<String>)>, ApiError> {
    match core::request(app, Request::ListRoots).await? {
        Reply::Roots { roots } => Ok(roots.into_iter().map(|r| (r.path, r.excludes)).collect()),
        other => Err(unexpected(&other)),
    }
}

/// 选择保存位置并导出配置；取消时返回 false
#[tauri::command]
pub async fn export_config<R: Runtime>(app: AppHandle<R>) -> Result<bool, ApiError> {
    let values = current_params(&app).await?;
    let folders = root_paths(&app)
        .await?
        .into_iter()
        .map(|(path, excludes)| ConfigFolder { path, excludes })
        .collect();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let file = config::export(
        &values,
        folders,
        std::env::consts::OS,
        env!("CARGO_PKG_VERSION"),
        now,
    );
    let text = serde_json::to_string_pretty(&file)
        .expect("配置只含字符串键的映射和可序列化的值，序列化不会失败");
    let [y, m, d, ..] = platform::local_time();
    let name = format!("Rummage 配置 {y:04}-{m:02}-{d:02}.json");
    save(&app, &name, text).await
}

/// 用系统的“创建文件”保存；取消时返回 false
#[cfg(target_os = "android")]
async fn save<R: Runtime>(app: &AppHandle<R>, name: &str, text: String) -> Result<bool, ApiError> {
    crate::android::storage::save_export(
        app,
        name,
        "application/json",
        "配置未能保存，请确认所选位置可以写入后重试。",
        move |path| fs::write(path, text),
    )
    .await
    .map(|saved| saved.is_some())
}

/// 用系统的“另存为”对话框保存；取消时返回 false
#[cfg(not(target_os = "android"))]
async fn save<R: Runtime>(app: &AppHandle<R>, name: &str, text: String) -> Result<bool, ApiError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    crate::file_dialog(app, "导出配置")
        .set_file_name(name)
        .add_filter("Rummage 配置", &["json"])
        .save_file(move |path| {
            let _ = tx.send(path);
        });
    let Some(target) = rx.await.ok().flatten() else {
        return Ok(false);
    };
    let target = target.into_path().map_err(|err| {
        log::warn!("无法读取所选的保存位置：{err}");
        error(
            ApiError::BAD_REQUEST,
            "无法读取所选的保存位置，请重新选择。",
        )
    })?;
    tauri::async_runtime::spawn_blocking(move || fs::write(&target, text))
        .await
        .map_err(|err| error(ApiError::INTERNAL, format!("保存配置时出错：{err}")))?
        .map_err(|err| {
            log::warn!("无法保存配置：{err}");
            error(
                ApiError::INTERNAL,
                "配置未能保存，请确认所选位置可以写入后重试。",
            )
        })?;
    Ok(true)
}

/// 用系统的打开文件对话框选择配置文件并读出内容；取消时为空
#[cfg(not(target_os = "android"))]
async fn pick<R: Runtime>(app: &AppHandle<R>) -> Result<Option<String>, ApiError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    crate::file_dialog(app, "导入配置")
        .add_filter("Rummage 配置", &["json"])
        .pick_file(move |path| {
            let _ = tx.send(path);
        });
    let Some(file) = rx.await.ok().flatten() else {
        return Ok(None);
    };
    let path = file.into_path().map_err(|err| {
        log::warn!("无法读取所选文件的路径：{err}");
        error(
            ApiError::BAD_REQUEST,
            "无法读取所选文件的路径，请重新选择。",
        )
    })?;
    tauri::async_runtime::spawn_blocking(move || read(&path))
        .await
        .map_err(|err| error(ApiError::INTERNAL, format!("读取配置文件时出错：{err}")))?
        .map(Some)
}

/// 用系统的文件选择器选择配置文件并读出内容；取消时为空
#[cfg(target_os = "android")]
async fn pick<R: Runtime>(app: &AppHandle<R>) -> Result<Option<String>, ApiError> {
    crate::android::storage::pick_config(app).await
}

/// 选择并校验配置文件，返回会改变的参数；取消时为空。校验不通过时回 `bad_request`，原因是给用户看的整句
#[tauri::command]
pub async fn import_config<R: Runtime>(app: AppHandle<R>) -> Result<Option<Preview>, ApiError> {
    // 没有权限时配置中的文件夹都会被当作无法访问丢弃，先请用户授权
    #[cfg(target_os = "android")]
    if !crate::android::storage::files_granted(&app).await {
        return Err(error(
            ApiError::BAD_REQUEST,
            "请先授予“所有文件访问”权限，再导入配置。",
        ));
    }
    let Some(text) = pick(&app).await? else {
        return Ok(None);
    };
    let mut cfg = config::parse(&text, std::env::consts::OS)
        .map_err(|err| error(ApiError::BAD_REQUEST, err.to_string()))?;
    let current = current_params(&app).await?;
    cfg.params.retain(|k, v| current.get(k) != Some(v));
    let preview = Preview {
        params: cfg.params.clone(),
    };
    *app.state::<PendingConfig>()
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(cfg);
    Ok(Some(preview))
}

/// 导入 `import_config` 记下的配置。核心不可用等与单项无关的错误直接返回，已导入的部分保留
#[tauri::command]
pub async fn apply_config<R: Runtime>(app: AppHandle<R>) -> Result<Outcome, ApiError> {
    let pending = app
        .state::<PendingConfig>()
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    let Some(cfg) = pending else {
        return Err(error(
            ApiError::BAD_REQUEST,
            "没有待导入的配置，请重新选择文件。",
        ));
    };
    let mut warning = None;
    let mut dropped = cfg.dropped;
    for (key, value) in cfg.params {
        match core::request(
            &app,
            Request::SetParam {
                key: key.clone(),
                value,
            },
        )
        .await
        {
            Ok(Reply::Done) => {}
            Ok(Reply::DoneWithWarning { message }) => warning = Some(message),
            Ok(other) => return Err(unexpected(&other)),
            Err(err) if err.code == ApiError::BAD_REQUEST => {
                log::warn!("导入配置时没能保存参数 {key}：{}", err.message);
                dropped += 1;
            }
            Err(err) => return Err(err),
        }
    }
    let existing: Vec<String> = root_paths(&app).await?.into_iter().map(|r| r.0).collect();
    #[cfg(target_os = "android")]
    let granted = crate::android::storage::files_granted(&app).await;
    #[cfg(not(target_os = "android"))]
    let granted = true;
    let (add, mut out) = tauri::async_runtime::spawn_blocking(move || {
        plan(cfg.folders, &existing, |p| {
            if granted {
                access(p)
            } else {
                Access::Inaccessible
            }
        })
    })
    .await
    .map_err(|err| error(ApiError::INTERNAL, format!("检查文件夹时出错：{err}")))?;
    for f in add {
        let body = Request::AddRoot {
            path: f.path.clone(),
            excludes: f.excludes,
        };
        match core::request(&app, body).await {
            Ok(Reply::Root { .. }) => out.added += 1,
            Ok(other) => return Err(unexpected(&other)),
            Err(err) if err.code == ApiError::BAD_REQUEST => {
                log::warn!("导入配置时没能添加文件夹 {}：{}", f.path, err.message);
                out.failed += 1;
            }
            Err(err) => return Err(err),
        }
    }
    out.dropped_params = dropped;
    out.warning = warning;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(path: &Path) -> ConfigFolder {
        ConfigFolder {
            path: path.display().to_string(),
            excludes: vec!["*.tmp".into()],
        }
    }

    #[test]
    fn drops_missing_folders_and_skips_added_ones() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let file = dir.path().join("f.txt");
        fs::write(&file, "x").unwrap();
        let folders = vec![
            folder(&a),
            folder(&b),
            // 配置中重复的只加一次
            folder(&b),
            folder(&dir.path().join("不存在")),
            ConfigFolder {
                path: "相对路径".into(),
                excludes: vec![],
            },
        ];
        // a 已添加，大小写不同也算同一个
        let existing = vec![a.display().to_string().to_uppercase()];
        let (add, out) = plan(folders, &existing, access);
        assert_eq!(add, vec![folder(&b)]);
        assert_eq!(out.missing, 2);
        assert_eq!(out.inaccessible, 0);
        // 文件不是文件夹，读不了目录
        assert_eq!(access(&file), Access::Inaccessible);
    }

    #[test]
    fn treats_all_folders_as_inaccessible_without_permission() {
        let dir = tempfile::tempdir().unwrap();
        let (add, out) = plan(vec![folder(dir.path())], &[], |_| Access::Inaccessible);
        assert!(add.is_empty());
        assert_eq!(out.inaccessible, 1);
    }

    #[test]
    fn reads_only_small_utf8_files() {
        let dir = tempfile::tempdir().unwrap();
        let ok = dir.path().join("ok.json");
        fs::write(&ok, "{}").unwrap();
        assert_eq!(read(&ok).unwrap(), "{}");
        let bad = dir.path().join("bad.json");
        fs::write(&bad, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(
            read(&bad).unwrap_err().message,
            "这不是 Rummage 的配置文件。"
        );
        let big = dir.path().join("big.json");
        fs::write(&big, vec![b' '; MAX_CONFIG_BYTES as usize + 1]).unwrap();
        assert_eq!(
            read(&big).unwrap_err().message,
            "这不是 Rummage 的配置文件。"
        );
        assert!(read(&dir.path().join("none.json")).is_err());
    }
}
