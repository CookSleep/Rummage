//! 命令行的输出：给人看的中文文本，以及 `--json`（字段同 `rummage-proto`，带协议版本号 `protocol`）。
//! JSON 把非 ASCII 字符转义为 `\uXXXX`：Windows PowerShell 5.1 按控制台代码页（中文系统为 936）解码外部程序的输出，
//! UTF-8 的中文会乱码，纯 ASCII 在任何代码页下都不变，`ConvertFrom-Json` 再还原成中文。
use std::fmt::Write as _;
use std::io;
use std::path::Path;

use chrono::{Local, TimeZone};
use rummage_proto::{
    FileInfo, IndexState, IndexStatus, PROTOCOL_VERSION, PauseReason, RootInfo, SearchHit,
    SegmentPreview, SemanticUnavailable,
};
use serde::Serialize;

use crate::manage::{clock, progress_pct};

pub struct Results<'a> {
    pub hits: &'a [SearchHit],
    pub took_ms: u64,
    pub partial: bool,
    pub semantic_unavailable: bool,
    pub unavailable: Option<SemanticUnavailable>,
    /// 没有显示的匹配度较低的结果数；只数核心按数量上限返回的那些，实际可能更多，所以是“至少”
    pub weak_omitted: usize,
    /// 以图搜图
    pub image: bool,
}

pub struct Status<'a> {
    pub core_version: &'a str,
    pub onboarded: bool,
    pub index: &'a IndexStatus,
    pub roots: &'a [RootInfo],
}

/// 非 ASCII 字符写成 `\uXXXX`（超出基本平面的写成代理对），其余同 serde_json 的紧凑格式
struct Ascii;

impl serde_json::ser::Formatter for Ascii {
    fn write_string_fragment<W: ?Sized + io::Write>(
        &mut self,
        w: &mut W,
        fragment: &str,
    ) -> io::Result<()> {
        for c in fragment.chars() {
            if c.is_ascii() {
                w.write_all(&[c as u8])?;
                continue;
            }
            let mut buf = [0u16; 2];
            for u in c.encode_utf16(&mut buf) {
                write!(w, "\\u{u:04x}")?;
            }
        }
        Ok(())
    }
}

pub fn ascii_json(v: &impl Serialize) -> String {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, Ascii);
    v.serialize(&mut ser)
        .expect("输出的值只含字符串键的映射和可序列化的值，序列化不会失败");
    String::from_utf8(out).expect("非 ASCII 字符都已转义")
}

pub fn kind_label(kind: &str) -> &'static str {
    match kind {
        "document" => "文档",
        "image" => "图片",
        "audio" => "音频",
        "video" => "视频",
        _ => "其他",
    }
}

pub fn format_size(bytes: i64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64 / 1024.0;
    for unit in ["KB", "MB", "GB"] {
        if v < 1024.0 {
            return format!("{v:.1} {unit}");
        }
        v /= 1024.0;
    }
    format!("{v:.1} TB")
}

/// 本地日期，形如 2026-01-05
pub(crate) fn format_date(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map_or_else(String::new, |t| t.format("%Y-%m-%d").to_string())
}

/// “第 3 页”“第 3–4 页”
fn span(v: &serde_json::Value, start: &str, end: &str, unit: &str) -> Option<String> {
    let a = v.get(start)?.as_u64().filter(|n| *n > 0)?;
    Some(match v.get(end).and_then(|e| e.as_u64()) {
        Some(b) if b != a => format!("第 {a}–{b} {unit}"),
        _ => format!("第 {a} {unit}"),
    })
}

/// 命中位置（同界面 `src/lib/format.ts` 的 `locatorText`），没有可显示的位置时为空
pub fn locator_text(locator_json: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(locator_json) else {
        return String::new();
    };
    let time = match (
        v.get("start_ms").and_then(|t| t.as_u64()),
        v.get("end_ms").and_then(|t| t.as_u64()),
    ) {
        (Some(a), Some(b)) => Some(format!("{}–{}", clock(a), clock(b))),
        _ => None,
    };
    if let Some(e) = v.get("embed") {
        let what = match e.get("kind").and_then(|k| k.as_str()) {
            Some("image") => "图片",
            Some("audio") => "音频",
            Some("video") => "视频",
            _ => "内容",
        };
        let at = if let Some(p) = e.get("page").and_then(|p| p.as_u64()) {
            format!("第 {p} 页中的{what}")
        } else if let Some(p) = e.get("slide").and_then(|p| p.as_u64()) {
            format!("第 {p} 张幻灯片中的{what}")
        } else if let Some(s) = e.get("sheet").and_then(|s| s.as_str()) {
            format!("工作表“{s}”中的{what}")
        } else {
            format!("文档中的{what}")
        };
        return [Some(at), time]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
    }
    if let Some(t) = time {
        return t;
    }
    if v.get("region").is_some() {
        return "长图中的一段".into();
    }
    let at = span(&v, "page_start", "page_end", "页")
        .or_else(|| span(&v, "slide_start", "slide_end", "张幻灯片"));
    let heading = v
        .get("heading")
        .and_then(|h| h.as_str())
        .filter(|h| !h.is_empty())
        .map(String::from);
    let lines = span(&v, "line_start", "line_end", "行");
    [at, heading, lines]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// 语义检索不可用时的说明（同界面的 `unavailableText`）
fn unavailable_text(reason: Option<SemanticUnavailable>, image: bool) -> &'static str {
    match (reason, image) {
        (Some(SemanticUnavailable::Paused), true) => {
            "正在检测设备或下载模型（或尚未完成首次设置），完成后才能以图搜图。"
        }
        (Some(SemanticUnavailable::Paused), false) => {
            "正在检测设备或下载模型（或尚未完成首次设置），完成前只显示文件名或正文中含有搜索词的结果。"
        }
        (Some(SemanticUnavailable::NoModel), true) => {
            "尚未下载文字模型，无法以图搜图。可在 Rummage 的 设置 → 模型 中下载或导入。"
        }
        (Some(SemanticUnavailable::NoModel), false) => {
            "尚未下载文字模型，只显示文件名或正文中含有搜索词的结果。可在 Rummage 的 设置 → 模型 中下载或导入。"
        }
        (Some(SemanticUnavailable::ImagesOff), true) => {
            "尚未选择按内容查找图片，无法以图搜图。可在 Rummage 的 设置 → 文件夹 中勾选“图片”。"
        }
        (Some(SemanticUnavailable::NoMmproj), true) => {
            "尚未下载图片与音视频模型，无法以图搜图。可在 Rummage 的 设置 → 模型 中下载或导入。"
        }
        (Some(SemanticUnavailable::QueryTooLong), true) => "搜索内容过长，请缩短后再试。",
        (Some(SemanticUnavailable::QueryTooLong), false) => {
            "搜索内容过长，只显示文件名或正文中含有搜索词的结果。请缩短后再试。"
        }
        (_, true) => "内容识别无法使用，无法以图搜图。请重新启动 Rummage。",
        (_, false) => "内容识别无法使用，只显示文件名或正文中含有搜索词的结果。",
    }
}

fn match_text(hit: &SearchHit) -> Vec<&'static str> {
    let mut parts = Vec::new();
    if hit.cosine.is_some() {
        parts.push("内容相近");
    }
    if hit.name_hit {
        parts.push("文件名含搜索词");
    }
    if !hit.text_segments.is_empty() {
        parts.push("正文含搜索词");
    }
    parts
}

pub fn search_text(r: &Results) -> String {
    let mut out = String::new();
    if r.semantic_unavailable {
        let _ = writeln!(out, "{}", unavailable_text(r.unavailable, r.image));
    }
    if r.partial {
        let _ = writeln!(out, "索引尚未完成，结果可能不完整。");
    }
    if r.hits.is_empty() {
        out.push_str("没有找到相关的文件。");
    } else {
        let _ = writeln!(out, "找到 {} 个结果：", r.hits.len());
    }
    for (i, h) in r.hits.iter().enumerate() {
        let mut head = format!("{}. {}  {}", i + 1, h.name, kind_label(&h.kind));
        if let Some(p) = h.match_pct {
            let _ = write!(head, "  匹配度 {p}%");
        }
        let _ = write!(out, "\n{head}\n   {}\n", h.path);
        let mut info = vec![
            format!("修改于 {}", format_date(h.mtime)),
            format_size(h.size),
        ];
        if h.offline {
            info.push("设备未连接".into());
        } else if h.cloud {
            info.push("仅在云端".into());
        }
        info.extend(match_text(h).into_iter().map(String::from));
        if let Some(s) = &h.snippet {
            info.push(format!("命中 ID {}", s.segment_id));
        }
        let _ = write!(out, "   {}", info.join(" · "));
        // 位置单独一行，免得没有匹配度时被看成匹配度
        if let Some(loc) = h.segments.first().map(|s| locator_text(&s.locator_json))
            && !loc.is_empty()
        {
            let _ = write!(out, "\n   位置：{loc}");
        }
        if let Some(s) = &h.snippet {
            let _ = write!(out, "\n   “{}”", s.text);
        }
        out.push('\n');
    }
    if r.weak_omitted > 0 {
        let _ = write!(
            out,
            "\n另有至少 {} 个匹配度较低的结果，加 --include-weak 显示。",
            r.weak_omitted
        );
    }
    out.trim_end().to_string()
}

/// `search --json` 的输出，字段同 `Reply::Results`，另加协议版本与省略的低相关结果数
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchOut<'a> {
    protocol: u32,
    hits: &'a [SearchHit],
    took_ms: u64,
    partial: bool,
    semantic_unavailable: bool,
    unavailable: Option<SemanticUnavailable>,
    /// 没有列出的匹配度较低的结果，至少这么多（只数核心按数量上限返回的那些）
    weak_omitted: usize,
}

pub fn search_json(r: &Results) -> String {
    ascii_json(&SearchOut {
        protocol: PROTOCOL_VERSION,
        hits: r.hits,
        took_ms: r.took_ms,
        partial: r.partial,
        semantic_unavailable: r.semantic_unavailable,
        unavailable: r.unavailable,
        weak_omitted: r.weak_omitted,
    })
}

/// `--folder` 不是已索引的文件夹时的错误，列出全部已索引的文件夹
pub fn not_a_root(folder: &Path, roots: &[RootInfo]) -> String {
    if roots.is_empty() {
        return format!(
            "\"{}\" 不是已索引的文件夹。还没有已索引的文件夹，请先在 Rummage 中添加。",
            folder.display()
        );
    }
    let list: Vec<_> = roots.iter().map(|r| format!("  {}", r.path)).collect();
    format!(
        "\"{}\" 不是已索引的文件夹。已索引的文件夹：\n{}",
        folder.display(),
        list.join("\n")
    )
}

pub(crate) fn pause_text(p: &PauseReason) -> String {
    match p {
        PauseReason::Manual { until: None } => "已手动暂停".into(),
        PauseReason::Manual { until: Some(t) } => {
            let at = Local
                .timestamp_millis_opt(*t)
                .single()
                .map_or_else(String::new, |t| t.format("%Y-%m-%d %H:%M").to_string());
            format!("已手动暂停，到 {at} 恢复")
        }
        PauseReason::Sleeping => "系统即将睡眠或刚唤醒，稍后继续".into(),
        PauseReason::DiskLow { .. } => "磁盘空间不足，已暂停".into(),
        PauseReason::Quota => "已达到系统每天 6 小时的后台运行上限".into(),
        PauseReason::Hot => "手机温度过高，已暂停".into(),
        PauseReason::Battery => "未接电源，已暂停".into(),
        PauseReason::ScreenOn => "屏幕亮着，已暂停".into(),
        PauseReason::Fullscreen => "有全屏程序在运行，已暂停".into(),
    }
}

/// 剩余时间（同界面的 `formatDuration`）
fn duration_text(sec: u64) -> String {
    if sec < 60 {
        return "不到 1 分钟".into();
    }
    let min = (sec as f64 / 60.0).round() as u64;
    if min < 60 {
        return format!("约 {min} 分钟");
    }
    if min < 24 * 60 {
        let m = min % 60;
        return format!(
            "约 {} 小时{}",
            min / 60,
            if m > 0 {
                format!(" {m} 分钟")
            } else {
                String::new()
            }
        );
    }
    let h = min / 60 % 24;
    format!(
        "约 {} 天{}",
        min / 1440,
        if h > 0 {
            format!(" {h} 小时")
        } else {
            String::new()
        }
    )
}

pub fn status_text(s: &Status) -> String {
    let i = s.index;
    let mut out = format!("Rummage {}\n", s.core_version);
    if !s.onboarded {
        out.push_str("尚未完成首次设置，请先打开 Rummage。\n");
    }
    let state = match i.state {
        IndexState::Idle if i.pending == 0 => "索引已完成".to_string(),
        IndexState::Idle | IndexState::Running => "正在建立索引".to_string(),
        IndexState::NoModel => "缺少文字模型，请在 Rummage 中下载或导入".to_string(),
        IndexState::NoMmproj => "缺少图片与音视频模型，请在 Rummage 中下载或导入".to_string(),
        // 后面可能接括号，去掉末尾的句号；兜底与界面的 IndexHint 一致
        IndexState::Error => crate::cli::plain(i.error.as_deref().unwrap_or(
            "内容识别无法启动，目前只能按文件名搜索。请重新启动 Rummage，或在 Rummage 的 设置 → 设备 中选择使用 CPU。",
        ))
        .trim_end_matches('。')
        .to_string(),
    };
    let _ = write!(out, "状态：{state}");
    if let Some(p) = &i.paused {
        let _ = write!(out, "（{}）", pause_text(p));
    }
    let pct = progress_pct(i);
    let _ = write!(
        out,
        "\n进度：{} / {}（{pct}%），失败 {}",
        i.done + i.failed,
        i.total,
        i.failed
    );
    if let Some(eta) = i.eta_s {
        let _ = write!(out, "，剩余{}", duration_text(eta));
    }
    for (kind, c) in &i.by_kind {
        let _ = write!(
            out,
            "\n  {} {} / {}",
            kind_label(kind),
            c.done + c.failed,
            c.total
        );
    }
    if let Some(r) = &i.rebuild {
        let _ = write!(out, "\n按新设置重建：{} / {}", r.done, r.total);
    }
    let _ = write!(
        out,
        "\n内容识别设备：{}",
        i.backend.as_deref().unwrap_or("未启动")
    );
    if s.roots.is_empty() {
        out.push_str("\n还没有已索引的文件夹，请在 Rummage 中添加。");
        return out;
    }
    out.push_str("\n已索引的文件夹：");
    for r in s.roots {
        let note = if r.offline {
            "设备未连接".to_string()
        } else {
            format!("{} 个文件", r.files)
        };
        let _ = write!(out, "\n  {}（{note}）", r.path);
    }
    out
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusOut<'a> {
    protocol: u32,
    core_version: &'a str,
    onboarded: bool,
    index: &'a IndexStatus,
    roots: &'a [RootInfo],
}

pub fn status_json(s: &Status) -> String {
    ascii_json(&StatusOut {
        protocol: PROTOCOL_VERSION,
        core_version: s.core_version,
        onboarded: s.onboarded,
        index: s.index,
        roots: s.roots,
    })
}

pub fn context_text(file: Option<&FileInfo>, seg: &SegmentPreview) -> String {
    let mut out = String::new();
    let loc = locator_text(&seg.locator_json);
    if let Some(f) = file {
        out.push_str(&f.name);
        if !loc.is_empty() {
            let _ = write!(out, "（{loc}）");
        }
        let _ = write!(out, "\n{}\n", f.path);
    } else if !loc.is_empty() {
        let _ = writeln!(out, "{loc}");
    }
    for (label, text) in [
        ("前文", &seg.before),
        ("命中", &seg.text),
        ("后文", &seg.after),
    ] {
        if let Some(t) = text {
            let _ = write!(out, "\n[{label}]\n{}\n", t.trim());
        }
    }
    out.trim_end().to_string()
}

#[derive(Serialize)]
struct ContextOut<'a> {
    protocol: u32,
    file: Option<&'a FileInfo>,
    segment: &'a SegmentPreview,
}

pub fn context_json(file: Option<&FileInfo>, seg: &SegmentPreview) -> String {
    ascii_json(&ContextOut {
        protocol: PROTOCOL_VERSION,
        file,
        segment: seg,
    })
}
