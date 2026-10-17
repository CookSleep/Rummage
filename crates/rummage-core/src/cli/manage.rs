//! 命令行中查看与操作应用的子命令：`folders`、`settings`、`pause`、`resume`、`failed`，
//! 与 MCP 的 `list_folders` 等工具一一对应，判断逻辑见 `crate::manage`。
//! - 文件夹路径的相对路径按本进程的当前目录转成绝对路径；移除、修改时必须是已索引的文件夹（同 `--folder`）。
//! - 添加的校验（必须是存在的本地文件夹、不能与已有的互相包含等）由核心做，与界面添加时一致。
//! - 输出：中文文本，或 `--json`（带协议版本号 `protocol`，非 ASCII 字符转义，同 `output`）。
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use chrono::{Local, TimeZone};
use rummage_proto::{
    FailedFile, Group, IndexStatus, PROTOCOL_VERSION, ParamDef, ParamKind, ParamValue, Reply,
    Request, RootInfo, params,
};
use serde::Serialize;

use super::output::{ascii_json, kind_label, pause_text};
use super::{Args, Cmd, Outcome, Session, emit, parse_kinds, unexpected, utf8};
use crate::manage::{
    CONTENT_KINDS, MAX_PAUSE_MIN, current, enabled_kinds, find_root, kind_changes, parse_value,
    progress_pct, rebuild_kinds, value_text,
};

/// `failed` 最多列出的文件数（最近失败的在前）
const FAILED_SHOWN: u32 = 100;

/// `cmd` 为 `folders`、`settings`、`pause`、`resume` 或 `failed`
pub(super) fn parse(cmd: &str, args: impl IntoIterator<Item = OsString>) -> Result<Args, String> {
    let mut args = args.into_iter();
    let mut json = false;
    let mut data_dir = None;
    let mut words = Vec::new();
    let mut kinds: Option<Vec<String>> = None;
    let mut excludes: Option<Vec<String>> = None;
    let mut minutes = None;
    let mut no_exclude = false;
    while let Some(arg) = args.next() {
        let flag = arg.to_string_lossy().into_owned();
        if !flag.starts_with("--") {
            words.push(utf8(arg, "参数")?);
            continue;
        }
        if flag == "--json" {
            json = true;
            continue;
        }
        // 清空排除规则；Windows PowerShell 5.1 不把空字符串参数传给外部程序，`--exclude ""` 在那里用不了
        if flag == "--no-exclude" && cmd == "folders" {
            no_exclude = true;
            excludes.get_or_insert_default();
            continue;
        }
        let known = flag == "--data-dir"
            || (cmd == "folders" && matches!(flag.as_str(), "--types" | "--exclude"))
            || (cmd == "pause" && flag == "--minutes");
        if !known {
            return Err(format!("无法识别的参数：{flag}。"));
        }
        let value = args.next().ok_or_else(|| format!("{flag} 后面缺少值。"))?;
        match flag.as_str() {
            "--data-dir" => data_dir = Some(PathBuf::from(value)),
            "--types" => {
                let all: Vec<_> = CONTENT_KINDS.iter().map(|c| c.0).collect();
                let list = kinds.get_or_insert_default();
                for k in parse_kinds(utf8(value, &flag)?.split([',', '，']), &all)
                    .map_err(|k| format!("无法识别的类型“{k}”，可选：{}。", all.join("、")))?
                {
                    if !list.contains(&k) {
                        list.push(k);
                    }
                }
            }
            "--exclude" => excludes.get_or_insert_default().push(utf8(value, &flag)?),
            _ => {
                minutes = Some(
                    utf8(value, &flag)?
                        .trim()
                        .parse()
                        .ok()
                        .filter(|n| (1..=MAX_PAUSE_MIN).contains(n))
                        .ok_or(format!("--minutes 应为 1–{MAX_PAUSE_MIN} 的整数。"))?,
                );
            }
        }
    }
    if kinds.as_ref().is_some_and(|k| k.is_empty()) {
        return Err("--types 至少给出一种类型。".into());
    }
    if no_exclude && excludes.as_ref().is_some_and(|e| !e.is_empty()) {
        return Err("--no-exclude 与 --exclude 不能同时使用。".into());
    }
    let mut words = words.into_iter();
    let sub = words.next();
    let mut path = |what: &str| {
        words
            .next()
            .map(PathBuf::from)
            .ok_or(format!("folders {what} 需要一个文件夹路径。"))
    };
    let cmd = match (cmd, sub.as_deref()) {
        ("folders", None) => Cmd::Folders,
        ("folders", Some("add")) => Cmd::AddFolder {
            path: path("add")?,
            kinds: kinds.take(),
            excludes: excludes.take(),
        },
        ("folders", Some("set")) => {
            let path = path("set")?;
            if kinds.is_none() && excludes.is_none() {
                return Err("folders set 需要 --types 或 --exclude。".into());
            }
            Cmd::SetFolder {
                path,
                kinds: kinds.take(),
                excludes: excludes.take(),
            }
        }
        ("folders", Some("remove")) => Cmd::RemoveFolder(path("remove")?),
        ("settings", None) => Cmd::Settings(None),
        ("settings", Some("set")) => {
            let (Some(key), Some(value)) = (words.next(), words.next()) else {
                return Err(
                    "settings set 需要参数的键和值，例如 settings set chunk_tokens 256。".into(),
                );
            };
            Cmd::SetSetting { key, value }
        }
        ("settings", Some(key)) => Cmd::Settings(Some(key.to_string())),
        ("pause", None) => Cmd::Pause(minutes),
        ("resume", None) => Cmd::Resume,
        ("failed", None) => Cmd::Failed,
        ("failed", Some("retry")) => Cmd::RetryFailed,
        (_, Some(word)) => return Err(format!("无法识别的参数：{word}。")),
        (cmd, None) => unreachable!("main 只把 COMMANDS 中的子命令交给 parse：{cmd}"),
    };
    if let Some(word) = words.next() {
        return Err(format!("无法识别的参数：{word}。"));
    }
    if kinds.is_some() || excludes.is_some() {
        return Err(
            "--types、--exclude、--no-exclude 只能用于 folders add 与 folders set。".into(),
        );
    }
    Ok(Args {
        cmd,
        json,
        data_dir,
    })
}

fn absolute(p: &Path) -> Result<String, String> {
    std::path::absolute(p)
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|_| format!("无法识别路径 \"{}\"。", p.display()))
}

/// “文档、图片”；一种都没有时为“无（只按文件名搜索）”
fn kinds_text(kinds: &[&str]) -> String {
    if kinds.is_empty() {
        return "无（只按文件名搜索）".into();
    }
    kinds
        .iter()
        .map(|k| kind_label(k))
        .collect::<Vec<_>>()
        .join("、")
}

fn excludes_text(rules: &[String]) -> String {
    if rules.is_empty() {
        return "无".into();
    }
    rules.join("、")
}

pub fn folders_text(kinds: &[&str], roots: &[RootInfo], index: Option<&IndexStatus>) -> String {
    let mut out = format!(
        "识别内容的文件类型：{}（所有文件夹共用）",
        kinds_text(kinds)
    );
    if let Some(i) = index {
        let pct = progress_pct(i);
        let _ = write!(
            out,
            "\n索引进度：{} / {}（{pct}%），失败 {}",
            i.done + i.failed,
            i.total,
            i.failed
        );
        if let Some(p) = &i.paused {
            let _ = write!(out, "，{}", pause_text(p));
        }
    }
    if roots.is_empty() {
        out.push_str("\n还没有已索引的文件夹，可用 folders add <文件夹> 添加。");
        return out;
    }
    out.push_str("\n已索引的文件夹：");
    for r in roots {
        let note = if r.offline {
            "设备未连接".to_string()
        } else if r.scanning {
            "正在统计文件".to_string()
        } else {
            format!("{} 个文件", r.files)
        };
        let _ = write!(out, "\n  {}（{note}）", r.path);
        if !r.excludes.is_empty() {
            let _ = write!(out, "\n    排除：{}", excludes_text(&r.excludes));
        }
    }
    out
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FoldersOut<'a> {
    protocol: u32,
    kinds: &'a [&'a str],
    roots: &'a [RootInfo],
    index: Option<&'a IndexStatus>,
}

/// 操作的结果：一句说明，添加、修改文件夹时附上该文件夹
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DoneOut<'a> {
    protocol: u32,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    root: Option<&'a RootInfo>,
}

fn done_json(message: &str, root: Option<&RootInfo>) -> String {
    ascii_json(&DoneOut {
        protocol: PROTOCOL_VERSION,
        message,
        root,
    })
}

fn group_text(g: Group) -> &'static str {
    match g {
        Group::Search => "搜索（修改后立即生效）",
        Group::Index => "索引",
        Group::Resource => "资源占用（修改后立即生效）",
        Group::Advanced => "高级",
    }
}

/// 允许的取值
pub fn range_text(kind: ParamKind) -> String {
    match kind {
        ParamKind::Bool => "true 或 false".into(),
        ParamKind::Int { min, max } => format!("{min}–{max} 的整数"),
        ParamKind::Float { min, max } => format!("{min}–{max} 的数"),
        ParamKind::Named { options, .. } => {
            let list: Vec<_> = options
                .iter()
                .map(|o| value_text(kind, ParamValue::Int(o.0), false))
                .collect();
            format!("可选 {}", list.join("、"))
        }
    }
}

fn index_kinds_text(kinds: &[&str]) -> String {
    kinds
        .iter()
        .map(|k| kind_label(k))
        .collect::<Vec<_>>()
        .join("、")
}

fn setting_text(out: &mut String, d: &ParamDef, value: ParamValue) {
    let _ = write!(
        out,
        "\n  {} = {}（默认 {}，{}",
        d.key,
        value_text(d.kind, value, false),
        value_text(d.kind, (d.default)(), false),
        range_text(d.kind)
    );
    if d.affects_index {
        let _ = write!(out, "，修改后重建{}的索引", index_kinds_text(d.index_kinds));
    }
    let _ = write!(out, "）\n    {}：{}", d.label, d.help);
}

/// 全部参数，按分组
pub fn settings_text(values: &BTreeMap<String, ParamValue>) -> String {
    let mut out = String::new();
    for g in [
        Group::Search,
        Group::Index,
        Group::Resource,
        Group::Advanced,
    ] {
        let _ = write!(out, "\n{}", group_text(g));
        for d in params::PARAMS.iter().filter(|d| d.group == g && d.shown()) {
            setting_text(&mut out, d, current(values, d));
        }
        out.push('\n');
    }
    out.trim().to_string()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingOut<'a> {
    #[serde(flatten)]
    def: &'a ParamDef,
    value: ParamValue,
}

#[derive(Serialize)]
struct SettingsOut<'a> {
    protocol: u32,
    settings: Vec<SettingOut<'a>>,
}

/// 修改参数后的说明；`changed` 为假时值没有变化，`rebuild` 为要重建的种类
pub fn set_text(d: &ParamDef, value: ParamValue, changed: bool, rebuild: &[&str]) -> String {
    let shown = value_text(d.kind, value, false);
    if !changed {
        return format!("{}（{}）已是 {shown}，没有变化。", d.key, d.label);
    }
    let mut out = format!("已将 {}（{}）设为 {shown}。", d.key, d.label);
    if !rebuild.is_empty() {
        let _ = write!(
            out,
            "将按新设置重建{}的索引，重建期间旧索引仍可搜索。",
            index_kinds_text(rebuild)
        );
    } else if d.affects_index {
        out.push_str("视频识别方式不是“自定义”，此项暂不生效。");
    }
    if d.key == "selftest_min_cosine" {
        out.push_str("新的通过标准在 Rummage 的 设置 → 设备 中重新检测后生效。");
    }
    out
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SetOut<'a> {
    protocol: u32,
    key: &'a str,
    value: ParamValue,
    changed: bool,
    rebuild: &'a [&'a str],
    message: &'a str,
}

pub fn failed_text(files: &[FailedFile], total: u64, retryable: u64) -> String {
    if total == 0 {
        return "没有建立索引失败的文件。".into();
    }
    let mut out = format!("共 {total} 个文件建立索引失败，其中 {retryable} 个可以重试：");
    for f in files {
        let _ = write!(out, "\n  {}", f.path);
        if !f.retryable {
            out.push_str("（加密文档，不能重试）");
        }
        let _ = write!(
            out,
            "\n    {}",
            crate::cli::plain(f.reason.as_deref().unwrap_or("原因未知。"))
        );
    }
    if total > files.len() as u64 {
        let _ = write!(out, "\n只列出了最近的 {} 个。", files.len());
    }
    out
}

#[derive(Serialize)]
struct FailedOut<'a> {
    protocol: u32,
    total: u64,
    retryable: u64,
    files: &'a [FailedFile],
}

async fn params_of(s: &mut Session) -> Result<BTreeMap<String, ParamValue>, String> {
    let Reply::Params { values } = s.request(Request::GetParams).await? else {
        return Err(unexpected());
    };
    Ok(values)
}

async fn roots_of(s: &mut Session) -> Result<Vec<RootInfo>, String> {
    let Reply::Roots { roots } = s.request(Request::ListRoots).await? else {
        return Err(unexpected());
    };
    Ok(roots)
}

/// 已索引的文件夹；不是时的错误列出全部已索引的文件夹
async fn root_of(s: &mut Session, path: &Path) -> Result<RootInfo, String> {
    let roots = roots_of(s).await?;
    let path = absolute(path)?;
    find_root(&roots, &path)
        .cloned()
        .ok_or_else(|| super::output::not_a_root(Path::new(&path), &roots))
}

/// 把识别内容的类型改为 `kinds`，返回改后开启的类型
async fn set_kinds(s: &mut Session, kinds: Option<&[String]>) -> Result<Vec<&'static str>, String> {
    let values = params_of(s).await?;
    let Some(kinds) = kinds else {
        return Ok(enabled_kinds(&values));
    };
    for (key, value) in kind_changes(&values, kinds) {
        let body = Request::SetParam {
            key: key.into(),
            value,
        };
        s.request(body).await?;
    }
    Ok(CONTENT_KINDS
        .iter()
        .map(|c| c.0)
        .filter(|k| kinds.iter().any(|w| w == k))
        .collect())
}

fn print(text: &str) -> Result<(), String> {
    emit(text).map_err(|err| format!("无法输出结果：{err}"))
}

pub(super) async fn run(s: &mut Session, cmd: Cmd, json: bool) -> Result<Outcome, String> {
    match cmd {
        Cmd::Folders => {
            let values = params_of(s).await?;
            let roots = roots_of(s).await?;
            // 索引尚未启动时只是没有进度
            let index = match s.request(Request::IndexStatus).await {
                Ok(Reply::Index(i)) => Some(i),
                _ => None,
            };
            let kinds = enabled_kinds(&values);
            print(&if json {
                ascii_json(&FoldersOut {
                    protocol: PROTOCOL_VERSION,
                    kinds: &kinds,
                    roots: &roots,
                    index: index.as_ref(),
                })
            } else {
                folders_text(&kinds, &roots, index.as_ref())
            })?;
            Ok(if roots.is_empty() {
                Outcome::Empty
            } else {
                Outcome::Found
            })
        }
        Cmd::AddFolder {
            path,
            kinds,
            excludes,
        } => {
            let body = Request::AddRoot {
                path: absolute(&path)?,
                excludes: excludes.unwrap_or_default(),
            };
            // 路径是后台服务规范化后的，搜索的 --folder 要用它
            let Reply::Root { root } = s.request(body).await? else {
                return Err(unexpected());
            };
            let kinds = set_kinds(s, kinds.as_deref())
                .await
                .map_err(|err| format!("文件夹已添加，但文件类型未能保存：{err}"))?;
            let msg = format!(
                "已添加 {}，正在后台扫描并建立索引。\n排除规则：{}\n识别内容的文件类型：{}（所有文件夹共用）",
                root.path,
                excludes_text(&root.excludes),
                kinds_text(&kinds)
            );
            print(&if json {
                done_json(&msg, Some(&root))
            } else {
                msg
            })?;
            Ok(Outcome::Found)
        }
        Cmd::SetFolder {
            path,
            kinds,
            excludes,
        } => {
            let mut root = root_of(s, &path).await?;
            if let Some(excludes) = excludes {
                let body = Request::SetRootExcludes {
                    id: root.id,
                    excludes,
                };
                s.request(body).await?;
                root = root_of(s, Path::new(&root.path)).await?;
            }
            let kinds = set_kinds(s, kinds.as_deref()).await?;
            let msg = format!(
                "已更新 {}，将按新设置重新扫描。\n排除规则：{}\n识别内容的文件类型：{}（所有文件夹共用）",
                root.path,
                excludes_text(&root.excludes),
                kinds_text(&kinds)
            );
            print(&if json {
                done_json(&msg, Some(&root))
            } else {
                msg
            })?;
            Ok(Outcome::Found)
        }
        Cmd::RemoveFolder(path) => {
            let root = root_of(s, &path).await?;
            s.request(Request::RemoveRoot { id: root.id }).await?;
            let msg = format!("已移除 {} 的索引，文件本身没有删除。", root.path);
            print(&if json { done_json(&msg, None) } else { msg })?;
            Ok(Outcome::Found)
        }
        Cmd::Settings(key) => {
            let values = params_of(s).await?;
            let defs: Vec<&ParamDef> = match &key {
                Some(k) => vec![
                    params::def(k)
                        .ok_or_else(|| format!("没有名为“{k}”的参数，可用 settings 查看全部。"))?,
                ],
                None => params::PARAMS.iter().filter(|d| d.shown()).collect(),
            };
            let text = if json {
                ascii_json(&SettingsOut {
                    protocol: PROTOCOL_VERSION,
                    settings: defs
                        .iter()
                        .map(|d| SettingOut {
                            def: d,
                            value: current(&values, d),
                        })
                        .collect(),
                })
            } else if key.is_some() {
                let mut out = String::new();
                setting_text(&mut out, defs[0], current(&values, defs[0]));
                out.trim_start().to_string()
            } else {
                settings_text(&values)
            };
            print(&text)?;
            Ok(Outcome::Found)
        }
        Cmd::SetSetting { key, value } => {
            let d = params::def(&key)
                .ok_or_else(|| format!("没有名为“{key}”的参数，可用 settings 查看全部。"))?;
            let parsed = parse_value(d.kind, &value)
                .ok_or_else(|| format!("“{}”的值应为{}。", d.label, range_text(d.kind)))?;
            let parsed = params::validate(&key, &parsed).map_err(|err| err.to_string())?;
            let before = params_of(s).await?;
            let changed = before.get(d.key) != Some(&parsed);
            let rebuild = rebuild_kinds(d.key, &before, parsed);
            let mut msg = set_text(d, parsed, changed, rebuild);
            if changed {
                let body = Request::SetParam {
                    key: key.clone(),
                    value: parsed,
                };
                if let Reply::DoneWithWarning { message } = s.request(body).await? {
                    let _ = write!(msg, "\n{}", crate::cli::plain(&message));
                }
            }
            print(&if json {
                ascii_json(&SetOut {
                    protocol: PROTOCOL_VERSION,
                    key: d.key,
                    value: parsed,
                    changed,
                    rebuild,
                    message: &msg,
                })
            } else {
                msg
            })?;
            Ok(Outcome::Found)
        }
        Cmd::Pause(minutes) => {
            let until = minutes.map(|m| Local::now().timestamp_millis() + i64::from(m) * 60_000);
            s.request(Request::PauseIndexing { until }).await?;
            let msg = match until.and_then(|t| Local.timestamp_millis_opt(t).single()) {
                Some(t) => format!(
                    "已暂停建立索引，到 {} 自动恢复。暂停期间照常可以搜索。",
                    t.format("%Y-%m-%d %H:%M")
                ),
                None => {
                    "已暂停建立索引，直到执行 resume 或在 Rummage 中恢复。暂停期间照常可以搜索。"
                        .into()
                }
            };
            print(&if json { done_json(&msg, None) } else { msg })?;
            Ok(Outcome::Found)
        }
        Cmd::Resume => {
            s.request(Request::ResumeIndexing).await?;
            let msg = "已恢复建立索引。未接电源、全屏或磁盘空间不足时仍会按设置自动暂停。";
            print(&if json {
                done_json(msg, None)
            } else {
                msg.into()
            })?;
            Ok(Outcome::Found)
        }
        Cmd::Failed => {
            let body = Request::FailedFiles {
                after: None,
                limit: FAILED_SHOWN,
            };
            let Reply::FailedFiles {
                files,
                total,
                retryable,
            } = s.request(body).await?
            else {
                return Err(unexpected());
            };
            print(&if json {
                ascii_json(&FailedOut {
                    protocol: PROTOCOL_VERSION,
                    total,
                    retryable,
                    files: &files,
                })
            } else {
                failed_text(&files, total, retryable)
            })?;
            Ok(if total == 0 {
                Outcome::Empty
            } else {
                Outcome::Found
            })
        }
        Cmd::RetryFailed => {
            let body = Request::FailedFiles {
                after: None,
                limit: 1,
            };
            let Reply::FailedFiles {
                total, retryable, ..
            } = s.request(body).await?
            else {
                return Err(unexpected());
            };
            if retryable > 0 {
                s.request(Request::RetryAllFailed).await?;
            }
            let mut msg = if retryable == 0 {
                "没有可以重试的失败文件。".to_string()
            } else {
                format!("已将 {retryable} 个失败的文件重新排队建立索引。")
            };
            if total > retryable {
                let _ = write!(msg, "另有 {} 个加密文档不能重试。", total - retryable);
            }
            print(&if json { done_json(&msg, None) } else { msg })?;
            Ok(if retryable == 0 {
                Outcome::Empty
            } else {
                Outcome::Found
            })
        }
        Cmd::Search(_) | Cmd::Status | Cmd::Context(_) => {
            unreachable!("search、status、context 由 cli::run 处理")
        }
    }
}
