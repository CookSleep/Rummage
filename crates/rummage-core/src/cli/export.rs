//! 把搜索结果保存到文件（命令行的 `--output`、MCP 的 `save_search_results`），方便 AI 或脚本据此整理文件。
//! - 格式按扩展名：`.csv`（UTF-8 带 BOM，Excel 直接打开不乱码）、`.json`（对象数组）、`.txt`（每行一个路径）。
//! - 每条结果的字段：`path`、`name`、`kind`、`size`（字节）、`modified`（本地时间）、`match`（匹配度 0–100，可能为空）、
//!   `status`（`local`，或 `cloud` 仅在云端、`offline` 设备未连接、`missing` 所在文件夹不存在）。
//! - CSV 中以 `=`、`+`、`-`、`@` 开头的值前面加 `'`，Excel 打开时不会当成公式。
//! - 只新建文件，目标已存在时报错，不会覆盖任何文件；写到一半失败时删掉写了一半的文件。
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;

use chrono::{Local, TimeZone};
use rummage_proto::SearchHit;

use super::kind_name;

pub const EXTENSIONS: [&str; 3] = ["csv", "json", "txt"];

#[derive(Debug)]
pub enum ExportError {
    /// 扩展名不是 `EXTENSIONS` 之一
    Format,
    Exists,
    Io(io::Error),
}

/// 小写的扩展名；不是 `EXTENSIONS` 之一时为空
pub fn ext(path: &Path) -> Option<&'static str> {
    let e = path.extension()?.to_string_lossy().to_lowercase();
    EXTENSIONS.into_iter().find(|x| *x == e)
}

fn modified(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map_or_else(String::new, |t| t.format("%Y-%m-%d %H:%M:%S").to_string())
}

fn status(h: &SearchHit) -> &'static str {
    if h.missing {
        "missing"
    } else if h.offline {
        "offline"
    } else if h.cloud {
        "cloud"
    } else {
        "local"
    }
}

fn csv_field(s: &str) -> String {
    let s = if s.starts_with(['=', '+', '-', '@']) {
        format!("'{s}")
    } else {
        s.to_string()
    };
    if s.contains([',', '"', '\r', '\n']) {
        return format!("\"{}\"", s.replace('"', "\"\""));
    }
    s
}

/// 文件内容，`ext` 为 `ext()` 返回的扩展名
pub fn render(hits: &[SearchHit], ext: &str) -> String {
    match ext {
        "csv" => {
            let mut out = String::from("\u{feff}path,name,kind,size,modified,match,status\r\n");
            for h in hits {
                let row = [
                    csv_field(&h.path),
                    csv_field(&h.name),
                    kind_name(&h.kind).to_string(),
                    h.size.to_string(),
                    modified(h.mtime),
                    h.match_pct.map(|p| p.to_string()).unwrap_or_default(),
                    status(h).to_string(),
                ];
                out.push_str(&row.join(","));
                out.push_str("\r\n");
            }
            out
        }
        "json" => {
            let items: Vec<_> = hits
                .iter()
                .map(|h| {
                    serde_json::json!({
                        "path": h.path,
                        "name": h.name,
                        "kind": kind_name(&h.kind),
                        "size": h.size,
                        "modified": modified(h.mtime),
                        "match": h.match_pct,
                        "status": status(h),
                    })
                })
                .collect();
            serde_json::to_string_pretty(&items)
                .expect("只含字符串键的对象与基本类型，序列化不会失败")
        }
        _ => hits.iter().map(|h| format!("{}\r\n", h.path)).collect(),
    }
}

/// 写入新文件
pub fn save(path: &Path, hits: &[SearchHit]) -> Result<(), ExportError> {
    let text = render(hits, ext(path).ok_or(ExportError::Format)?);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|err| match err.kind() {
            io::ErrorKind::AlreadyExists => ExportError::Exists,
            _ => ExportError::Io(err),
        })?;
    file.write_all(text.as_bytes()).map_err(|err| {
        drop(file);
        let _ = std::fs::remove_file(path);
        ExportError::Io(err)
    })
}
