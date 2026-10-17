//! 诊断日志与诊断包。
//! - 日志：警告和错误写入 `<数据目录>\logs\app.log`，同时输出到 stdout，Rust 侧日志转发到开发者工具控制台。
//!   数据目录不可用或日志文件不能追加写入时只输出到后两者。参数 `write_log_file` 只控制文件，
//!   核心连上后和参数变化时由 `core` 调用 `apply_params`；连上之前按开启处理。
//! - 打开日志目录：只打开日志目录本身，不向前端开放任意路径的打开权限。
//! - 诊断包：系统保存对话框在 Rust 侧弹出，只在本地生成 ZIP，不上传；Android 上先生成到缓存目录，
//!   再用系统的“创建文件”复制到所选位置（`android::storage::save_export`）。内容为日志、自检记录（`cache\selftest.json`）、
//!   软件与系统版本、显卡与驱动版本、设置（参数、外观、代理、文件夹、自检与索引状态）；不含搜索历史与索引内容。
//!   所有文本中地址里的用户名和密码一律遮盖（`mask_credentials`）；文件路径默认替换为占位符（`redact`），
//!   用户选择保留时原样写入。
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rummage_proto::{ApiError, ParamValue, Reply, Request};
use serde_json::{Value, json};
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Manager, Runtime, State};
use tauri_plugin_log::{RotationStrategy, Target, TargetKind, TimezoneStrategy, WEBVIEW_TARGET};

use crate::data::DataDir;
use crate::search::error;
use crate::zip::ZipWriter;
use crate::{core, platform};

/// 参数 `write_log_file`：只控制文件 target，控制台和开发者工具的转发不受影响
static FILE_ENABLED: AtomicBool = AtomicBool::new(true);

pub fn plugin<R: Runtime>(data_dir: Option<&Path>) -> TauriPlugin<R> {
    let builder = tauri_plugin_log::Builder::new()
        .clear_targets()
        .level(log::LevelFilter::Warn)
        .timezone_strategy(TimezoneStrategy::UseLocal)
        .max_file_size(5 * 1024 * 1024)
        // 单文件约 5 MB，当前文件之外最多保留 2 个历史文件
        .rotation_strategy(RotationStrategy::KeepSome(2))
        .target(Target::new(TargetKind::Stdout))
        // 前端日志在控制台已打印过，只转发 Rust 侧日志，既避免重复，也避免前端包装 console 后形成回环
        .target(Target::new(TargetKind::Webview).filter(|meta| {
            !matches!(meta.target().strip_prefix(WEBVIEW_TARGET), Some(rest) if rest.is_empty() || rest.starts_with("::"))
        }));
    // 插件初始化失败会导致应用无法启动，先确认日志文件可以追加写入，不可用时只输出到控制台。
    // 仍无法规避的情况：启动时 app.log 已超过 5 MB 需要改名、或旧的历史文件需要删除，而文件恰好被其他程序锁住
    let Some(data_dir) = data_dir else {
        return builder.build();
    };
    let dir = data_dir.join("logs");
    let file = fs::create_dir_all(&dir).and_then(|()| {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("app.log"))
    });
    match file {
        Ok(_) => builder.target(
            Target::new(TargetKind::Folder {
                path: dir,
                file_name: Some("app".into()),
            })
            .filter(|_| FILE_ENABLED.load(Ordering::Relaxed)),
        ),
        Err(err) => {
            eprintln!("[log] 日志文件不可写，日志不会写入文件：{err}");
            builder
        }
    }
    .build()
}

/// 按核心的参数开关日志文件
pub fn apply_params(values: &BTreeMap<String, ParamValue>) {
    let on = values.get("write_log_file") != Some(&ParamValue::Bool(false));
    FILE_ENABLED.store(on, Ordering::Relaxed);
}

#[tauri::command]
pub async fn open_log_dir(data: State<'_, DataDir>) -> Result<(), ApiError> {
    let dir = data.dir()?.join("logs");
    tauri::async_runtime::spawn_blocking(move || {
        fs::create_dir_all(&dir)?;
        tauri_plugin_opener::open_path(&dir, None::<&str>).map_err(std::io::Error::other)
    })
    .await
    .map_err(|err| error(ApiError::INTERNAL, format!("打开日志目录时出错：{err}")))?
    .map_err(|err| {
        log::warn!("无法打开日志目录：{err}");
        error(ApiError::INTERNAL, "无法打开日志目录，请稍后再试。")
    })
}

/// 路径在这些字符处结束：空白、引号、路径中不允许的字符，以及日志中常跟在路径后面的中文标点
fn ends_path(c: char) -> bool {
    c.is_whitespace() || "\"'<>|*?：，。；）」】、,;)]}".contains(c)
}

/// 盘符（`D:\`、`D:/`）或 UNC（`\\server`，JSON 中为 4 个反斜杠）路径在 `i` 处开始；前面必须是开头、空白、引号或标点，
/// 避免把 `<数据目录>\\cache` 这种转义或单词中的冒号误认作路径
fn path_start(chars: &[char], i: usize) -> bool {
    let prev = i.checked_sub(1).map(|p| chars[p]);
    if prev.is_some_and(char::is_alphanumeric) {
        return false;
    }
    let drive = chars[i].is_ascii_alphabetic()
        && chars.get(i + 1) == Some(&':')
        && matches!(chars.get(i + 2), Some('\\' | '/'));
    let slashes = chars[i..].iter().take_while(|&&c| c == '\\').count();
    let unc = matches!(slashes, 2 | 4)
        && prev.is_none_or(|c| ends_path(c) || "(（：:=[".contains(c))
        && chars.get(i + slashes).is_some_and(|c| c.is_alphanumeric());
    drive || unc
}

/// 已知位置在 `i` 处出现（不区分大小写），前面不是字母数字，后面是分隔符或路径的结尾；返回匹配的长度
fn known_at(chars: &[char], i: usize, pattern: &[char]) -> Option<usize> {
    let end = i + pattern.len();
    let same = chars
        .get(i..end)?
        .iter()
        .zip(pattern)
        .all(|(a, b)| a.to_lowercase().eq(b.to_lowercase()));
    let bounded = chars
        .get(end)
        .is_none_or(|&c| c == '\\' || c == '/' || ends_path(c));
    let free = !i.checked_sub(1).is_some_and(|p| chars[p].is_alphanumeric());
    (same && bounded && free).then_some(pattern.len())
}

/// 把文本中的文件路径换成占位符。先换已知的位置（`known` 为路径与占位符，长的在前），连同 JSON 中转义后的写法，
/// 不区分大小写；其余盘符或 UNC 开头的路径整段换成“<路径>”。路径中可能有空格，兜底时一直换到引号、换行、
/// 路径中不允许的字符、中文标点或下一个路径为止，宁可多换掉几个字，也不留下半截路径
pub(crate) fn redact(text: &str, known: &[(String, String)]) -> String {
    let patterns: Vec<(Vec<char>, &str)> = known
        .iter()
        .filter(|(path, _)| !path.is_empty())
        .flat_map(|(path, label)| {
            let path = path.trim_end_matches(['\\', '/']);
            [path.to_string(), path.replace('\\', r"\\")]
                .map(|p| (p.chars().collect::<Vec<_>>(), label.as_str()))
        })
        .collect();
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    'outer: while i < chars.len() {
        for (pattern, label) in &patterns {
            if let Some(len) = known_at(&chars, i, pattern) {
                out.push_str(label);
                i += len;
                continue 'outer;
            }
        }
        if !path_start(&chars, i) {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        i += 2;
        while i < chars.len() {
            let c = chars[i];
            if c == '\n' || c == '\r' || "\"'<>|*?：，。；）」】、".contains(c) {
                break;
            }
            // 下一个路径之前的那个空白留在原文中
            if c.is_whitespace() && i + 1 < chars.len() && path_start(&chars, i + 1) {
                break;
            }
            i += 1;
        }
        out.push_str("<路径>");
    }
    out
}

/// 所有文本中 `scheme://用户名:密码@` 形式的凭据换成 ***，不论是否保留路径
fn mask_credentials(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        let host_end = tail
            .find(|c: char| c == '/' || c.is_whitespace() || "\"'<>".contains(c))
            .unwrap_or(tail.len());
        match tail[..host_end].rfind('@') {
            Some(cred) => {
                out.push_str("***");
                rest = &tail[cred..];
            }
            None => rest = tail,
        }
    }
    out.push_str(rest);
    out
}

/// 代理地址中的用户名和密码换成 ***（设置中的代理地址可以不带 `http://`）
fn mask_proxy(url: &str) -> String {
    match url.rfind('@') {
        Some(at) => format!("***@{}", &url[at + 1..]),
        None => url.to_string(),
    }
}

/// 向核心取一项设置；失败时写入原因，诊断包照常生成
async fn fetch<R: Runtime>(app: &AppHandle<R>, body: Request) -> Value {
    match core::request(app, body).await {
        Ok(Reply::Params { values }) => json!(values),
        Ok(Reply::Appearance(a)) => json!(a),
        Ok(Reply::Setup(s)) => {
            let mut v = json!(s);
            v["proxy"]["url"] = json!(mask_proxy(&s.proxy.url));
            v
        }
        Ok(Reply::Roots { roots }) => json!(roots),
        Ok(Reply::Index(status)) => json!(status),
        Ok(other) => json!({ "error": format!("{other:?}") }),
        Err(err) => json!({ "error": err.message }),
    }
}

/// 生成诊断包的全部内容，在阻塞线程中执行
/// `known` 为空时保留文件路径
fn build(
    dir: &Path,
    settings: Value,
    system: Value,
    known: Option<&[(String, String)]>,
) -> std::io::Result<Vec<u8>> {
    let mut zip = ZipWriter::new(platform::local_time());
    let text = |s: &str| {
        let s = mask_credentials(s);
        match known {
            Some(known) => redact(&s, known),
            None => s,
        }
    };
    zip.add(
        "system.json",
        text(&serde_json::to_string_pretty(&system)?).as_bytes(),
    )?;
    zip.add(
        "settings.json",
        text(&serde_json::to_string_pretty(&settings)?).as_bytes(),
    )?;
    match fs::read(dir.join("cache").join("selftest.json")) {
        Ok(bytes) => zip.add(
            "selftest.json",
            text(&String::from_utf8_lossy(&bytes)).as_bytes(),
        )?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => log::warn!("诊断包中缺少自检记录：{err}"),
    }
    let mut logs: Vec<PathBuf> = fs::read_dir(dir.join("logs"))
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect()
        })
        .unwrap_or_default();
    logs.sort();
    for path in logs {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        match fs::read(&path) {
            Ok(bytes) => zip.add(
                &format!("logs/{name}"),
                text(&String::from_utf8_lossy(&bytes)).as_bytes(),
            )?,
            Err(err) => log::warn!("诊断包中缺少日志 {name}：{err}"),
        }
    }
    zip.finish()
}

/// 选择保存位置并导出诊断包，返回保存的路径（Android 上为所选文档的名称）；取消选择时返回空
#[tauri::command]
pub async fn export_diagnostics<R: Runtime>(
    app: AppHandle<R>,
    keep_paths: bool,
) -> Result<Option<String>, ApiError> {
    #[cfg(not(target_os = "android"))]
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let dialog = crate::file_dialog(&app, "导出诊断包")
            .set_file_name("Rummage 诊断包.zip")
            .add_filter("ZIP 压缩包", &["zip"]);
        dialog.save_file(move |path| {
            let _ = tx.send(path);
        });
        let Some(target) = rx.await.ok().flatten() else {
            return Ok(None);
        };
        let target = target.into_path().map_err(|err| {
            log::warn!("无法读取所选的保存位置：{err}");
            error(
                ApiError::BAD_REQUEST,
                "无法读取所选的保存位置，请重新选择。",
            )
        })?;
        let write = package(&app, keep_paths).await?;
        let saved = target.clone();
        tauri::async_runtime::spawn_blocking(move || write(&target))
            .await
            .map_err(|err| error(ApiError::INTERNAL, format!("生成诊断包时出错：{err}")))?
            .map_err(|err| {
                log::warn!("无法生成诊断包：{err}");
                error(
                    ApiError::INTERNAL,
                    "诊断包未能保存，请确认所选位置可以写入后重试。",
                )
            })?;
        Ok(Some(saved.display().to_string()))
    }
    // 系统的“创建文件”只给出文档地址，先生成到缓存目录再复制过去
    #[cfg(target_os = "android")]
    {
        let write = package(&app, keep_paths).await?;
        crate::android::storage::save_export(
            &app,
            "Rummage 诊断包.zip",
            "application/zip",
            "诊断包未能保存，请确认所选位置可以写入后重试。",
            write,
        )
        .await
    }
}

/// 收集诊断包的内容，返回把诊断包写到指定路径的函数（在阻塞线程中调用）
async fn package<R: Runtime>(
    app: &AppHandle<R>,
    keep_paths: bool,
) -> Result<impl FnOnce(&Path) -> std::io::Result<()> + Send + 'static, ApiError> {
    let dir = app.state::<DataDir>().dir()?.to_path_buf();
    let roots = fetch(app, Request::ListRoots).await;
    let settings = json!({
        "params": fetch(app, Request::GetParams).await,
        "appearance": fetch(app, Request::GetAppearance).await,
        "setup": fetch(app, Request::SetupStatus).await,
        "roots": roots,
        "index": fetch(app, Request::IndexStatus).await,
    });
    let system = json!({
        "appVersion": env!("CARGO_PKG_VERSION"),
        "runtime": rummage_embed::space::RUNTIME,
        "webview": tauri::webview_version().unwrap_or_else(|err| format!("未知（{err}）")),
        "os": platform::os_version(),
        "adapters": rummage_embed::adapters(),
        "createdAt": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64),
        "keepPaths": keep_paths,
    });
    let mut known = Vec::new();
    if !keep_paths {
        let roots = roots.as_array().cloned().unwrap_or_default();
        for (i, r) in roots.iter().enumerate() {
            if let Some(path) = r["path"].as_str() {
                known.push((path.to_string(), format!("<文件夹 {}>", i + 1)));
            }
        }
        known.push((dir.display().to_string(), "<数据目录>".into()));
        if let Ok(base) = rummage_datadir::base_dir() {
            known.push((base.display().to_string(), "<安装目录>".into()));
        }
        if let Some(home) = std::env::var_os("USERPROFILE") {
            known.push((home.to_string_lossy().into_owned(), "<用户目录>".into()));
        }
        // 长的在前：数据目录常在安装目录或用户目录之中
        known.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
    }
    Ok(move |target: &Path| {
        let bytes = build(&dir, settings, system, (!keep_paths).then_some(&known[..]))?;
        fs::write(target, bytes)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> Vec<(String, String)> {
        vec![
            (r"D:\Apps\Rummage\data".into(), "<数据目录>".into()),
            (r"D:\Apps\Rummage".into(), "<安装目录>".into()),
        ]
    }

    #[test]
    fn redacts_paths() {
        let mut known = known();
        known.insert(0, (r"E:\我的 照片".into(), "<文件夹 1>".into()));
        let cases = [
            (
                r"无法打开 d:\apps\RUMMAGE\data\index.db：拒绝访问",
                r"无法打开 <数据目录>\index.db：拒绝访问",
            ),
            (
                r#"{"cache": "D:\\Apps\\Rummage\\data\\cache"}"#,
                r#"{"cache": "<数据目录>\\cache"}"#,
            ),
            // 带空格的已知位置整段替换，其后的相对部分保留
            (
                r"无法生成 E:\我的 照片\2025 旅行\a.jpg 的缩略图",
                r"无法生成 <文件夹 1>\2025 旅行\a.jpg 的缩略图",
            ),
            // 名称只是前缀相同的不算已知位置，按兜底规则整段替换
            (r"读取 D:\Apps\Rummage2\x.log", "读取 <路径>"),
            // 兜底：带空格的路径一直换到行尾、引号、中文标点或下一个路径
            (
                "无法生成 F:\\照片\\2025 旅行\\a.jpg 的缩略图\n下一行",
                "无法生成 <路径>\n下一行",
            ),
            (r#"{"path": "F:\\照片 一\\a"}"#, r#"{"path": "<路径>"}"#),
            (r"从 F:\a b 复制到 G:\c d", "从 <路径> <路径>"),
            (r"（位置：F:\a b）", "（位置：<路径>）"),
            (r"共享 \\nas\照片，失败", "共享 <路径>，失败"),
            (r#"{"path": "\\\\nas\\照片"}"#, r#"{"path": "<路径>"}"#),
            (r"\\?\C:\Users\me\x", r"\\?\<路径>"),
            // 不是路径的冒号
            ("时间 12:30:00，版本 b11461", "时间 12:30:00，版本 b11461"),
            ("https://example.com/a", "https://example.com/a"),
        ];
        for (input, expected) in cases {
            assert_eq!(redact(input, &known), expected, "{input}");
        }
    }

    #[test]
    fn masks_credentials_in_urls() {
        assert_eq!(
            mask_credentials("代理 http://user:p@ss@127.0.0.1:7890/x 失败，https://a.com/@b"),
            "代理 http://***@127.0.0.1:7890/x 失败，https://a.com/@b"
        );
        assert_eq!(
            mask_credentials(r#""socks5://u:p@h""#),
            r#""socks5://***@h""#
        );
        assert_eq!(mask_credentials("没有地址"), "没有地址");
    }

    #[test]
    fn masks_proxy_credentials() {
        assert_eq!(
            mask_proxy("http://user:pass@127.0.0.1:7890"),
            "***@127.0.0.1:7890"
        );
        assert_eq!(mask_proxy("127.0.0.1:7890"), "127.0.0.1:7890");
    }

    #[test]
    fn builds_package_from_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("logs")).unwrap();
        fs::create_dir_all(dir.path().join("cache")).unwrap();
        let data = dir.path().display().to_string();
        fs::write(
            dir.path().join("logs").join("app.log"),
            format!("[WARN] 无法读取 {data}\\thumbs"),
        )
        .unwrap();
        fs::write(dir.path().join("logs").join("core.log"), "core").unwrap();
        fs::write(dir.path().join("cache").join("selftest.json"), "{}").unwrap();
        let known = vec![(data.clone(), "<数据目录>".to_string())];
        let bytes = build(dir.path(), json!({"params": {}}), json!({}), Some(&known)).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        // 文件名不压缩，可以直接在压缩包中找到
        for name in [
            "system.json",
            "settings.json",
            "selftest.json",
            "logs/app.log",
            "logs/core.log",
        ] {
            assert!(text.contains(name), "缺少 {name}");
        }
    }
}
