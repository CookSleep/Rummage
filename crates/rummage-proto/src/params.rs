//! 统一参数表：所有可调参数的默认值、允许范围、是否影响索引和界面文案都登记在 `PARAMS` 中，
//! 设置界面据此生成，代码中只从这里读取，不另写字面量。
//! 默认值用函数给出，因为有的默认值要到运行时才能确定（如 `index_threads` 按 CPU 核数决定），或按平台不同
//! （函数中用 `cfg!(target_os = "android")` 区分）。只在某个平台上有意义的参数用 `platform` 标出，设置页、命令行与 MCP
//! 的设置列表在其他平台上不显示它（`shown`）。
//! 参数定义表见 `table`。代码中按类型用 `int`、`float`、`flag` 读取参数值。
//! 参数定义不经 IPC 传输，各进程直接读本地的 `PARAMS`（握手已保证两端版本一致）；`ParamDef` 只序列化给前端。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize, Serializer};

use crate::Error;

mod table;

pub use table::PARAMS;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ParamValue {
    Bool(bool),
    Int(i64),
    Float(f64),
}

#[derive(Debug, Clone, Copy, Serialize)]
pub enum ParamKind {
    Bool,
    Int {
        min: i64,
        max: i64,
    },
    Float {
        min: f64,
        max: f64,
    },
    /// 只能取列出的值之一，界面显示各值的名称（如视频预设）
    Named {
        options: &'static [(i64, &'static str)],
        /// 值本身有意义时的单位（如视觉 token 档位的“Token”），界面在名称后写出数值，如“标准（140 Token）”；
        /// 值只是编号时为空，只显示名称
        unit: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Group {
    Search,
    Index,
    Resource,
    Advanced,
}

/// 界面进程通过 Tauri 命令交给前端（`src/types.ts`）
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamDef {
    pub key: &'static str,
    pub group: Group,
    /// 设置页中的小标题，同组同小标题的参数在 `PARAMS` 中相邻排列，界面按顺序分组；
    /// 为空时不在参数列表中显示（文件类型开关、搜索历史、日志在各自的分区中显示）
    pub section: &'static str,
    pub kind: ParamKind,
    /// 有的默认值要到运行时才能确定；序列化时输出调用结果，derive 只能用 serialize_with 做到
    #[serde(serialize_with = "call_default")]
    pub default: fn() -> ParamValue,
    /// 修改后需要重建受影响的索引
    pub affects_index: bool,
    /// `affects_index` 为真时受影响的文件种类（`image`、`document`、`audio`、`video`），否则为空。
    /// 核心据此只重建这几类，界面据此在确认时说明影响哪些索引
    pub index_kinds: &'static [&'static str],
    /// 界面名称
    pub label: &'static str,
    /// 一句说明
    pub help: &'static str,
    /// 默认值的依据
    pub basis: &'static str,
    /// 只在此平台上显示（`android`、`windows`，同 `std::env::consts::OS`）；为空时各平台都显示
    pub platform: &'static str,
}

impl ParamDef {
    /// 当前平台是否显示此参数
    pub fn shown(&self) -> bool {
        self.platform.is_empty() || self.platform == std::env::consts::OS
    }
}

fn call_default<S: Serializer>(f: &fn() -> ParamValue, s: S) -> Result<S::Ok, S::Error> {
    f().serialize(s)
}

pub fn def(key: &str) -> Option<&'static ParamDef> {
    PARAMS.iter().find(|d| d.key == key)
}

/// `values` 中参数 `key` 的值；缺少或不合法时用默认值。`key` 须登记在参数表中
pub fn value(values: &BTreeMap<String, ParamValue>, key: &str) -> ParamValue {
    let def = def(key).expect("调用方用到的参数都登记在参数表中");
    values
        .get(key)
        .and_then(|v| validate(key, v).ok())
        .unwrap_or_else(def.default)
}

/// `values` 中整数参数（含选项参数）`key` 的值；缺少或不合法时用默认值
pub fn int(values: &BTreeMap<String, ParamValue>, key: &str) -> i64 {
    match value(values, key) {
        ParamValue::Int(n) => n,
        v => unreachable!("{key} 是整数参数，校验过的值不会是 {v:?}"),
    }
}

/// `values` 中浮点参数 `key` 的值；缺少或不合法时用默认值
pub fn float(values: &BTreeMap<String, ParamValue>, key: &str) -> f64 {
    match value(values, key) {
        ParamValue::Float(x) => x,
        v => unreachable!("{key} 是浮点参数，校验过的值不会是 {v:?}"),
    }
}

/// `values` 中开关参数 `key` 的值；缺少或不合法时用默认值
pub fn flag(values: &BTreeMap<String, ParamValue>, key: &str) -> bool {
    match value(values, key) {
        ParamValue::Bool(b) => b,
        v => unreachable!("{key} 是开关参数，校验过的值不会是 {v:?}"),
    }
}

/// 文档大小上限 `max_doc_mb`，换算为字节
pub fn max_doc_bytes(values: &BTreeMap<String, ParamValue>) -> u64 {
    int(values, "max_doc_mb") as u64 * 1024 * 1024
}

/// 图片大小上限 `max_image_mb`，换算为字节
pub fn max_image_bytes(values: &BTreeMap<String, ParamValue>) -> u64 {
    int(values, "max_image_mb") as u64 * 1024 * 1024
}

/// 图片像素数上限 `max_image_mp`，换算为像素
pub fn max_image_pixels(values: &BTreeMap<String, ParamValue>) -> u64 {
    int(values, "max_image_mp") as u64 * 1_000_000
}

/// 校验通过时返回规范化后的值：浮点参数收到整数时转为 `Float`
pub fn validate(key: &str, value: &ParamValue) -> Result<ParamValue, Error> {
    let Some(d) = def(key) else {
        return Err(Error::InvalidParam(format!("没有名为“{key}”的参数。")));
    };
    let bad = |msg: String| Err(Error::InvalidParam(format!("“{}”{msg}", d.label)));
    match (d.kind, *value) {
        (ParamKind::Bool, ParamValue::Bool(_)) => Ok(*value),
        (ParamKind::Bool, _) => bad("应为开关。".into()),
        (ParamKind::Int { min, max }, ParamValue::Int(v)) if (min..=max).contains(&v) => Ok(*value),
        (ParamKind::Int { min, max }, ParamValue::Int(_)) => {
            bad(format!("应在 {min} 到 {max} 之间。"))
        }
        (ParamKind::Int { .. }, _) => bad("应为整数。".into()),
        (ParamKind::Float { min, max }, ParamValue::Float(v)) if (min..=max).contains(&v) => {
            Ok(*value)
        }
        // 前端 JSON 中的 1.0 会写成 1，解码为 Int，这里转回 Float
        (ParamKind::Float { min, max }, ParamValue::Int(v))
            if (min..=max).contains(&(v as f64)) =>
        {
            Ok(ParamValue::Float(v as f64))
        }
        (ParamKind::Float { min, max }, ParamValue::Int(_) | ParamValue::Float(_)) => {
            bad(format!("应在 {min} 到 {max} 之间。"))
        }
        (ParamKind::Float { .. }, _) => bad("应为数字。".into()),
        (ParamKind::Named { options, .. }, ParamValue::Int(v))
            if options.iter().any(|o| o.0 == v) =>
        {
            Ok(*value)
        }
        (ParamKind::Named { options, .. }, _) => {
            let list: Vec<&str> = options.iter().map(|o| o.1).collect();
            bad(format!("只能取“{}”之一。", list.join("”“")))
        }
    }
}

pub fn defaults() -> BTreeMap<String, ParamValue> {
    PARAMS
        .iter()
        .map(|d| (d.key.to_string(), (d.default)()))
        .collect()
}

#[cfg(test)]
mod tests;
