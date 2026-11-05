//! 以图搜图的查询图片：前端拿不到、也不能提交图片路径。
//! 系统文件对话框（`pick_image`）和拖入窗口（`on_drop`，主窗口的 `DragDrop` 事件）选中的图片路径留在 Rust 侧，
//! 换成递增的令牌，前端只收到 `{ token, name }`，搜索时提交令牌（`search::search`）。
//! 只保留最近 `KEEP` 个令牌，更早的失效，需要重新选择。拖入的文件中没有图片时推送 `null`，由前端提示。
//!
//! 粘贴的图片（`paste_image`）没有路径：前端在粘贴事件中取出图片内容，以 Base64 交过来，写到
//! `<数据目录>/cache/paste/<序号>/<文件名>` 后同样换成令牌。每次粘贴后删除令牌已失效的副本，启动时清空整个目录（`clear_pasted`）。
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine;
use rummage_proto::ApiError;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime, Window};

use crate::data::DataDir;
use crate::search::error;

/// 保留的令牌数
const KEEP: usize = 16;

/// 以图搜图可选的图片格式
const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "bmp", "tif", "tiff", "gif", "heic", "heif", "avif",
];

/// 对应前端 `src/types.ts` 的 `ImageQuery`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageQuery {
    token: u64,
    name: String,
}

/// (上一个发出的令牌, 令牌 → 路径)
#[derive(Default)]
pub struct ImageTokens(Mutex<(u64, HashMap<u64, PathBuf>)>);

impl ImageTokens {
    pub(crate) fn insert(&self, path: PathBuf) -> ImageQuery {
        let mut g = self.0.lock().unwrap_or_else(|err| err.into_inner());
        g.0 += 1;
        let token = g.0;
        let name = path
            .file_name()
            .map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy())
            .into_owned();
        g.1.insert(token, path);
        // 令牌递增，最小的就是最早的
        while g.1.len() > KEEP {
            let Some(&old) = g.1.keys().min() else {
                break;
            };
            g.1.remove(&old);
        }
        ImageQuery { token, name }
    }

    pub fn path(&self, token: u64) -> Option<PathBuf> {
        let g = self.0.lock().unwrap_or_else(|err| err.into_inner());
        g.1.get(&token).cloned()
    }

    /// 是否还有令牌指向 `dir` 中的图片
    fn in_use(&self, dir: &Path) -> bool {
        let g = self.0.lock().unwrap_or_else(|err| err.into_inner());
        g.1.values().any(|p| p.starts_with(dir))
    }
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// 选择以图搜图的图片；取消时为空（Android 上见 `android::storage`）
#[cfg(not(target_os = "android"))]
#[tauri::command]
pub async fn pick_image<R: Runtime>(app: tauri::AppHandle<R>) -> Option<ImageQuery> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let dialog = crate::file_dialog(&app, "选择图片").add_filter("图片", IMAGE_EXTS);
    dialog.pick_file(move |file| {
        let _ = tx.send(file);
    });
    let file = rx.await.ok().flatten()?;
    match file.into_path() {
        Ok(path) => Some(app.state::<ImageTokens>().insert(path)),
        Err(err) => {
            log::warn!("无法读取所选图片的路径：{err}");
            None
        }
    }
}

/// 文件拖入主窗口：取第一张图片换成令牌，以 `image-drop` 事件推给前端；没有图片时推送 `null`
pub fn on_drop<R: Runtime>(window: &Window<R>, paths: &[PathBuf]) {
    let query = paths
        .iter()
        .find(|p| is_image(p))
        .map(|p| window.state::<ImageTokens>().insert(p.clone()));
    if let Err(err) = window.emit("image-drop", query) {
        log::warn!("无法推送拖入的图片：{err}");
    }
}

fn paste_dir(data: &Path) -> PathBuf {
    data.join("cache").join("paste")
}

/// 清空粘贴图片的副本；启动时调用
pub fn clear_pasted(data: &Path) {
    match std::fs::remove_dir_all(paste_dir(data)) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            log::warn!("无法清理粘贴图片的副本：{err}")
        }
        _ => {}
    }
}

/// 粘贴图片的文件名：`name` 是原文件名（截图等没有文件名时为空），`mime` 是图片类型。
/// 去掉文件名中不能用的字符；没有图片扩展名时按类型补上，类型也不是图片时为空
fn paste_name(name: &str, mime: &str) -> Option<String> {
    let name: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let name = name.trim_matches(|c: char| c == '.' || c.is_whitespace());
    let name = if name.is_empty() {
        "粘贴的图片"
    } else {
        name
    };
    if is_image(Path::new(name)) {
        return Some(name.to_string());
    }
    let ext = match mime.strip_prefix("image/")? {
        "jpeg" => "jpg",
        other => other,
    };
    IMAGE_EXTS.contains(&ext).then(|| format!("{name}.{ext}"))
}

/// 粘贴的图片：`data` 是 Base64 编码的图片内容。
/// 不用原始字节：Android 的 IPC 走 postMessage，字节会被转成 JSON 数字数组，占用的内存是图片的几十倍
#[tauri::command]
pub async fn paste_image<R: Runtime>(
    app: AppHandle<R>,
    data: String,
    name: String,
    mime: String,
) -> Result<ImageQuery, ApiError> {
    let name =
        paste_name(&name, &mime).ok_or_else(|| error(ApiError::BAD_REQUEST, "请粘贴图片文件。"))?;
    let bytes = tauri::async_runtime::spawn_blocking(move || {
        base64::engine::general_purpose::STANDARD.decode(data)
    })
    .await
    .ok()
    .and_then(Result::ok)
    .filter(|b| !b.is_empty())
    .ok_or_else(|| error(ApiError::BAD_REQUEST, "无法读取粘贴的图片。"))?;
    // 每张图片放在单独的子目录中，文件名保持原样
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let root = paste_dir(app.state::<DataDir>().dir()?);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = root.join(seq.to_string());
    let path = dir.join(&name);
    let written = match tokio::fs::create_dir_all(&dir).await {
        Ok(()) => tokio::fs::write(&path, bytes).await,
        Err(err) => Err(err),
    };
    if let Err(err) = written {
        log::warn!("无法保存粘贴的图片：{err}");
        return Err(error(ApiError::INTERNAL, "无法保存粘贴的图片。"));
    }
    let tokens = app.state::<ImageTokens>();
    let query = tokens.insert(path);
    if let Ok(mut entries) = tokio::fs::read_dir(&root).await {
        while let Ok(Some(e)) = entries.next_entry().await {
            // 序号更大的是同时进行的另一次粘贴，可能已写好文件、还没换成令牌
            let older = e
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u64>().ok())
                .is_some_and(|n| n < seq);
            if older
                && !tokens.in_use(&e.path())
                && let Err(err) = tokio::fs::remove_dir_all(e.path()).await
            {
                log::warn!("无法删除已失效的粘贴图片：{err}");
            }
        }
    }
    Ok(query)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_names() {
        assert_eq!(paste_name("a.jpg", "image/jpeg").as_deref(), Some("a.jpg"));
        assert_eq!(paste_name("猫.PNG", "").as_deref(), Some("猫.PNG"));
        assert_eq!(
            paste_name("", "image/png").as_deref(),
            Some("粘贴的图片.png")
        );
        assert_eq!(
            paste_name("shot", "image/jpeg").as_deref(),
            Some("shot.jpg")
        );
        // 路径分隔符与上级目录不能离开子目录
        assert_eq!(paste_name(r"../..\a.png", "").as_deref(), Some("_.._a.png"));
        assert_eq!(
            paste_name("..", "image/png").as_deref(),
            Some("粘贴的图片.png")
        );
        assert_eq!(paste_name("a.txt", "text/plain"), None);
        assert_eq!(paste_name("a", "image/svg+xml"), None);
    }

    #[test]
    fn in_use_by_dir() {
        let t = ImageTokens::default();
        t.insert(PathBuf::from("/d/cache/paste/1/a.png"));
        assert!(t.in_use(Path::new("/d/cache/paste/1")));
        assert!(!t.in_use(Path::new("/d/cache/paste/2")));
    }

    #[test]
    fn tokens_bounded() {
        let t = ImageTokens::default();
        let first = t.insert(PathBuf::from(r"D:\图\a.jpg"));
        assert_eq!(first.name, "a.jpg");
        assert_eq!(t.path(first.token), Some(PathBuf::from(r"D:\图\a.jpg")));
        let mut last = first.clone();
        for i in 0..KEEP {
            last = t.insert(PathBuf::from(format!(r"D:\图\{i}.png")));
        }
        // 最早的令牌已失效，最近的仍有效
        assert_eq!(t.path(first.token), None);
        assert_eq!(
            t.path(last.token),
            Some(PathBuf::from(format!(r"D:\图\{}.png", KEEP - 1)))
        );
        assert_eq!(t.path(999), None);
    }

    #[test]
    fn image_extensions() {
        assert!(is_image(Path::new(r"D:\a.JPG")));
        assert!(is_image(Path::new("b.heic")));
        assert!(!is_image(Path::new("c.txt")));
        assert!(!is_image(Path::new("noext")));
    }
}
