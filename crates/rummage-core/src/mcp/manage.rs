//! MCP 中查看与操作应用的工具：`list_folders`、`add_folder`、`remove_folder`、
//! `update_folder`、`get_settings`、`set_setting`、`pause_indexing`、`resume_indexing`、`list_failed_files`、
//! `retry_failed_files`，与命令行的 `folders` 等子命令一一对应，判断逻辑见 `crate::manage`。
//! - 参数写法同 `params`：可选参数用 `#[serde(default)]`，宽松解析，schema 中不出现 `null`（`Option` 字段用自定义 schema，
//!   并以 `skip_serializing_if` 让 schemars 不输出 `null` 默认值）。
//! - 文件夹路径必须是绝对路径；移除、修改时必须是已索引的文件夹（同 `search_files` 的 `folder`）。
//! - 结果是简洁的英文文本；参数的名称、说明（参数表中的中文）原样输出。
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use chrono::{Local, TimeZone};
use rmcp::schemars::{self, JsonSchema, Schema, SchemaGenerator, json_schema};
use rummage_proto::{
    FailedFile, Group, IndexStatus, ParamDef, ParamKind, ParamValue, Reply, Request, RootInfo,
    params,
};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use super::params::int;
use super::{Server, bad, kinds_param, rummage, unexpected};
use crate::manage::{
    CONTENT_KINDS, MAX_PAUSE_MIN, current, enabled_kinds, find_root, kind_changes, parse_value,
    progress_pct, rebuild_kinds, value_text,
};

/// `list_failed_files` 默认列出的文件数
pub const DEFAULT_FAILED: u32 = 20;
/// `list_failed_files` 最多列出的文件数
pub const MAX_FAILED: u32 = 100;

fn default_failed() -> u32 {
    DEFAULT_FAILED
}

/// 文件夹路径；缺少时由 serde 报 “missing field”，`null` 与空字符串留给调用处报错
fn path<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    match Value::deserialize(d)? {
        Value::Null => Ok(String::new()),
        Value::String(s) => Ok(s.trim().to_string()),
        v => Err(bad(format!(
            r#"path must be a string but got {v}. Example: {{"path": "D:\\Photos"}}"#
        ))),
    }
}

/// 识别内容的类型：单个字符串或字符串数组；`null`、空字符串、空数组视为未给出
fn content_kinds<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<String>>, D::Error> {
    let all: Vec<_> = CONTENT_KINDS.iter().map(|c| c.0).collect();
    let msg = format!(
        r#"kinds must be one or more of {}. Example: {{"kinds": ["document", "image"]}}"#,
        all.join(", ")
    );
    let out = kinds_param(d, &all, msg)?;
    Ok((!out.is_empty()).then_some(out))
}

/// 排除规则：单个字符串或字符串数组；`null` 与空字符串视为未给出（同参数的约定），空数组表示没有规则（修改时即清空）
fn rules<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<String>>, D::Error> {
    let msg = || {
        r#"excludes must be an array of strings. Example: {"excludes": ["*.tmp", "node_modules"]}"#
            .to_string()
    };
    let items = match Value::deserialize(d)? {
        Value::Null => return Ok(None),
        Value::String(s) if s.trim().is_empty() => return Ok(None),
        Value::Array(a) => a,
        v => vec![v],
    };
    items
        .into_iter()
        .map(|v| match v {
            Value::String(s) => Ok(s),
            _ => Err(bad(msg())),
        })
        .collect::<Result<_, _>>()
        .map(Some)
}

fn minutes<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    match int(Value::deserialize(d)?) {
        Ok(None) => Ok(0),
        Ok(Some(n)) if (0..=i64::from(MAX_PAUSE_MIN)).contains(&n) => Ok(n as u32),
        _ => Err(bad(format!(
            r#"minutes must be an integer from 0 to {MAX_PAUSE_MIN}; 0 or omitted pauses until resume_indexing. Example: {{"minutes": 60}}"#
        ))),
    }
}

fn failed_limit<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    match int(Value::deserialize(d)?) {
        Ok(None) => Ok(DEFAULT_FAILED),
        Ok(Some(n)) if (1..=i64::from(MAX_FAILED)).contains(&n) => Ok(n as u32),
        _ => Err(bad(format!(
            r#"limit must be an integer from 1 to {MAX_FAILED}. Example: {{"limit": {DEFAULT_FAILED}}}"#
        ))),
    }
}

fn key<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    match Value::deserialize(d)? {
        Value::Null => Ok(String::new()),
        Value::String(s) => Ok(s.trim().to_string()),
        v => Err(bad(format!(
            r#"key must be a setting key string but got {v}. Example: {{"key": "chunk_tokens"}}"#
        ))),
    }
}

fn kinds_schema(_: &mut SchemaGenerator) -> Schema {
    let all: Vec<_> = CONTENT_KINDS.iter().map(|c| c.0).collect();
    json_schema!({
        "type": "array",
        "items": {"type": "string", "enum": all},
        "description": "File types whose content Rummage recognizes. This is one setting shared by all indexed folders (other types stay searchable by file name). Omit to keep the current setting.",
    })
}

fn add_excludes_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "array",
        "items": {"type": "string"},
        "description": "Exclude rules for this folder, written like .gitignore: \"node_modules\" matches at any depth, \"/drafts\" only at the top level, \"*.tmp\" by pattern. Omit (or pass an empty string) for none.",
    })
}

fn update_excludes_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "array",
        "items": {"type": "string"},
        "description": "New exclude rules, written like .gitignore. Replaces all existing rules of this folder: include the ones to keep; [] removes all. Omit (or pass an empty string) to keep the current rules.",
    })
}

fn value_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": ["boolean", "number", "string"],
        "description": "New value: true/false for switches, a number within the allowed range, or for named options the number or its name as listed by get_settings.",
    })
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddFolderParams {
    /// Absolute path of an existing local folder to index.
    #[serde(deserialize_with = "path")]
    pub path: String,
    #[serde(
        default,
        deserialize_with = "content_kinds",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(schema_with = "kinds_schema")]
    pub kinds: Option<Vec<String>>,
    #[serde(
        default,
        deserialize_with = "rules",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(schema_with = "add_excludes_schema")]
    pub excludes: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateFolderParams {
    /// An indexed folder, exactly as listed by list_folders.
    #[serde(deserialize_with = "path")]
    pub path: String,
    #[serde(
        default,
        deserialize_with = "content_kinds",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(schema_with = "kinds_schema")]
    pub kinds: Option<Vec<String>>,
    #[serde(
        default,
        deserialize_with = "rules",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(schema_with = "update_excludes_schema")]
    pub excludes: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RemoveFolderParams {
    /// An indexed folder, exactly as listed by list_folders.
    #[serde(deserialize_with = "path")]
    pub path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetSettingsParams {
    /// Show only this setting; omit to list all.
    #[serde(default, deserialize_with = "key")]
    pub key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetSettingParams {
    /// Setting key as listed by get_settings, e.g. "chunk_tokens".
    #[serde(deserialize_with = "key")]
    pub key: String,
    #[schemars(schema_with = "value_schema")]
    pub value: Value,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PauseParams {
    // 说明中的 10080 同 MAX_PAUSE_MIN，要同步修改
    /// Resume automatically after this many minutes, 1-10080. Omit or 0 to pause until resume_indexing is called.
    #[serde(default, deserialize_with = "minutes")]
    #[schemars(range(min = 0, max = MAX_PAUSE_MIN))]
    pub minutes: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FailedParams {
    // 说明中的 1-100、20 同 MAX_FAILED、DEFAULT_FAILED，要同步修改
    /// Number of most recent failures to list, 1-100. Default 20.
    #[serde(default = "default_failed", deserialize_with = "failed_limit")]
    #[schemars(range(min = 1, max = MAX_FAILED))]
    pub limit: u32,
}

fn kind_list(kinds: &[&str]) -> String {
    if kinds.is_empty() {
        return "none (all files are searchable by file name only)".into();
    }
    kinds.join(", ")
}

fn rules_list(rules: &[String]) -> String {
    if rules.is_empty() {
        return "none".into();
    }
    rules.join(", ")
}

pub fn folders_text(kinds: &[&str], roots: &[RootInfo], index: Option<&IndexStatus>) -> String {
    let mut out = format!(
        "Content recognition: {} (shared by all folders)",
        kind_list(kinds)
    );
    if let Some(i) = index {
        let pct = progress_pct(i);
        let _ = write!(
            out,
            "\nIndexing: {} / {} files ({pct}%), {} failed",
            i.done + i.failed,
            i.total,
            i.failed
        );
        if let Some(p) = &i.paused {
            let _ = write!(out, " ({})", super::format::pause_text(p));
        }
    }
    if roots.is_empty() {
        out.push_str("\nNo folders are indexed yet. Add one with add_folder.");
        return out;
    }
    out.push_str("\nIndexed folders:");
    for r in roots {
        if r.offline {
            let _ = write!(out, "\n  {} (device not connected)", r.path);
        } else if r.scanning {
            let _ = write!(out, "\n  {} (still counting files)", r.path);
        } else {
            let _ = write!(out, "\n  {} ({} files)", r.path, r.files);
        }
        if !r.excludes.is_empty() {
            let _ = write!(out, "\n    excludes: {}", rules_list(&r.excludes));
        }
    }
    out
}

/// 允许的取值
pub fn range_text(kind: ParamKind) -> String {
    match kind {
        ParamKind::Bool => "true or false".into(),
        ParamKind::Int { min, max } => format!("integer {min} to {max}"),
        ParamKind::Float { min, max } => format!("number {min} to {max}"),
        ParamKind::Named { options, .. } => {
            let list: Vec<_> = options
                .iter()
                .map(|o| value_text(kind, ParamValue::Int(o.0), true))
                .collect();
            format!("one of {}", list.join(", "))
        }
    }
}

fn group_text(g: Group) -> &'static str {
    match g {
        Group::Search => "Search (applies immediately)",
        Group::Index => "Indexing",
        Group::Resource => "Resource use (applies immediately)",
        Group::Advanced => "Advanced",
    }
}

fn setting_text(out: &mut String, d: &ParamDef, value: ParamValue) {
    let _ = write!(
        out,
        "\n  {} = {} | default {} | {}",
        d.key,
        value_text(d.kind, value, true),
        value_text(d.kind, (d.default)(), true),
        range_text(d.kind)
    );
    if d.affects_index {
        let _ = write!(
            out,
            " | changing it rebuilds the {} index",
            d.index_kinds.join(", ")
        );
    }
    let _ = write!(out, "\n    {}: {}", d.label, d.help);
}

/// 参数列表；`only` 不为空时只列这一项
pub fn settings_text(values: &BTreeMap<String, ParamValue>, only: Option<&ParamDef>) -> String {
    let mut out = String::new();
    if let Some(d) = only {
        setting_text(&mut out, d, current(values, d));
        return out.trim_start().to_string();
    }
    for g in [
        Group::Search,
        Group::Index,
        Group::Resource,
        Group::Advanced,
    ] {
        let _ = write!(out, "\n{}:", group_text(g));
        for d in params::PARAMS.iter().filter(|d| d.group == g && d.shown()) {
            setting_text(&mut out, d, current(values, d));
        }
    }
    out.push_str("\nChange one with set_setting.");
    out.trim_start().to_string()
}

/// 修改参数后的说明；`changed` 为假时值没有变化，`rebuild` 为要重建的种类
pub fn set_text(d: &ParamDef, value: ParamValue, changed: bool, rebuild: &[&str]) -> String {
    let shown = value_text(d.kind, value, true);
    if !changed {
        return format!("{} is already {shown}; nothing changed.", d.key);
    }
    let mut out = format!("Set {} to {shown}.", d.key);
    if !rebuild.is_empty() {
        let _ = write!(
            out,
            " Rummage will rebuild the {} index with the new setting in the background; the old index stays searchable meanwhile.",
            rebuild.join(", ")
        );
    } else if d.affects_index {
        out.push_str(" It has no effect unless video_preset is 0 (自定义).");
    }
    if d.key == "selftest_min_cosine" {
        out.push_str(" The new threshold takes effect after the user runs the device check again (Rummage settings → Device).");
    }
    out
}

/// MCP 给的值：布尔、数字，或文字（同命令行的写法）
pub fn json_value(kind: ParamKind, v: &Value) -> Option<ParamValue> {
    match v {
        Value::Bool(b) => Some(ParamValue::Bool(*b)),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Some(ParamValue::Int(i)),
            None => n.as_f64().map(ParamValue::Float),
        },
        Value::String(s) => parse_value(kind, s),
        _ => None,
    }
}

pub fn failed_text(files: &[FailedFile], total: u64, retryable: u64) -> String {
    if total == 0 {
        return "No files failed to index.".into();
    }
    let mut out = format!("{total} files failed to index ({retryable} can be retried):");
    for f in files {
        let _ = write!(out, "\n  {}", f.path);
        if !f.retryable {
            out.push_str(" (encrypted document, cannot be retried)");
        }
        let _ = write!(
            out,
            "\n    reason: {}",
            crate::cli::plain(f.reason.as_deref().unwrap_or("unknown"))
        );
    }
    if total > files.len() as u64 {
        let _ = write!(out, "\nShowing the {} most recent.", files.len());
    }
    out
}

fn absolute(path: &str) -> Result<(), String> {
    if path.is_empty() || !Path::new(path).is_absolute() {
        return Err(
            r#"path must be an absolute folder path. Example: {"path": "D:\\Photos"}"#.into(),
        );
    }
    Ok(())
}

impl Server {
    async fn values(&self) -> Result<BTreeMap<String, ParamValue>, String> {
        let Reply::Params { values } = self.call(Request::GetParams).await? else {
            return Err(unexpected());
        };
        Ok(values)
    }

    async fn roots(&self) -> Result<Vec<RootInfo>, String> {
        let Reply::Roots { roots } = self.call(Request::ListRoots).await? else {
            return Err(unexpected());
        };
        Ok(roots)
    }

    /// 已索引的文件夹；不是时的错误列出全部已索引的文件夹
    async fn root(&self, path: &str) -> Result<RootInfo, String> {
        absolute(path)?;
        let roots = self.roots().await?;
        find_root(&roots, path)
            .cloned()
            .ok_or_else(|| super::format::not_a_root(path, &roots))
    }

    /// 把识别内容的类型改为 `kinds`，返回改后开启的类型
    async fn set_kinds(&self, kinds: Option<&[String]>) -> Result<Vec<&'static str>, String> {
        let values = self.values().await?;
        let Some(kinds) = kinds else {
            return Ok(enabled_kinds(&values));
        };
        for (key, value) in kind_changes(&values, kinds) {
            let body = Request::SetParam {
                key: key.into(),
                value,
            };
            self.call(body).await?;
        }
        Ok(CONTENT_KINDS
            .iter()
            .map(|c| c.0)
            .filter(|k| kinds.iter().any(|w| w == k))
            .collect())
    }

    pub(super) async fn list_folders_text(&self) -> Result<String, String> {
        let values = self.values().await?;
        let roots = self.roots().await?;
        // 索引尚未启动时只是没有进度
        let index = match self.call(Request::IndexStatus).await {
            Ok(Reply::Index(i)) => Some(i),
            _ => None,
        };
        Ok(folders_text(
            &enabled_kinds(&values),
            &roots,
            index.as_ref(),
        ))
    }

    pub(super) async fn add_folder_text(&self, p: AddFolderParams) -> Result<String, String> {
        absolute(&p.path)?;
        let body = Request::AddRoot {
            path: p.path,
            excludes: p.excludes.unwrap_or_default(),
        };
        // 路径是后台服务规范化后的，search_files 的 folder 要用它
        let Reply::Root { root } = self.call(body).await? else {
            return Err(unexpected());
        };
        let kinds = self.set_kinds(p.kinds.as_deref()).await.map_err(|err| {
            format!("The folder was added, but the file types were not saved. {err}")
        })?;
        Ok(format!(
            "Added {}; Rummage is scanning and indexing it in the background.\nexcludes: {}\nContent recognition: {} (shared by all folders)",
            root.path,
            rules_list(&root.excludes),
            kind_list(&kinds)
        ))
    }

    pub(super) async fn update_folder_text(&self, p: UpdateFolderParams) -> Result<String, String> {
        if p.kinds.is_none() && p.excludes.is_none() {
            return Err(
                r#"Provide kinds, excludes, or both. Example: {"path": "D:\\Photos", "excludes": ["*.tmp"]}"#
                    .into(),
            );
        }
        let mut root = self.root(&p.path).await?;
        if let Some(excludes) = p.excludes {
            let body = Request::SetRootExcludes {
                id: root.id,
                excludes,
            };
            self.call(body).await?;
            root = self.root(&root.path).await?;
        }
        let kinds = self.set_kinds(p.kinds.as_deref()).await?;
        Ok(format!(
            "Updated {}; it will be rescanned with the new settings.\nexcludes: {}\nContent recognition: {} (shared by all folders)",
            root.path,
            rules_list(&root.excludes),
            kind_list(&kinds)
        ))
    }

    pub(super) async fn remove_folder_text(&self, p: RemoveFolderParams) -> Result<String, String> {
        let root = self.root(&p.path).await?;
        self.call(Request::RemoveRoot { id: root.id }).await?;
        Ok(format!(
            "Removed {} from Rummage's index. No files were deleted.",
            root.path
        ))
    }

    pub(super) async fn get_settings_text(&self, p: GetSettingsParams) -> Result<String, String> {
        let only = match p.key.as_str() {
            "" => None,
            k => Some(params::def(k).ok_or_else(|| {
                format!("Unknown setting key \"{k}\". Call get_settings without key to list all.")
            })?),
        };
        Ok(settings_text(&self.values().await?, only))
    }

    pub(super) async fn set_setting_text(&self, p: SetSettingParams) -> Result<String, String> {
        let Some(d) = params::def(&p.key) else {
            return Err(format!(
                "Unknown setting key \"{}\". Call get_settings to list all keys.",
                p.key
            ));
        };
        let msg = || {
            format!(
                r#"value for {} must be {}. Example: {{"key": "{}", "value": {}}}"#,
                d.key,
                range_text(d.kind),
                d.key,
                serde_json::to_string(&(d.default)()).unwrap_or_default()
            )
        };
        let value = json_value(d.kind, &p.value).ok_or_else(msg)?;
        let value = params::validate(d.key, &value).map_err(|_| msg())?;
        let before = self.values().await?;
        let changed = before.get(d.key) != Some(&value);
        let rebuild = rebuild_kinds(d.key, &before, value);
        let mut out = set_text(d, value, changed, rebuild);
        if changed {
            let body = Request::SetParam {
                key: d.key.into(),
                value,
            };
            if let Reply::DoneWithWarning { message } = self.call(body).await? {
                let _ = write!(out, "\n{}", rummage(crate::cli::plain(&message)));
            }
        }
        Ok(out)
    }

    pub(super) async fn pause_text(&self, p: PauseParams) -> Result<String, String> {
        let until = (p.minutes > 0)
            .then(|| Local::now().timestamp_millis() + i64::from(p.minutes) * 60_000);
        self.call(Request::PauseIndexing { until }).await?;
        Ok(
            match until.and_then(|t| Local.timestamp_millis_opt(t).single()) {
                Some(t) => format!(
                    "Indexing paused until {} (local time). Search keeps working.",
                    t.format("%Y-%m-%d %H:%M")
                ),
                None => "Indexing paused until resume_indexing is called or the user resumes it in Rummage. Search keeps working.".into(),
            },
        )
    }

    pub(super) async fn resume_text(&self) -> Result<String, String> {
        self.call(Request::ResumeIndexing).await?;
        Ok("Indexing resumed. Automatic pauses (on battery, while a full-screen program runs, low disk space) still apply per the user's settings.".into())
    }

    pub(super) async fn failed_text(&self, p: FailedParams) -> Result<String, String> {
        let body = Request::FailedFiles {
            after: None,
            limit: p.limit,
        };
        let Reply::FailedFiles {
            files,
            total,
            retryable,
        } = self.call(body).await?
        else {
            return Err(unexpected());
        };
        Ok(failed_text(&files, total, retryable))
    }

    pub(super) async fn retry_text(&self) -> Result<String, String> {
        let body = Request::FailedFiles {
            after: None,
            limit: 1,
        };
        let Reply::FailedFiles {
            total, retryable, ..
        } = self.call(body).await?
        else {
            return Err(unexpected());
        };
        if retryable > 0 {
            self.call(Request::RetryAllFailed).await?;
        }
        let mut out = if retryable == 0 {
            "No failed files can be retried.".to_string()
        } else {
            format!("Queued {retryable} failed files for indexing again.")
        };
        if total > retryable {
            let _ = write!(
                out,
                " {} encrypted documents cannot be retried.",
                total - retryable
            );
        }
        Ok(out)
    }
}
