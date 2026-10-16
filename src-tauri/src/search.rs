//! 搜索页的命令与 `media` 自定义协议。
//! - 命令把请求转给核心进程，拆开回复后返回；错误为核心的 `ApiError`（`code` 与中文 `message`）。
//! - 打开、在资源管理器中显示只接受索引中的文件 ID，路径由核心从索引中查出；不提供按任意路径打开的命令。
//!   Android 上的打开在 `android::ui` 中（界面不显示“在资源管理器中显示”）。
//!   网盘占位文件照常交给系统打开，由系统负责下载。
//! - 打开时不执行文件：`SCRIPT_EXTS` 中的脚本（含注册表文件与驱动安装信息）用 “edit” 动词在编辑器中打开，没有编辑动词时改为在资源管理器中显示；
//!   `EXEC_EXTS` 中的程序、安装包、快捷方式等（没有可看的文本内容）直接改为在资源管理器中显示。
//!   命令行与 MCP 没有打开文件的入口。
//! - 以图搜图只接受 `image_query` 发出的令牌，前端提交不了路径；“找相似的”只接受索引中的文件与片段 ID。
//!   以图搜图与图片的“找相似的”可附文字描述（`text`），由核心与图片合成一条查询。
//! - 筛选条（种类、根目录 ID、修改时间范围）原样转给核心，在粗排之前生效。
//! - 搜索历史存在核心，前端只在用户确认了一次文字搜索时记录，以图搜图不记录。
//! - 打开可带命中位置（`open_at`）：音视频的起点时刻、PDF 的页码。默认程序是已知程序时带跳转参数启动，
//!   启动失败或不是已知程序时按普通方式打开；`can_open_at` 让界面决定按钮写“打开到 03:12”还是“打开”。
//! - `media` 协议只按索引中的 ID 提供内容：`/thumb/<id>?v=<版本>` 是核心生成的缩略图（WebP），地址带文件的修改时间与大小，
//!   内容变了地址就变，因此长期缓存；`/frame/<id>/<毫秒>?v=<版本>` 是视频这一刻的画面、`/crop/<片段 ID>?v=<版本>`
//!   是长图命中那一段的裁剪图，同样由核心生成并缓存；`/image/<id>` 是预览用的原图，不缓存，只提供
//!   WebView2 能直接显示的格式，其他格式退回缩略图，非图片文件不提供；`/play/<id>` 是应用内播放的音视频，
//!   只提供 WebView2 能直接播放的格式（`play_mime`），按 HTTP Range 每次读一段。
//!   读取原图与音视频前的占位文件检查与读取在同一个阻塞任务中。
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use rummage_proto::{
    ApiError, FileInfo, IndexStatus, Reply, Request, SearchHit, SegmentPreview,
    SemanticUnavailable, SimilarTo, params,
};
use serde::{Deserialize, Serialize};
use tauri::http::{Response, StatusCode, header};
use tauri::{AppHandle, Manager, Runtime};

use crate::image_query::ImageTokens;
use crate::open_at::{self, OpenAt};
use crate::{core, platform};

/// 对应前端 `src/types.ts` 的 `SearchResults`
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Results {
    hits: Vec<SearchHit>,
    took_ms: u64,
    partial: bool,
    semantic_unavailable: bool,
    unavailable: Option<SemanticUnavailable>,
}

/// 用默认程序打开时可能被直接执行的脚本扩展名（小写）。Enter 或“打开”是为了查看内容，执行用户磁盘上的脚本
/// 可能造成破坏，所以这些文件改用 “edit” 动词在编辑器中打开：常见的脚本语言，系统默认会执行的几种 Windows 脚本，
/// 以及打开就会导入或安装的注册表文件与驱动安装信息（文本，用编辑器查看）
#[cfg(not(target_os = "android"))]
const SCRIPT_EXTS: &[&str] = &[
    "bat", "cmd", "ps1", "psm1", "vbs", "vbe", "js", "jse", "mjs", "cjs", "wsf", "wsh", "py",
    "pyw", "sh", "bash", "rb", "pl", "php", "lua", "reg", "inf",
];

/// 用默认程序打开就会运行、安装或导入的扩展名（小写）：程序与屏幕保护、控制面板项、安装包与补丁、应用包、
/// 管理单元、HTML 应用、Java 程序、快捷方式与网址快捷方式、ClickOnce、
/// 已编译的帮助（可执行脚本）、诊断包、扩展包等。打开它们只在资源管理器中显示
#[cfg(not(target_os = "android"))]
const EXEC_EXTS: &[&str] = &[
    "exe",
    "com",
    "scr",
    "pif",
    "cpl",
    "msi",
    "msp",
    "mst",
    "msc",
    "hta",
    "jar",
    "lnk",
    "url",
    "scf",
    "appref-ms",
    "application",
    "appinstaller",
    "msix",
    "msixbundle",
    "appx",
    "appxbundle",
    "settingcontent-ms",
    "chm",
    "diagcab",
    "xbap",
    "gadget",
    "vsix",
    "ws",
    "wsc",
];

pub(crate) fn error(code: &str, message: impl Into<String>) -> ApiError {
    ApiError {
        code: code.into(),
        message: message.into(),
    }
}

/// 回复类型与请求不对应，只会在两端版本不一致时出现（握手已拦下）
pub(crate) fn unexpected(reply: &Reply) -> ApiError {
    log::error!("核心进程的回复与请求不对应：{reply:?}");
    error(
        ApiError::INTERNAL,
        "后台服务的回复不正确，请重新启动 Rummage。",
    )
}

/// 搜索页的筛选条件，对应前端 `src/types.ts` 的 `SearchFilter`；为空的列表表示不限
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchFilter {
    kinds: Vec<String>,
    root_ids: Vec<i64>,
    /// 修改时间的起止（Unix 毫秒，含两端）
    since: Option<i64>,
    until: Option<i64>,
}

#[tauri::command]
pub async fn search<R: Runtime>(
    app: AppHandle<R>,
    text: Option<String>,
    image_token: Option<u64>,
    similar: Option<SimilarTo>,
    filter: Option<SearchFilter>,
) -> Result<Results, ApiError> {
    let image_path = match image_token {
        Some(token) => {
            let path = app
                .state::<ImageTokens>()
                .path(token)
                .ok_or_else(|| error(ApiError::BAD_REQUEST, "这张图片已失效，请重新选择。"))?;
            Some(path.to_string_lossy().into_owned())
        }
        None => None,
    };
    let filter = filter.unwrap_or_default();
    let body = Request::Search {
        text,
        image_path,
        similar,
        kinds: filter.kinds,
        root_ids: filter.root_ids,
        since: filter.since,
        until: filter.until,
        limit: None,
        snippets: false,
    };
    match core::request(&app, body).await? {
        Reply::Results {
            hits,
            took_ms,
            partial,
            semantic_unavailable,
            unavailable,
        } => Ok(Results {
            hits,
            took_ms,
            partial,
            semantic_unavailable,
            unavailable,
        }),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn index_status<R: Runtime>(app: AppHandle<R>) -> Result<IndexStatus, ApiError> {
    match core::request(&app, Request::IndexStatus).await? {
        Reply::Index(status) => Ok(status),
        other => Err(unexpected(&other)),
    }
}

/// 最近的文字搜索，新的在前；设置中关闭了保存搜索历史时为空
#[tauri::command]
pub async fn get_history<R: Runtime>(app: AppHandle<R>) -> Result<Vec<String>, ApiError> {
    match core::request(&app, Request::GetHistory).await? {
        Reply::History { queries } => Ok(queries),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn add_history<R: Runtime>(app: AppHandle<R>, query: String) -> Result<(), ApiError> {
    core::done(&app, Request::AddHistory { query }).await
}

#[tauri::command]
pub async fn remove_history<R: Runtime>(app: AppHandle<R>, query: String) -> Result<(), ApiError> {
    core::done(&app, Request::RemoveHistory { query }).await
}

#[tauri::command]
pub async fn clear_history<R: Runtime>(app: AppHandle<R>) -> Result<(), ApiError> {
    core::done(&app, Request::ClearHistory).await
}

/// 文档预览的片段正文与前后文；片段已不存在时为空
#[tauri::command]
pub async fn segment_text<R: Runtime>(
    app: AppHandle<R>,
    segment_id: i64,
) -> Result<Option<SegmentPreview>, ApiError> {
    match core::request(&app, Request::SegmentText { segment_id }).await? {
        Reply::Segment { segment } => Ok(segment),
        other => Err(unexpected(&other)),
    }
}

/// 详情栏的文件信息与索引情况
#[tauri::command]
pub async fn file_details<R: Runtime>(
    app: AppHandle<R>,
    file_id: i64,
) -> Result<FileInfo, ApiError> {
    file_info(&app, file_id).await
}

pub(crate) async fn file_info<R: Runtime>(
    app: &AppHandle<R>,
    file_id: i64,
) -> Result<FileInfo, ApiError> {
    match core::request(app, Request::FileInfo { file_id }).await? {
        Reply::File(info) => Ok(info),
        other => Err(unexpected(&other)),
    }
}

/// 打开或显示文件：设备未连接时直接说明；调用系统接口可能短暂阻塞，放在阻塞线程中
async fn shell<R: Runtime>(
    app: &AppHandle<R>,
    file_id: i64,
    action: impl FnOnce(&FileInfo) -> Result<(), String> + Send + 'static,
) -> Result<(), ApiError> {
    let info = file_info(app, file_id).await?;
    if info.offline {
        return Err(error(
            ApiError::BAD_REQUEST,
            "文件所在的设备未连接，请连接后重试。",
        ));
    }
    let res =
        tauri::async_runtime::spawn_blocking(move || action(&info).map_err(|err| (info.path, err)))
            .await
            .map_err(|err| error(ApiError::INTERNAL, format!("无法打开文件：{err}")))?;
    res.map_err(|(path, err)| {
        log::warn!("无法打开或显示 {path}：{err}");
        error(
            ApiError::BAD_REQUEST,
            "无法打开这个文件，它可能已被移动或删除。",
        )
    })
}

/// 扩展名（小写）在 `list` 中
#[cfg(not(target_os = "android"))]
fn ext_in(path: &Path, list: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| list.contains(&e.to_ascii_lowercase().as_str()))
}

fn reveal(path: &Path) -> Result<(), String> {
    tauri_plugin_opener::reveal_item_in_dir(path).map_err(|err| err.to_string())
}

/// 程序类文件在资源管理器中显示；脚本用 “edit” 动词打开，没有时在资源管理器中显示；其他文件用默认程序打开
#[cfg(not(target_os = "android"))]
fn open(path: &Path) -> Result<(), String> {
    if ext_in(path, EXEC_EXTS) {
        return reveal(path);
    }
    if !ext_in(path, SCRIPT_EXTS) {
        return tauri_plugin_opener::open_path(path, None::<&str>).map_err(|err| err.to_string());
    }
    platform::edit(path).or_else(|err| {
        log::info!(
            "{} 没有编辑动词（{err}），改为在资源管理器中显示",
            path.display()
        );
        reveal(path)
    })
}

/// 默认程序能跳到 `at` 时带参数启动并返回真；不能跳或启动失败时返回假，由调用方按普通方式打开
#[cfg(not(target_os = "android"))]
fn open_jump(info: &FileInfo, at: OpenAt) -> bool {
    let path = Path::new(&info.path);
    let Some(to) = open_at::target(&info.kind, &info.ext, at) else {
        return false;
    };
    let Some(exe) = platform::default_app(&info.ext) else {
        return false;
    };
    let Some(args) = open_at::args(&exe, path, to) else {
        return false;
    };
    match std::process::Command::new(&exe).args(args).spawn() {
        Ok(_) => true,
        Err(err) => {
            log::warn!("无法用 {} 打开到命中位置：{err}", exe.display());
            false
        }
    }
}

/// `at` 为命中位置（音视频的起点、PDF 的页码），默认程序支持时打开到那里
#[cfg(not(target_os = "android"))]
#[tauri::command]
pub async fn open_file<R: Runtime>(
    app: AppHandle<R>,
    file_id: i64,
    at: Option<OpenAt>,
) -> Result<(), ApiError> {
    shell(&app, file_id, move |info| {
        let path = Path::new(&info.path);
        if !ext_in(path, SCRIPT_EXTS)
            && !ext_in(path, EXEC_EXTS)
            && at.is_some_and(|at| open_jump(info, at))
        {
            return Ok(());
        }
        open(path)
    })
    .await
}

#[tauri::command]
pub async fn reveal_file<R: Runtime>(app: AppHandle<R>, file_id: i64) -> Result<(), ApiError> {
    shell(&app, file_id, |info| reveal(Path::new(&info.path))).await
}

/// 这个文件的默认程序能否跳到命中位置（音视频的时刻、PDF 的页码），界面据此决定按钮文案
#[tauri::command]
pub async fn can_open_at<R: Runtime>(app: AppHandle<R>, file_id: i64) -> Result<bool, ApiError> {
    let info = file_info(&app, file_id).await?;
    let probe = OpenAt {
        ms: Some(0),
        page: Some(1),
    };
    let Some(to) = open_at::target(&info.kind, &info.ext, probe) else {
        return Ok(false);
    };
    tauri::async_runtime::spawn_blocking(move || {
        platform::default_app(&info.ext)
            .is_some_and(|exe| open_at::args(&exe, Path::new(&info.path), to).is_some())
    })
    .await
    .map_err(|err| error(ApiError::INTERNAL, err.to_string()))
}

/// WebView2 能直接显示的图片格式及其 MIME 类型；其他格式预览时退回缩略图
fn web_mime(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "avif" => "image/avif",
        _ => return None,
    })
}

/// WebView2 能直接播放的音视频格式及其 MIME 类型；对应前端 `src/lib/media.ts` 的 `PLAYABLE`。
/// mkv、avi、wmv 等不在其中，预览时只显示画面与时间段。`.mov` 多为 H.264，按 MP4 交给 WebView 试播
fn play_mime(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "mp4" | "m4v" | "mov" => "video/mp4",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "wav" => "audio/wav",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "flac" => "audio/flac",
        _ => return None,
    })
}

/// 应用内播放时每次响应的最大字节数：WebView 按需再要后面的部分
const PLAY_CHUNK: u64 = 2 * 1024 * 1024;

/// 解析 `Range: bytes=起-止`（含两端，可省略一端），返回要读的 `[起, 止]`，不超过 `PLAY_CHUNK`；
/// 没有 Range 时从头读。超出文件末尾或写法不支持（多段等）时为空
fn byte_range(range: Option<&str>, len: u64) -> Option<(u64, u64)> {
    let (start, end) = match range {
        None => (0, len.checked_sub(1)?),
        Some(r) => {
            let (a, b) = r.trim().strip_prefix("bytes=")?.split_once('-')?;
            let (a, b) = (a.trim(), b.trim());
            match (a.is_empty(), b.is_empty()) {
                // 最后 n 个字节
                (true, false) => {
                    let n: u64 = b.parse().ok()?;
                    (len.checked_sub(n.min(len))?, len.checked_sub(1)?)
                }
                (false, _) => {
                    let start: u64 = a.parse().ok()?;
                    let end = if b.is_empty() {
                        u64::MAX
                    } else {
                        b.parse().ok()?
                    };
                    (start, end.min(len.checked_sub(1)?))
                }
                (true, true) => return None,
            }
        }
    };
    if start >= len || start > end {
        return None;
    }
    Some((start, end.min(start.saturating_add(PLAY_CHUNK - 1))))
}

/// 读出文件的一段，返回（内容, 起点, 文件长度）；读取前紧接着做占位文件检查。范围不成立时错误为 `None`
fn read_range(path: &Path, range: Option<&str>) -> Result<(Vec<u8>, u64, u64), Option<String>> {
    rummage_index::ensure_local(path).map_err(|err| Some(err.to_string()))?;
    let mut file = std::fs::File::open(path).map_err(|err| Some(format!("无法打开：{err}")))?;
    let len = file
        .metadata()
        .map_err(|err| Some(format!("无法读取大小：{err}")))?
        .len();
    let (start, end) = byte_range(range, len).ok_or(None)?;
    let mut buf = vec![0; (end - start + 1) as usize];
    file.seek(SeekFrom::Start(start))
        .and_then(|_| file.read_exact(&mut buf))
        .map_err(|err| Some(format!("无法读取：{err}")))?;
    Ok((buf, start, len))
}

async fn play<R: Runtime>(
    app: &AppHandle<R>,
    file_id: i64,
    range: Option<String>,
) -> Response<Vec<u8>> {
    let info = match file_info(app, file_id).await {
        Ok(info) => info,
        Err(err) => return not_found(&err.message),
    };
    let mime = match play_mime(&info.ext) {
        Some(m) if matches!(info.kind.as_str(), "audio" | "video") => m,
        _ => return not_found("应用内不能播放这种格式。"),
    };
    if info.offline {
        return not_found("文件所在的设备未连接。");
    }
    let res = tauri::async_runtime::spawn_blocking(move || {
        read_range(Path::new(&info.path), range.as_deref())
    })
    .await
    .unwrap_or_else(|err| Err(Some(err.to_string())));
    match res {
        Ok((bytes, start, len)) => Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_TYPE, mime)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{}/{len}", start + bytes.len() as u64 - 1),
            )
            .header(header::CACHE_CONTROL, "no-store")
            .body(bytes)
            .expect("头都是合法值"),
        Err(None) => Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .body(Vec::new())
            .expect("状态码是合法常量"),
        Err(Some(msg)) => {
            log::debug!("播放 {file_id} 失败：{msg}");
            not_found(&msg)
        }
    }
}

fn not_found(msg: &str) -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(msg.as_bytes().to_vec())
        .expect("状态码与头都是合法常量")
}

/// `cache` 为 `Cache-Control` 的值
fn image(mime: &str, cache: &str, bytes: Vec<u8>) -> Response<Vec<u8>> {
    Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(header::CACHE_CONTROL, cache)
        .body(bytes)
        .expect("头都是合法值")
}

/// 在阻塞线程中读取整个文件；`user_file` 为 true 时是用户的文件，读取前紧接着做占位文件检查
async fn read(path: String, user_file: bool) -> Result<Vec<u8>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if user_file {
            rummage_index::ensure_local(Path::new(&path)).map_err(|err| err.to_string())?;
        }
        std::fs::read(&path).map_err(|err| format!("无法读取 \"{path}\"：{err}"))
    })
    .await
    .map_err(|err| err.to_string())?
}

/// 核心生成的缩略图、视频画面帧或长图裁剪图
async fn cached_image<R: Runtime>(app: &AppHandle<R>, body: Request) -> Result<Vec<u8>, String> {
    match core::request(app, body).await {
        Ok(Reply::Thumbnail { path }) => read(path, false).await,
        Ok(other) => Err(unexpected(&other).message),
        Err(err) => Err(err.message),
    }
}

async fn original<R: Runtime>(
    app: &AppHandle<R>,
    file_id: i64,
) -> Result<(&'static str, Vec<u8>), String> {
    let info = file_info(app, file_id).await.map_err(|err| err.message)?;
    if info.kind != "image" {
        return Err("只能预览图片。".into());
    }
    let thumb = async || {
        let thumb = cached_image(app, Request::Thumbnail { file_id }).await?;
        Ok(("image/webp", thumb))
    };
    let Some(mime) = web_mime(&info.ext) else {
        return thumb().await;
    };
    // 超过图片大小上限的也用缩略图，不把整个大文件经界面送进 WebView
    let values = match core::request(app, Request::GetParams).await {
        Ok(Reply::Params { values }) => values,
        Ok(other) => return Err(unexpected(&other).message),
        Err(err) => return Err(err.message),
    };
    if info.size.max(0) as u64 > params::max_image_bytes(&values) {
        return thumb().await;
    }
    if info.offline {
        return Err("文件所在的设备未连接。".into());
    }
    Ok((mime, read(info.path, true).await?))
}

/// `media` 协议的地址
#[derive(Debug, PartialEq, Eq)]
enum Route {
    Thumb(i64),
    Image(i64),
    Play(i64),
    /// 文件 ID 与毫秒
    Frame(i64, u64),
    /// 片段 ID
    Crop(i64),
}

/// 拆出 `/thumb/<文件 ID>`、`/image/<文件 ID>`、`/play/<文件 ID>`、`/frame/<文件 ID>/<毫秒>`、`/crop/<片段 ID>`
fn route(path: &str) -> Option<Route> {
    let mut parts = path.strip_prefix('/')?.split('/');
    let name = parts.next()?;
    let id: i64 = parts.next()?.parse().ok()?;
    let ms = parts.next();
    if parts.next().is_some() {
        return None;
    }
    Some(match (name, ms) {
        ("thumb", None) => Route::Thumb(id),
        ("image", None) => Route::Image(id),
        ("play", None) => Route::Play(id),
        ("crop", None) => Route::Crop(id),
        ("frame", Some(ms)) => Route::Frame(id, ms.parse().ok()?),
        _ => return None,
    })
}

/// `media` 协议的请求处理；`path` 不含查询串（版本号只用于区分缓存），`range` 为请求的 `Range` 头
pub async fn media<R: Runtime>(
    app: AppHandle<R>,
    path: String,
    range: Option<String>,
) -> Response<Vec<u8>> {
    // 核心生成的图片地址带版本号，内容变了地址就变
    let forever = |b| ("image/webp", "max-age=31536000, immutable", b);
    let res = match route(&path) {
        Some(Route::Thumb(id)) => cached_image(&app, Request::Thumbnail { file_id: id })
            .await
            .map(forever),
        Some(Route::Frame(id, ms)) => cached_image(&app, Request::VideoFrame { file_id: id, ms })
            .await
            .map(forever),
        Some(Route::Crop(id)) => cached_image(&app, Request::RegionThumb { segment_id: id })
            .await
            .map(forever),
        // 原图的地址只有文件 ID，内容可能变化，不缓存
        Some(Route::Image(id)) => original(&app, id)
            .await
            .map(|(mime, b)| (mime, "no-store", b)),
        Some(Route::Play(id)) => return play(&app, id, range).await,
        None => return not_found("地址不正确。"),
    };
    match res {
        Ok((mime, cache, bytes)) => image(mime, cache, bytes),
        Err(msg) => {
            log::debug!("media 协议请求 {path} 失败：{msg}");
            not_found(&msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "android"))]
    #[test]
    fn scripts_not_executed() {
        for name in [
            "a.bat", "b.CMD", "c.ps1", "d.vbs", "e.js", "f.py", "g.sh", "h.wsf", "i.reg", "j.INF",
        ] {
            assert!(ext_in(Path::new(name), SCRIPT_EXTS), "{name}");
        }
        for name in ["a.exe", "b.MSI", "c.lnk", "e.appref-ms", "f.msix", "g.jar"] {
            assert!(ext_in(Path::new(name), EXEC_EXTS), "{name}");
        }
        for name in ["a.rs", "b.ts", "c.md", "d.txt", "e.jpg", "noext"] {
            assert!(!ext_in(Path::new(name), SCRIPT_EXTS), "{name}");
            assert!(!ext_in(Path::new(name), EXEC_EXTS), "{name}");
        }
    }

    #[test]
    fn ranges() {
        let mb = PLAY_CHUNK;
        assert_eq!(byte_range(None, 100), Some((0, 99)));
        assert_eq!(byte_range(Some("bytes=0-"), 100), Some((0, 99)));
        assert_eq!(byte_range(Some("bytes=10-19"), 100), Some((10, 19)));
        assert_eq!(byte_range(Some("bytes=90-200"), 100), Some((90, 99)));
        assert_eq!(byte_range(Some("bytes=-10"), 100), Some((90, 99)));
        assert_eq!(byte_range(Some("bytes=-500"), 100), Some((0, 99)));
        // 每次最多一段
        assert_eq!(byte_range(Some("bytes=0-"), 10 * mb), Some((0, mb - 1)));
        assert_eq!(byte_range(None, 10 * mb), Some((0, mb - 1)));
        for bad in [
            "bytes=100-",
            "bytes=5-2",
            "bytes=-",
            "bytes=a-",
            "items=0-1",
            "bytes=0-1,5-6",
        ] {
            assert_eq!(byte_range(Some(bad), 100), None, "{bad}");
        }
        assert_eq!(byte_range(None, 0), None);
    }

    #[test]
    fn read_ranges_of_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.mp3");
        std::fs::write(&path, (0u8..100).collect::<Vec<_>>()).unwrap();
        let (bytes, start, len) = read_range(&path, Some("bytes=10-14")).unwrap();
        assert_eq!((bytes, start, len), (vec![10, 11, 12, 13, 14], 10, 100));
        assert!(matches!(read_range(&path, Some("bytes=200-")), Err(None)));
        assert!(matches!(
            read_range(&dir.path().join("无.mp3"), None),
            Err(Some(_))
        ));
    }

    #[test]
    fn playable_formats() {
        assert_eq!(play_mime("mp4"), Some("video/mp4"));
        assert_eq!(play_mime("mp3"), Some("audio/mpeg"));
        assert_eq!(play_mime("mkv"), None);
        assert_eq!(play_mime("avi"), None);
    }

    #[test]
    fn web_formats() {
        assert_eq!(web_mime("jpg"), Some("image/jpeg"));
        assert_eq!(web_mime("avif"), Some("image/avif"));
        assert_eq!(web_mime("heic"), None);
        assert_eq!(web_mime("tif"), None);
    }

    #[test]
    fn media_routes() {
        assert_eq!(route("/thumb/12"), Some(Route::Thumb(12)));
        assert_eq!(route("/image/3"), Some(Route::Image(3)));
        assert_eq!(route("/play/4"), Some(Route::Play(4)));
        assert_eq!(route("/frame/5/192500"), Some(Route::Frame(5, 192_500)));
        assert_eq!(route("/crop/6"), Some(Route::Crop(6)));
        for path in [
            "/frame/5",
            "/frame/5/-1",
            "/frame/5/1/2",
            "/thumb/1/2",
            "/play/C:%5Ca.mp4",
            "/",
            "/thumb",
            "/thumb/",
            "/thumb/x",
            "/other/1",
            "/thumb/1/2",
            "/../1",
            "thumb/1",
            "/image/C:%5Ca.jpg",
        ] {
            assert_eq!(route(path), None, "{path}");
        }
    }
}
