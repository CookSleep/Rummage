//! 配置文件：导出、导入要索引的文件夹（含各自的排除规则）和参数，存成 UTF-8 JSON。
//! - 参数只含 `Group::Resource` 以外、本平台显示的参数；资源类与本机硬件相关，不导出，导入时出现也忽略。
//!   设备检测结果、选用的设备、被屏蔽的显卡、模型、代理、外观、搜索历史都不在配置中。
//! - 只能在同类设备之间导入：`platform` 与本机不同时拒绝。`format` 不对、无法解析、`version` 比本程序支持的新时也拒绝。
//! - 读入的文件是外部输入：参数逐项按参数表校验（键、类型、范围、平台），不合法的丢弃并计数；
//!   文件夹是否存在、能否访问、是否已添加由调用方检查。
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::params::{self, Group, ParamDef, ParamValue};

/// 文件中的 `format`，用来识别 Rummage 的配置
pub const CONFIG_FORMAT: &str = "rummage-config";
/// 配置格式版本：格式不兼容时加 1，旧版本程序据此拒绝新格式
pub const CONFIG_VERSION: u32 = 1;

/// 配置文件的内容；前端不直接读写，对应的类型见 `src/types.ts` 的 `ConfigFile`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigFile {
    pub format: String,
    pub version: u32,
    /// `windows` 或 `android`（同 `std::env::consts::OS`）
    pub platform: String,
    pub app_version: String,
    /// Unix 毫秒
    pub exported_at: i64,
    pub folders: Vec<ConfigFolder>,
    pub params: BTreeMap<String, ParamValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigFolder {
    pub path: String,
    /// 这个文件夹的用户排除规则（同 `Request::SetRootExcludes`）
    #[serde(default)]
    pub excludes: Vec<String>,
    /// 同 `RootInfo::skip_nomedia`
    #[serde(default = "yes")]
    pub skip_nomedia: bool,
}

fn yes() -> bool {
    true
}

/// 校验过的配置：参数已逐项校验，`dropped` 为丢弃的参数数（未知的键、不合法的值、其他平台的参数）
#[derive(Debug, Clone, PartialEq)]
pub struct Imported {
    pub folders: Vec<ConfigFolder>,
    pub params: BTreeMap<String, ParamValue>,
    pub dropped: u32,
}

/// 无法导入的原因，内容是给用户看的中文整句
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ConfigError {
    #[error("这不是 Rummage 的配置文件。")]
    NotConfig,
    #[error("这个配置文件来自更新版本的 Rummage，请先更新 Rummage 再导入。")]
    TooNew,
    #[error("这是在 {0} 上导出的配置，只能导入 {0} 版 Rummage。")]
    Platform(String),
    #[error("配置文件已损坏，无法导入。")]
    Damaged,
}

/// 参数是否进配置：资源类与其他平台的参数不进
fn exportable(d: &ParamDef, os: &str) -> bool {
    d.group != Group::Resource && (d.platform.is_empty() || d.platform == os)
}

/// 由当前的参数值和文件夹生成配置；`os` 为本机平台（`std::env::consts::OS`）
pub fn export(
    values: &BTreeMap<String, ParamValue>,
    folders: Vec<ConfigFolder>,
    os: &str,
    app_version: &str,
    exported_at: i64,
) -> ConfigFile {
    let params = params::PARAMS
        .iter()
        .filter(|d| exportable(d, os))
        .map(|d| (d.key.to_string(), params::value(values, d.key)))
        .collect();
    ConfigFile {
        format: CONFIG_FORMAT.into(),
        version: CONFIG_VERSION,
        platform: os.into(),
        app_version: app_version.into(),
        exported_at,
        folders,
        params,
    }
}

/// 平台在提示中的名称
fn platform_name(os: &str) -> String {
    match os {
        "windows" => "Windows".into(),
        "android" => "Android".into(),
        other => other.into(),
    }
}

/// 解析并校验配置文件的内容；`os` 为本机平台
pub fn parse(text: &str, os: &str) -> Result<Imported, ConfigError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let v: Value = serde_json::from_str(text).map_err(|_| ConfigError::NotConfig)?;
    if v["format"].as_str() != Some(CONFIG_FORMAT) {
        return Err(ConfigError::NotConfig);
    }
    let version = v["version"].as_u64().ok_or(ConfigError::Damaged)?;
    if version > u64::from(CONFIG_VERSION) {
        return Err(ConfigError::TooNew);
    }
    if version == 0 {
        return Err(ConfigError::Damaged);
    }
    let platform = v["platform"].as_str().ok_or(ConfigError::Damaged)?;
    if platform != os {
        return Err(ConfigError::Platform(platform_name(platform)));
    }
    let folders: Vec<ConfigFolder> =
        serde_json::from_value(v["folders"].clone()).map_err(|_| ConfigError::Damaged)?;
    let raw = v["params"].as_object().ok_or(ConfigError::Damaged)?;
    let mut params = BTreeMap::new();
    let mut dropped = 0;
    for (key, value) in raw {
        let def = params::def(key);
        if def.is_some_and(|d| d.group == Group::Resource) {
            continue;
        }
        let valid = def
            .filter(|d| exportable(d, os))
            .and_then(|_| serde_json::from_value::<ParamValue>(value.clone()).ok())
            .and_then(|v| params::validate(key, &v).ok());
        match valid {
            Some(v) => {
                params.insert(key.clone(), v);
            }
            None => dropped += 1,
        }
    }
    Ok(Imported {
        folders,
        params,
        dropped,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn file(over: Value) -> String {
        let mut v = json!({
            "format": CONFIG_FORMAT,
            "version": 1,
            "platform": "windows",
            "app_version": "0.1.0",
            "exported_at": 1_760_000_000_000_i64,
            "folders": [{ "path": "D:\\照片", "excludes": ["*.tmp"] }],
            "params": { "chunk_tokens": 256, "index_video": false },
        });
        for (k, val) in over.as_object().unwrap() {
            v[k] = val.clone();
        }
        v.to_string()
    }

    #[test]
    fn exports_and_parses_back() {
        let values = BTreeMap::from([
            ("chunk_tokens".to_string(), ParamValue::Int(256)),
            ("index_threads".to_string(), ParamValue::Int(3)),
        ]);
        let folders = vec![ConfigFolder {
            path: r"D:\照片".into(),
            excludes: vec!["*.tmp".into()],
            skip_nomedia: false,
        }];
        let cfg = export(&values, folders.clone(), "windows", "0.1.0", 1);
        assert_eq!(cfg.params["chunk_tokens"], ParamValue::Int(256));
        // 资源类、其他平台的参数不导出
        assert!(!cfg.params.contains_key("index_threads"));
        assert!(cfg.params.contains_key("worker_mem_gb"));
        let android = export(&values, vec![], "android", "0.1.0", 1);
        assert!(!android.params.contains_key("worker_mem_gb"));
        let back = parse(&serde_json::to_string_pretty(&cfg).unwrap(), "windows").unwrap();
        assert_eq!(back.folders, folders);
        assert_eq!(back.params, cfg.params);
        assert_eq!(back.dropped, 0);
    }

    #[test]
    fn rejects_files_that_are_not_configs() {
        assert_eq!(parse("不是 JSON", "windows"), Err(ConfigError::NotConfig));
        assert_eq!(parse("[1, 2]", "windows"), Err(ConfigError::NotConfig));
        assert_eq!(
            parse(&file(json!({ "format": "other" })), "windows"),
            Err(ConfigError::NotConfig)
        );
        assert_eq!(
            parse(&file(json!({ "version": "1" })), "windows"),
            Err(ConfigError::Damaged)
        );
        assert_eq!(
            parse(&file(json!({ "folders": [{ "excludes": [] }] })), "windows"),
            Err(ConfigError::Damaged)
        );
        assert_eq!(
            parse(&file(json!({ "params": [1] })), "windows"),
            Err(ConfigError::Damaged)
        );
    }

    #[test]
    fn rejects_newer_versions() {
        assert_eq!(
            parse(&file(json!({ "version": 2 })), "windows"),
            Err(ConfigError::TooNew)
        );
    }

    #[test]
    fn rejects_other_platforms() {
        let err = parse(&file(json!({ "platform": "android" })), "windows").unwrap_err();
        assert_eq!(err, ConfigError::Platform("Android".into()));
        assert_eq!(
            err.to_string(),
            "这是在 Android 上导出的配置，只能导入 Android 版 Rummage。"
        );
        assert_eq!(
            parse(&file(json!({})), "android").unwrap_err().to_string(),
            "这是在 Windows 上导出的配置，只能导入 Windows 版 Rummage。"
        );
    }

    #[test]
    fn drops_invalid_params_and_ignores_resource_ones() {
        let params = json!({
            "chunk_tokens": 256,
            // 浮点参数写成整数也接受
            "min_relevance": 1,
            "no_such_key": 1,
            "chunk_overlap_pct": 99,
            "index_images": "yes",
            "video_preset": 42,
            // 资源类：忽略，不计入丢弃
            "index_threads": 4,
            "pause_on_battery": true,
        });
        let cfg = parse(&file(json!({ "params": params })), "windows").unwrap();
        assert_eq!(
            cfg.params,
            BTreeMap::from([
                ("chunk_tokens".to_string(), ParamValue::Int(256)),
                ("min_relevance".to_string(), ParamValue::Float(1.0)),
            ])
        );
        assert_eq!(cfg.dropped, 4);
        // 只在 Windows 上显示的参数在 Android 上丢弃
        let params = json!({ "worker_mem_gb": 8, "chunk_tokens": 256 });
        let cfg = parse(
            &file(json!({ "platform": "android", "params": params })),
            "android",
        )
        .unwrap();
        assert_eq!(cfg.params.len(), 1);
        assert_eq!(cfg.dropped, 1);
    }

    #[test]
    fn accepts_folders_without_excludes_and_a_bom() {
        let text = format!(
            "\u{feff}{}",
            file(json!({ "folders": [{ "path": "D:\\文档" }] }))
        );
        let cfg = parse(&text, "windows").unwrap();
        assert_eq!(
            cfg.folders,
            vec![ConfigFolder {
                path: r"D:\文档".into(),
                excludes: vec![],
                skip_nomedia: true,
            }]
        );
    }
}
