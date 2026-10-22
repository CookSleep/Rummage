//! 命令行与 MCP 共用的“查看与操作应用”部分：文件夹、参数、暂停与失败文件。
//! 两边发出的请求相同（核心已有的 `ListRoots`、`AddRoot`、`SetParam` 等），这里只放与输出语言无关的判断：
//! - 识别内容的文件类型是全局参数 `index_documents` 等（所有文件夹共用，同界面“设置 → 文件夹”的开关），
//!   给文件夹指定类型即改这几个参数。
//! - 参数值按参数表解析与校验；是否会重建索引、重建哪几类与核心 `SetParam` 的判断一致（视频的高级项只在
//!   “自定义”时生效，实际设置没变就不重建）。
//! - 设备选择、模型下载与删除、清空索引、数据目录不开放（不是参数，也不提供对应的子命令或工具）。
use std::collections::BTreeMap;

use rummage_core::pipeline::video_opts;
use rummage_proto::{IndexStatus, ParamDef, ParamKind, ParamValue, RootInfo, params};

use crate::cli::folded;

/// 可以识别内容的文件类型及对应的参数
pub const CONTENT_KINDS: [(&str, &str); 4] = [
    ("document", "index_documents"),
    ("image", "index_images"),
    ("audio", "index_audio"),
    ("video", "index_video"),
];

/// 暂停分钟数的上限：一周
pub const MAX_PAUSE_MIN: u32 = 7 * 24 * 60;

/// 已处理（完成或失败）的文件占总数的百分比；没有文件时为 100
pub fn progress_pct(i: &IndexStatus) -> u64 {
    ((i.done + i.failed) * 100)
        .checked_div(i.total)
        .unwrap_or(100)
}

/// 毫秒写成 mm:ss，满一小时时为 h:mm:ss
pub fn clock(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m, s) = (s / 3600, s % 3600 / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// 开启了内容识别的类型；没有保存过的参数按默认值（开启）
pub fn enabled_kinds(values: &BTreeMap<String, ParamValue>) -> Vec<&'static str> {
    CONTENT_KINDS
        .iter()
        .filter(|(_, key)| params::flag(values, key))
        .map(|(kind, _)| *kind)
        .collect()
}

/// 把开启的类型改为 `kinds` 要修改的参数（只含值有变化的）
pub fn kind_changes(
    values: &BTreeMap<String, ParamValue>,
    kinds: &[String],
) -> Vec<(&'static str, ParamValue)> {
    let on = enabled_kinds(values);
    CONTENT_KINDS
        .iter()
        .filter_map(|(kind, key)| {
            let want = kinds.iter().any(|k| k == kind);
            (want != on.contains(kind)).then_some((*key, ParamValue::Bool(want)))
        })
        .collect()
}

/// 文字写的参数值（命令行，或 MCP 中以字符串给出的）：开关接受 true/false、on/off、yes/no、1/0、开/关，
/// 有名称的选项也接受名称（如“找物体”）。只解析类型，范围由 `params::validate` 校验
pub fn parse_value(kind: ParamKind, raw: &str) -> Option<ParamValue> {
    let raw = raw.trim();
    match kind {
        ParamKind::Bool => match raw.to_lowercase().as_str() {
            "true" | "on" | "yes" | "1" | "开" | "开启" => Some(ParamValue::Bool(true)),
            "false" | "off" | "no" | "0" | "关" | "关闭" => Some(ParamValue::Bool(false)),
            _ => None,
        },
        ParamKind::Int { .. } => raw.parse().ok().map(ParamValue::Int),
        ParamKind::Float { .. } => raw
            .parse()
            .ok()
            .filter(|v: &f64| v.is_finite())
            .map(ParamValue::Float),
        ParamKind::Named { options, .. } => raw.parse().ok().map(ParamValue::Int).or_else(|| {
            options
                .iter()
                .find(|o| o.1 == raw)
                .map(|o| ParamValue::Int(o.0))
        }),
    }
}

/// 改为 `value` 后要重建的文件种类；没有变化或此项在当前设置下不生效时为空
pub fn rebuild_kinds(
    key: &str,
    before: &BTreeMap<String, ParamValue>,
    value: ParamValue,
) -> &'static [&'static str] {
    let Some(def) = params::def(key) else {
        return &[];
    };
    if !def.affects_index || before.get(key) == Some(&value) {
        return &[];
    }
    if def.index_kinds == ["video"] {
        let mut after = before.clone();
        after.insert(key.to_string(), value);
        if video_opts(&after) == video_opts(before) {
            return &[];
        }
    }
    def.index_kinds
}

/// 按路径找已索引的文件夹：不区分大小写，忽略末尾的分隔符与 `/`、`\` 的区别
pub fn find_root<'a>(roots: &'a [RootInfo], path: &str) -> Option<&'a RootInfo> {
    let want = folded(path);
    roots.iter().find(|r| folded(&r.path) == want)
}

/// 参数值的文字，同界面：有单位的选项写成“标准（140 Token）”，没有单位时只写名称“找物体”；
/// `ascii` 为是时用半角括号（MCP 的英文输出）：“标准 (140 Token)”
pub fn value_text(kind: ParamKind, v: ParamValue, ascii: bool) -> String {
    match (kind, v) {
        (ParamKind::Named { options, unit }, ParamValue::Int(n)) => {
            match (options.iter().find(|o| o.0 == n), unit.is_empty()) {
                (Some(o), true) => o.1.to_string(),
                (Some(o), false) if ascii => format!("{} ({n} {unit})", o.1),
                (Some(o), false) => format!("{}（{n} {unit}）", o.1),
                (None, _) => n.to_string(),
            }
        }
        (_, ParamValue::Bool(b)) => b.to_string(),
        (_, ParamValue::Int(n)) => n.to_string(),
        (_, ParamValue::Float(f)) => f.to_string(),
    }
}

/// 参数的当前值，没有保存过时为默认值
pub fn current(values: &BTreeMap<String, ParamValue>, d: &ParamDef) -> ParamValue {
    values.get(d.key).copied().unwrap_or_else(d.default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_from_text() {
        let def = |k| params::def(k).unwrap().kind;
        assert_eq!(
            parse_value(def("save_history"), " Off "),
            Some(ParamValue::Bool(false))
        );
        assert_eq!(
            parse_value(def("save_history"), "开"),
            Some(ParamValue::Bool(true))
        );
        assert_eq!(parse_value(def("save_history"), "maybe"), None);
        assert_eq!(
            parse_value(def("audio_silence_db"), "-60"),
            Some(ParamValue::Int(-60))
        );
        assert_eq!(parse_value(def("chunk_tokens"), "2.5"), None);
        assert_eq!(
            parse_value(def("min_relevance"), "1.5"),
            Some(ParamValue::Float(1.5))
        );
        assert_eq!(parse_value(def("min_relevance"), "NaN"), None);
        // 有名称的选项：数值或名称
        assert_eq!(
            parse_value(def("video_preset"), "找动作"),
            Some(ParamValue::Int(2))
        );
        assert_eq!(
            parse_value(def("image_tokens"), "280"),
            Some(ParamValue::Int(280))
        );
        assert_eq!(
            value_text(def("image_tokens"), ParamValue::Int(280), false),
            "标准（280 Token）"
        );
        assert_eq!(
            value_text(def("image_tokens"), ParamValue::Int(280), true),
            "标准 (280 Token)"
        );
        assert_eq!(
            value_text(def("video_preset"), ParamValue::Int(1), false),
            "找物体"
        );
    }

    #[test]
    fn content_kinds() {
        let mut values = params::defaults();
        assert_eq!(
            enabled_kinds(&values),
            ["document", "image", "audio", "video"]
        );
        values.insert("index_audio".into(), ParamValue::Bool(false));
        assert_eq!(enabled_kinds(&values), ["document", "image", "video"]);
        // 只改有变化的
        let changes = kind_changes(&values, &["document".into(), "audio".into()]);
        assert_eq!(
            changes,
            [
                ("index_images", ParamValue::Bool(false)),
                ("index_audio", ParamValue::Bool(true)),
                ("index_video", ParamValue::Bool(false)),
            ]
        );
    }

    #[test]
    fn rebuilds() {
        let mut values = params::defaults();
        assert_eq!(
            rebuild_kinds("chunk_tokens", &values, ParamValue::Int(256)),
            ["document"]
        );
        assert!(rebuild_kinds("chunk_tokens", &values, ParamValue::Int(384)).is_empty());
        assert!(rebuild_kinds("result_limit", &values, ParamValue::Int(100)).is_empty());
        // 视频的高级项只在“自定义”时生效
        assert!(rebuild_kinds("video_window_s", &values, ParamValue::Int(8)).is_empty());
        assert_eq!(
            rebuild_kinds("video_preset", &values, ParamValue::Int(2)),
            ["video"]
        );
        values.insert("video_preset".into(), ParamValue::Int(0));
        assert_eq!(
            rebuild_kinds("video_window_s", &values, ParamValue::Int(8)),
            ["video"]
        );
    }

    #[test]
    fn roots_by_path() {
        let root = |id, path: &str| RootInfo {
            id,
            path: path.into(),
            offline: false,
            missing: false,
            files: 0,
            excludes: Vec::new(),
            skip_nomedia: true,
            scanning: false,
        };
        let roots = [root(1, r"D:\照片"), root(2, r"E:\Docs")];
        assert_eq!(find_root(&roots, r"e:/docs\").map(|r| r.id), Some(2));
        assert!(find_root(&roots, r"D:\照片\2026").is_none());
    }
}
