//! 资源占用控制中与平台无关的部分：由检测到的系统状态、手动暂停和参数判定建索引是否暂停、是否降温。
//! - 自动暂停：数据目录所在卷的剩余空间低于 `min_free_disk_gb`；Android 前台服务今日的后台额度已用完（`Quota`，由界面经
//!   `BackgroundQuota` 告知，不持久化）；手机过热（热状态达到 `SEVERE` 或温度余量达到 1.0，`pause_when_hot`）；未充电
//!   （`pause_on_battery`）；未充电且电量低于 `low_battery_percent`（`pause_on_low_battery`，接上电源即解除）；
//!   Android 亮屏（`pause_when_screen_on`）；任一显示器上有全屏程序（`pause_on_fullscreen`）。
//!   检测由平台模块的 `sense` 完成，查询失败的项按“不暂停”处理；Windows 上热状态、亮屏两项始终为默认值。
//! - 降温（`slow`，`slow_when_hot`）：温度余量达到 `thermal_slow_headroom`（百分数）或热状态为 `MODERATE` 及以上时，流水线按 50% 占空比领取作业
//!   （`pipeline::schedule`）。降温不是暂停，不报暂停原因。
//! - 手动暂停：存在索引数据库 `meta` 表的 `manual_pause` 中（`{"until": Unix 毫秒或 null}`），核心重启后仍然有效；
//!   到期由调用方清除。
//! - 多个原因同时成立时只报一个，优先级：手动 > 磁盘 > 额度 > 过热 > 电池 > 低电量 > 亮屏 > 全屏。睡眠（`sleeping`）由流水线在报告时
//!   插在手动之后（`pipeline::wake`）。
//! - 检测与时钟可注入（`System`），测试不依赖真实的电源、全屏状态与系统时间。
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rummage_proto::{ParamValue, PauseReason, params};

/// `meta` 表中手动暂停的键
pub(crate) const MANUAL_KEY: &str = "manual_pause";

/// `PowerManager.THERMAL_STATUS_MODERATE`
pub(crate) const THERMAL_MODERATE: i32 = 2;
/// `PowerManager.THERMAL_STATUS_SEVERE`
pub(crate) const THERMAL_SEVERE: i32 = 3;

/// 一次检测到的系统状态
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Sensed {
    pub fullscreen: bool,
    /// 使用电池供电（未充电）
    pub battery: bool,
    /// 剩余电量（%）；没有电池或查询失败时为空
    pub battery_percent: Option<u8>,
    /// 屏幕亮着（Android `PowerManager.isInteractive`）
    pub screen_on: bool,
    /// Android 的热状态（`PowerManager.getCurrentThermalStatus`，0 为 `NONE`）
    pub thermal: i32,
    /// Android 预测的温度余量（`PowerManager.getThermalHeadroom`），1.0 对应 `SEVERE`；不支持或调用过于频繁时为空
    pub headroom: Option<f64>,
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

/// 判定暂停原因。`manual` 为未到期的手动暂停：外层为空表示没有手动暂停，内层为到期时间；`quota` 为今日后台额度已用完
pub(crate) fn reason(
    manual: Option<Option<i64>>,
    quota: bool,
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
    if quota {
        return Some(PauseReason::Quota);
    }
    if (s.thermal >= THERMAL_SEVERE || s.headroom.is_some_and(|h| h >= 1.0))
        && params::flag(values, "pause_when_hot")
    {
        return Some(PauseReason::Hot);
    }
    if s.battery && params::flag(values, "pause_on_battery") {
        return Some(PauseReason::Battery);
    }
    if s.battery
        && s.battery_percent
            .is_some_and(|p| i64::from(p) < params::int(values, "low_battery_percent"))
        && params::flag(values, "pause_on_low_battery")
    {
        return Some(PauseReason::LowBattery);
    }
    if s.screen_on && params::flag(values, "pause_when_screen_on") {
        return Some(PauseReason::ScreenOn);
    }
    if s.fullscreen && params::flag(values, "pause_on_fullscreen") {
        return Some(PauseReason::Fullscreen);
    }
    None
}

/// 是否降温（见模块说明）
pub(crate) fn slow(s: &Sensed, values: &BTreeMap<String, ParamValue>) -> bool {
    if !params::flag(values, "slow_when_hot") {
        return false;
    }
    s.thermal >= THERMAL_MODERATE
        || s.headroom
            .is_some_and(|h| h * 100.0 >= params::int(values, "thermal_slow_headroom") as f64)
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn priority_and_merge() {
        let mut values = params::defaults();
        values.insert("pause_on_battery".into(), ParamValue::Bool(true));
        values.insert("pause_when_screen_on".into(), ParamValue::Bool(true));
        let all = Sensed {
            fullscreen: true,
            battery: true,
            battery_percent: None,
            screen_on: true,
            thermal: THERMAL_SEVERE,
            headroom: Some(1.2),
            free_gb: Some(1.25),
        };
        let r = |manual, quota, s: Sensed| reason(manual, quota, &s, &values);
        assert_eq!(r(None, false, Sensed::default()), None);
        // 全部成立时手动优先，其后依次是磁盘、额度、过热、电池、亮屏、全屏
        assert_eq!(
            r(Some(Some(5)), true, all),
            Some(PauseReason::Manual { until: Some(5) })
        );
        assert_eq!(
            r(Some(None), false, Sensed::default()),
            Some(PauseReason::Manual { until: None })
        );
        assert_eq!(
            r(None, true, all),
            Some(PauseReason::DiskLow { free_gb: 1.2 })
        );
        let s = Sensed {
            free_gb: Some(500.0),
            ..all
        };
        assert_eq!(r(None, true, s), Some(PauseReason::Quota));
        assert_eq!(r(None, false, s), Some(PauseReason::Hot));
        // 热状态与温度余量任一达到即为过热
        let s = Sensed { thermal: 0, ..s };
        assert_eq!(r(None, false, s), Some(PauseReason::Hot));
        let s = Sensed {
            thermal: THERMAL_SEVERE,
            headroom: None,
            ..s
        };
        assert_eq!(r(None, false, s), Some(PauseReason::Hot));
        let s = Sensed {
            thermal: THERMAL_MODERATE,
            headroom: Some(0.99),
            ..s
        };
        assert_eq!(r(None, false, s), Some(PauseReason::Battery));
        let mut low = values.clone();
        low.insert("pause_on_battery".into(), ParamValue::Bool(false));
        let s = Sensed {
            battery_percent: Some(39),
            ..s
        };
        assert_eq!(reason(None, false, &s, &low), Some(PauseReason::LowBattery));
        // 不低于门槛、查不到电量或已接上电源时不暂停
        for s in [
            Sensed {
                battery_percent: Some(40),
                ..s
            },
            Sensed {
                battery_percent: None,
                ..s
            },
            Sensed {
                battery: false,
                ..s
            },
        ] {
            assert_eq!(reason(None, false, &s, &low), Some(PauseReason::ScreenOn));
        }
        low.insert("pause_on_low_battery".into(), ParamValue::Bool(false));
        assert_eq!(reason(None, false, &s, &low), Some(PauseReason::ScreenOn));
        let s = Sensed {
            battery: false,
            ..s
        };
        assert_eq!(r(None, false, s), Some(PauseReason::ScreenOn));
        let s = Sensed {
            screen_on: false,
            ..s
        };
        assert_eq!(r(None, false, s), Some(PauseReason::Fullscreen));
        // 剩余空间查询失败时不按磁盘不足处理
        let s = Sensed {
            free_gb: None,
            ..Sensed::default()
        };
        assert_eq!(r(None, false, s), None);
    }

    #[test]
    fn params_switch_conditions() {
        let mut values = params::defaults();
        values.insert("pause_on_battery".into(), ParamValue::Bool(false));
        values.insert("pause_on_low_battery".into(), ParamValue::Bool(false));
        values.insert("pause_on_fullscreen".into(), ParamValue::Bool(false));
        values.insert("pause_when_screen_on".into(), ParamValue::Bool(false));
        values.insert("pause_when_hot".into(), ParamValue::Bool(false));
        values.insert("min_free_disk_gb".into(), ParamValue::Int(10));
        let s = Sensed {
            fullscreen: true,
            battery: true,
            battery_percent: Some(10),
            screen_on: true,
            thermal: THERMAL_SEVERE,
            headroom: Some(1.2),
            free_gb: Some(20.0),
        };
        assert_eq!(reason(None, false, &s, &values), None);
        let s = Sensed {
            free_gb: Some(9.99),
            ..s
        };
        assert_eq!(
            reason(None, false, &s, &values),
            Some(PauseReason::DiskLow { free_gb: 9.9 })
        );
    }

    #[test]
    fn slow_by_headroom_or_status() {
        let mut values = params::defaults();
        assert!(!slow(&Sensed::default(), &values));
        let s = Sensed {
            headroom: Some(0.8),
            ..Sensed::default()
        };
        assert!(slow(&s, &values));
        values.insert("thermal_slow_headroom".into(), ParamValue::Int(90));
        assert!(!slow(&s, &values));
        let s = Sensed {
            thermal: THERMAL_MODERATE,
            ..Sensed::default()
        };
        assert!(slow(&s, &values));
        // 热状态 LIGHT 不降温
        let s = Sensed {
            thermal: 1,
            ..Sensed::default()
        };
        assert!(!slow(&s, &values));
        values.insert("slow_when_hot".into(), ParamValue::Bool(false));
        let s = Sensed {
            thermal: THERMAL_SEVERE,
            headroom: Some(1.2),
            ..Sensed::default()
        };
        assert!(!slow(&s, &values));
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
