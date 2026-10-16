//! 以图搜图的查询图片：前端拿不到、也不能提交图片路径。
//! 系统文件对话框（`pick_image`）和拖入窗口（`on_drop`，主窗口的 `DragDrop` 事件）选中的图片路径留在 Rust 侧，
//! 换成递增的令牌，前端只收到 `{ token, name }`，搜索时提交令牌（`search::search`）。
//! 只保留最近 `KEEP` 个令牌，更早的失效，需要重新选择。拖入的文件中没有图片时推送 `null`，由前端提示。
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{Emitter, Manager, Runtime, Window};

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

#[cfg(test)]
mod tests {
    use super::*;

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
