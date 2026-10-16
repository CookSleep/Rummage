//! 设置页用到的消息内容：外观（字体）与需要在设置中提示的通知。
//! 请求、回复的枚举项在 `lib.rs` 中，这里只放它们携带的类型。前端的对应类型在 `src/types.ts`。
use serde::{Deserialize, Serialize};

/// 字体来源：内置字体、只用系统字体回退列表，或用户填写的字体名
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FontMode {
    #[default]
    Builtin,
    System,
    Custom,
}

/// 外观设置，存在核心的 `meta` 表，与其他设置统一。
/// 字体名只在 `Custom` 时使用，切换到其他来源时仍保留，切回时不必重填
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Appearance {
    pub ui_font: FontMode,
    pub ui_font_name: String,
    pub mono_font: FontMode,
    pub mono_font_name: String,
}

/// 需要在设置页顶部提示、用户确认后清除的通知
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Notice {
    /// 上次更新索引数据库时出错，已从迁移前的备份恢复
    DbRestored,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode, encode};

    #[test]
    fn settings_round_trip() {
        let a = Appearance {
            ui_font: FontMode::Custom,
            ui_font_name: "霞鹜文楷 \"Screen\"".into(),
            mono_font: FontMode::System,
            mono_font_name: String::new(),
        };
        assert_eq!(decode::<Appearance>(&encode(&a)).unwrap(), a);
        assert_eq!(
            encode(&Appearance::default()),
            r#"{"uiFont":"builtin","uiFontName":"","monoFont":"builtin","monoFontName":""}"#
        );
        assert_eq!(encode(&Notice::DbRestored), r#""db_restored""#);
        assert_eq!(
            decode::<Notice>(r#""db_restored""#).unwrap(),
            Notice::DbRestored
        );
    }
}
