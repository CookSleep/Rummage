//! MCP 工具的参数、校验与分页。
//! - 可选参数都不是 `Option`，用 `#[serde(default)]`，schema 中不出现 `null` 类型；空字符串、空数组视为未给出。
//! - 解析放宽：数值参数也接受数字字符串，`kinds`、`folders` 也接受单个字符串，`null` 视为未给出；类型不对时的报错附正确写法
//!   （rmcp 在前面加上 “failed to deserialize parameters: ”，以 `isError` 结果返回）。
//! - 数量的上限：`MAX_LIMIT` 是一次最多返回的条数；`MAX_TOTAL` 同参数表 `result_limit` 的上限（校验时以参数表为准，
//!   测试核对两者一致），schema 与说明文字用它。字段说明（`///`）是字面量，不能由常量生成，改常量时要同步修改，
//!   测试会核对说明中的数字。
use std::path::Path;

use chrono::NaiveDate;
use rmcp::schemars::{self, JsonSchema, Schema, SchemaGenerator, json_schema};
use rummage_proto::SearchHit;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use super::{bad, kinds_param};
use crate::cli::export::ext;
use crate::cli::{KINDS, limit_range, parse_date};

/// 默认返回的结果数
pub const DEFAULT_LIMIT: u32 = 10;
/// 一次最多返回的结果数
pub const MAX_LIMIT: u32 = 50;
/// offset + limit 的上限，同参数表 `result_limit` 的上限（`tests::max_total_matches_params` 核对）
pub const MAX_TOTAL: u32 = 500;
/// offset 的上限：至少还要取 1 条
pub const MAX_OFFSET: u32 = MAX_TOTAL - 1;

const FOLDERS_EXAMPLE: &str =
    r#"folders must be absolute folder paths. Example: {"folders": ["D:\\Photos\\2024"]}"#;

fn default_limit() -> u32 {
    DEFAULT_LIMIT
}

/// 字符串参数：`null` 视为未给出
fn text<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    match Value::deserialize(d)? {
        Value::Null => Ok(String::new()),
        Value::String(s) => Ok(s),
        v => Err(bad(format!(
            r#"expected a string but got {v}. Example: {{"query": "hotel receipt from Kyoto"}}"#
        ))),
    }
}

/// 整数参数：数字或数字字符串，`null` 视为未给出（`None`）
pub(super) fn int(v: Value) -> Result<Option<i64>, ()> {
    match v {
        Value::Null => Ok(None),
        Value::Number(n) => n.as_i64().map(Some).ok_or(()),
        Value::String(s) if s.trim().is_empty() => Ok(None),
        Value::String(s) => s.trim().parse().map(Some).map_err(|_| ()),
        _ => Err(()),
    }
}

fn limit<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    let msg = || {
        format!(
            r#"limit must be an integer from 1 to {MAX_LIMIT}. Example: {{"limit": {DEFAULT_LIMIT}}}"#
        )
    };
    match int(Value::deserialize(d)?) {
        Ok(None) => Ok(DEFAULT_LIMIT),
        // 超出范围的留给 check_search 报错
        Ok(Some(n)) => Ok(u32::try_from(n).unwrap_or(0)),
        Err(()) => Err(bad(msg())),
    }
}

fn offset<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    match int(Value::deserialize(d)?) {
        Ok(None) => Ok(0),
        Ok(Some(n)) if n >= 0 => Ok(u32::try_from(n).unwrap_or(u32::MAX)),
        _ => Err(bad(format!(
            r#"offset must be an integer from 0 to {MAX_OFFSET}. Example: {{"offset": 10}}"#
        ))),
    }
}

fn hit_id<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    match int(Value::deserialize(d)?) {
        Ok(Some(n)) => Ok(n),
        _ => Err(bad(
            r#"hit_id must be an integer from a search_files result. Example: {"hit_id": 18342}"#
                .into(),
        )),
    }
}

fn flag<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    match Value::deserialize(d)? {
        Value::Null => Ok(false),
        Value::Bool(b) => Ok(b),
        Value::String(s) if s.eq_ignore_ascii_case("true") => Ok(true),
        Value::String(s) if s.eq_ignore_ascii_case("false") || s.is_empty() => Ok(false),
        _ => Err(bad(
            r#"include_weak must be true or false. Example: {"include_weak": true}"#.into(),
        )),
    }
}

/// 文件类型：单个字符串或字符串数组，不区分大小写，去重
fn kinds<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let msg = format!(
        r#"kinds must be one or more of {}. Example: {{"kinds": ["image", "video"]}}"#,
        KINDS.join(", ")
    );
    kinds_param(d, &KINDS, msg)
}

fn kinds_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "array",
        "items": {"type": "string", "enum": KINDS},
        "description": "Limit to these file types; omit for all. 'other' = files indexed by name only.",
    })
}

/// 文件夹：单个字符串或字符串数组，去掉首尾空白，跳过空项
fn folders<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let items = match Value::deserialize(d)? {
        Value::Null => Vec::new(),
        Value::Array(a) => a,
        v => vec![v],
    };
    let mut out = Vec::new();
    for item in items {
        let Value::String(s) = item else {
            return Err(bad(FOLDERS_EXAMPLE.into()));
        };
        if let Some(s) = given(s) {
            out.push(s);
        }
    }
    Ok(out)
}

fn folders_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "array",
        "items": {"type": "string"},
        "description": "Limit to these folders; omit for all. Each is an absolute path of an indexed folder (as listed by list_folders) or of any folder inside one.",
    })
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// What to find, in natural language or keywords, in any language. Required unless image_path is set; with image_path it adds a text description to the image query.
    #[serde(default, deserialize_with = "text")]
    pub query: String,
    /// Absolute path of a local image to search by example. The image is read only to compute the query.
    #[serde(default, deserialize_with = "text")]
    pub image_path: String,
    #[serde(default, deserialize_with = "kinds")]
    #[schemars(schema_with = "kinds_schema")]
    pub kinds: Vec<String>,
    #[serde(default, deserialize_with = "folders")]
    #[schemars(schema_with = "folders_schema")]
    pub folders: Vec<String>,
    /// Only files modified on or after this date: YYYY-MM-DD, local time.
    #[serde(default, deserialize_with = "text")]
    pub modified_after: String,
    /// Only files modified on or before this date: YYYY-MM-DD, local time.
    #[serde(default, deserialize_with = "text")]
    pub modified_before: String,
    // 说明中的 1-50 同 MAX_LIMIT，要同步修改
    /// Number of results to return, 1-50. Default 10.
    #[serde(default = "default_limit", deserialize_with = "limit")]
    #[schemars(range(min = 1, max = MAX_LIMIT))]
    pub limit: u32,
    // 说明中的 500 同 MAX_TOTAL，要同步修改
    /// Skip this many results, for paging. offset + limit must not exceed 500.
    #[serde(default, deserialize_with = "offset")]
    #[schemars(range(min = 0, max = MAX_OFFSET))]
    pub offset: u32,
    /// Also return results below the match threshold (usually noise). Default false.
    #[serde(default, deserialize_with = "flag")]
    pub include_weak: bool,
}

/// `save_search_results` 的参数：筛选同 `SearchParams`，没有分页，多了保存的位置
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExportParams {
    /// Absolute path of the new file, ending in .csv, .json or .txt (one path per line). It must not exist yet.
    #[serde(deserialize_with = "text")]
    pub path: String,
    /// What to find, as for search_files.
    #[serde(default, deserialize_with = "text")]
    pub query: String,
    /// Absolute path of a local image to search by example.
    #[serde(default, deserialize_with = "text")]
    pub image_path: String,
    #[serde(default, deserialize_with = "kinds")]
    #[schemars(schema_with = "kinds_schema")]
    pub kinds: Vec<String>,
    #[serde(default, deserialize_with = "folders")]
    #[schemars(schema_with = "folders_schema")]
    pub folders: Vec<String>,
    /// Only files modified on or after this date: YYYY-MM-DD, local time.
    #[serde(default, deserialize_with = "text")]
    pub modified_after: String,
    /// Only files modified on or before this date: YYYY-MM-DD, local time.
    #[serde(default, deserialize_with = "text")]
    pub modified_before: String,
    /// Also save results below the match threshold (usually noise). Default false.
    #[serde(default, deserialize_with = "flag")]
    pub include_weak: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ContextParams {
    /// hit_id from a search_files result.
    #[serde(deserialize_with = "hit_id")]
    pub hit_id: i64,
}

/// 校验后的搜索参数
#[derive(Debug, Clone, PartialEq)]
pub struct Search {
    pub text: Option<String>,
    pub image: Option<String>,
    pub kinds: Vec<String>,
    /// 绝对路径
    pub folders: Vec<String>,
    pub since: Option<NaiveDate>,
    pub until: Option<NaiveDate>,
    pub limit: u32,
    pub offset: u32,
    pub include_weak: bool,
    pub export: Option<String>,
}

/// 去掉首尾空白，空字符串视为未给出
fn given(s: String) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// 参数校验；不对时返回给模型看的英文整句，附正确写法
pub fn check_search(p: SearchParams) -> Result<Search, String> {
    let text = given(p.query);
    let image = given(p.image_path);
    if text.is_none() && image.is_none() {
        return Err(
            r#"Provide query, image_path, or both. Example: {"query": "hotel receipt from Kyoto"}"#
                .into(),
        );
    }
    if let Some(i) = &image
        && !Path::new(i).is_absolute()
    {
        return Err(
            r#"image_path must be an absolute path. Example: {"image_path": "C:\\Users\\me\\Pictures\\cat.jpg"}"#
                .into(),
        );
    }
    if p.folders.iter().any(|f| !Path::new(f).is_absolute()) {
        return Err(FOLDERS_EXAMPLE.into());
    }
    let date = |v: String, name: &str| {
        given(v)
            .map(|d| {
                parse_date(&d, name).map_err(|_| {
                    format!(r#"{name} must be a date written as YYYY-MM-DD. Example: {{"{name}": "2026-01-05"}}"#)
                })
            })
            .transpose()
    };
    let since = date(p.modified_after, "modified_after")?;
    let until = date(p.modified_before, "modified_before")?;
    if let (Some(a), Some(b)) = (since, until)
        && a > b
    {
        return Err("modified_after must not be later than modified_before.".into());
    }
    if !(1..=MAX_LIMIT).contains(&p.limit) {
        return Err(format!(
            r#"limit must be an integer from 1 to {MAX_LIMIT}. Example: {{"limit": {DEFAULT_LIMIT}}}"#
        ));
    }
    // 先比较再相加，offset 很大时不会溢出
    let max = limit_range().1;
    if p.offset > max.saturating_sub(p.limit) {
        return Err(format!(
            "offset + limit must not exceed {max}. Narrow the search with kinds, folders or dates instead of paging further."
        ));
    }
    Ok(Search {
        text,
        image,
        kinds: p.kinds,
        folders: p.folders,
        since,
        until,
        limit: p.limit,
        offset: p.offset,
        include_weak: p.include_weak,
        export: None,
    })
}

/// `save_search_results` 的参数校验：筛选同 `check_search`，再校验保存的位置
pub fn check_export(p: ExportParams) -> Result<Search, String> {
    let path = given(p.path).filter(|e| {
        let e = Path::new(e);
        e.is_absolute() && ext(e).is_some()
    });
    let Some(path) = path else {
        return Err(
            r#"path must be an absolute path ending in .csv, .json or .txt. Example: {"path": "C:\\Users\\me\\Desktop\\results.csv"}"#
                .into(),
        );
    };
    let s = check_search(SearchParams {
        query: p.query,
        image_path: p.image_path,
        kinds: p.kinds,
        folders: p.folders,
        modified_after: p.modified_after,
        modified_before: p.modified_before,
        limit: DEFAULT_LIMIT,
        offset: 0,
        include_weak: p.include_weak,
    })?;
    Ok(Search {
        export: Some(path),
        ..s
    })
}

/// 向核心请求的数量：要覆盖到这一页的末尾，且不低于核心接受的下限；要保存到文件时取到上限
pub fn request_limit(s: &Search) -> u32 {
    if s.export.is_some() {
        return limit_range().1;
    }
    s.offset.saturating_add(s.limit).max(limit_range().0)
}

/// 一页结果
#[derive(Debug)]
pub struct Page<'a> {
    pub hits: &'a [SearchHit],
    /// 这一页第一条的序号（从 0 起）
    pub offset: u32,
    /// 没有列出的匹配度较低的结果，至少这么多（只数核心按数量返回的那些）
    pub weak_omitted: usize,
    /// 可能还有更多结果时，下一页的 offset
    pub next_offset: Option<u32>,
}

/// 从核心按 `request_limit` 返回的结果中取出这一页；`kept` 是按 `include_weak` 筛过的结果
pub fn page<'a>(s: &Search, returned: usize, kept: &'a [SearchHit], weak: usize) -> Page<'a> {
    let next = s.offset.saturating_add(s.limit);
    let start = (s.offset as usize).min(kept.len());
    let end = (next as usize).min(kept.len());
    // 核心按数量上限截断了，后面可能还有；已到参数表的上限时不再翻页
    let more = returned >= request_limit(s) as usize && end == next as usize;
    Page {
        hits: &kept[start..end],
        offset: s.offset,
        weak_omitted: weak,
        next_offset: (more && next < limit_range().1).then_some(next),
    }
}
