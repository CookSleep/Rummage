//! 资源占用控制中与平台无关的部分：由检测到的系统状态、手动暂停和参数判定建索引是否暂停。
//! - 自动暂停：任一显示器上有全屏程序（`pause_on_fullscreen`）、使用电池（`pause_on_battery`）、数据目录所在卷的剩余空间低于
//!   `min_free_disk_gb`。检测由平台模块的 `sense` 完成，查询失败的项按“不暂停”处理。
//! - 手动暂停：存在索引数据库 `meta` 表的 `manual_pause` 中（`{"until": Unix 毫秒或 null}`），核心重启后仍然有效；
//!   到期由调用方清除。
//! - 多个原因同时成立时只报一个，优先级：手动 > 磁盘 > 电池 > 全屏。睡眠（`sleeping`）由流水线在报告时插在手动之后
//!   （`pipeline::wake`）。
//! - 检测与时钟可注入（`System`），测试不依赖真实的电源、全屏状态与系统时间。
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rummage_proto::{ParamValue, PauseReason, params};

/// `meta` 表中手动暂停的键
pub(crate) const MANUAL_KEY: &str = "manual_pause";

/// 一次检测到的系统状态
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Sensed {
    pub fullscreen: bool,
    pub battery: bool,
    /// 数据目录所在卷对当前用户可用的空间（GB）；查询失败时为空
    pub free_gb: Option<f64>,
}

/// 检测系统状态与读取时间的方式
pub(crate) struct System {
    /// 参数为数据目录
    pub sense: Box<dyn Fn(&Path) -> Sensed + Send + Sync>,
    /// 当前时间，Unix 毫秒
    pub now: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl System {
    pub fn real() -> Self {
        System {
            sense: Box::new(crate::platform::sense),
            now: Box::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as i64)
            }),
        }
    }
}

/// 手动暂停在 `meta` 表中的写法
pub(crate) fn encode_manual(until: Option<i64>) -> String {
    serde_json::json!({ "until": until }).to_string()
}

/// `meta` 表中的手动暂停；格式不对时返回 `None`
pub(crate) fn decode_manual(raw: &str) -> Option<Option<i64>> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    match v.get("until")? {
        serde_json::Value::Null => Some(None),
        until => Some(Some(until.as_i64()?)),
    }
}

/// 判定暂停原因。`manual` 为未到期的手动暂停：外层为空表示没有手动暂停，内层为到期时间
pub(crate) fn reason(
    manual: Option<Option<i64>>,
    s: &Sensed,
    values: &BTreeMap<String, ParamValue>,
) -> Option<PauseReason> {
    if let Some(until) = manual {
        return Some(PauseReason::Manual { until });
    }
    let min = params::int(values, "min_free_disk_gb") as f64;
    if let Some(gb) = s.free_gb
        && gb < min
    {
        // 取一位小数，剩余空间的微小变化不会每次检测都广播一次
        return Some(PauseReason::DiskLow {
            free_gb: (gb * 10.0).floor() / 10.0,
        });
    }
    if s.battery && params::flag(values, "pause_on_battery") {
        return Some(PauseReason::Battery);
    }
    if s.fullscreen && params::flag(values, "pause_on_fullscreen") {
        return Some(PauseReason::Fullscreen);
    }
    None
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn priority_and_merge() {
        let values = params::defaults();
        let all = Sensed {
            fullscreen: true,
            battery: true,
            free_gb: Some(1.25),
        };
        let r = |manual, s: Sensed| reason(manual, &s, &values);
        assert_eq!(r(None, Sensed::default()), None);
        // 全部成立时手动优先，其后依次是磁盘、电池、全屏
        assert_eq!(
            r(Some(Some(5)), all),
            Some(PauseReason::Manual { until: Some(5) })
        );
        assert_eq!(
            r(Some(None), Sensed::default()),
            Some(PauseReason::Manual { until: None })
        );
        assert_eq!(r(None, all), Some(PauseReason::DiskLow { free_gb: 1.2 }));
        let s = Sensed {
            free_gb: Some(500.0),
            ..all
        };
        assert_eq!(r(None, s), Some(PauseReason::Battery));
        let s = Sensed {
            battery: false,
            ..s
        };
        assert_eq!(r(None, s), Some(PauseReason::Fullscreen));
        // 剩余空间查询失败时不按磁盘不足处理
        let s = Sensed {
            free_gb: None,
            ..Sensed::default()
        };
        assert_eq!(r(None, s), None);
    }

    #[test]
    fn params_switch_conditions() {
        let mut values = params::defaults();
        values.insert("pause_on_battery".into(), ParamValue::Bool(false));
        values.insert("pause_on_fullscreen".into(), ParamValue::Bool(false));
        values.insert("min_free_disk_gb".into(), ParamValue::Int(10));
        let s = Sensed {
            fullscreen: true,
            battery: true,
            free_gb: Some(20.0),
        };
        assert_eq!(reason(None, &s, &values), None);
        let s = Sensed {
            free_gb: Some(9.99),
            ..s
        };
        assert_eq!(
            reason(None, &s, &values),
            Some(PauseReason::DiskLow { free_gb: 9.9 })
        );
    }

    #[test]
    fn manual_codec() {
        assert_eq!(decode_manual(&encode_manual(None)), Some(None));
        assert_eq!(decode_manual(&encode_manual(Some(42))), Some(Some(42)));
        assert_eq!(decode_manual("{"), None);
        assert_eq!(decode_manual(r#"{"until":"x"}"#), None);
        assert_eq!(decode_manual("{}"), None);
    }
}
