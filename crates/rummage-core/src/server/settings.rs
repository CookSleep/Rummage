//! 设置页在核心状态上的部分：
//! - 根目录的用户排除规则：保存后重新扫描该根目录，新排除的文件在对账时删除，
//!   不再排除的文件重新加入。
//! - 外观（字体）存在 `meta` 表的 `appearance`（JSON），与其他设置统一放在核心；没有或无法解析时为默认值。
//!   界面另在本地缓存一份，只为首次绘制前就用上字体，以核心为准。
//! - 通知：迁移失败后从备份恢复数据库时写入的 `notice.db_restored`，设置页提示一次，用户确认后删除。
use std::sync::Arc;

use rummage_index::NOTICE_DB_RESTORED;
use rummage_proto::{ApiError, Appearance, Event, Notice, Reply, Request};

use super::{ScanJob, State, api_error, db, root_error, unavailable};

const APPEARANCE_KEY: &str = "appearance";
/// 自定义字体名的长度上限（字符），只为挡住误粘贴的长文本
const MAX_FONT_NAME: usize = 100;

/// 设置页的请求（`Request::SetRootExcludes` 等，见 `rummage_proto::Request`）
pub(super) fn respond(state: &Arc<State>, body: Request) -> Result<Reply, ApiError> {
    match body {
        Request::SetRootExcludes { id, excludes } => {
            db(state)?
                .set_root_excludes(id, &excludes)
                .map_err(root_error)?;
            state.queue_scan(ScanJob::Root(id));
            let _ = state.events.send(Event::RootsChanged);
            Ok(Reply::Done)
        }
        Request::GetAppearance => {
            let raw = db(state)?.meta_get(APPEARANCE_KEY).map_err(unavailable)?;
            let appearance = raw
                .and_then(|raw| {
                    serde_json::from_str(&raw)
                        .inspect_err(|err| log::warn!("忽略格式不正确的外观设置：{err}"))
                        .ok()
                })
                .unwrap_or_default();
            Ok(Reply::Appearance(appearance))
        }
        Request::SetAppearance { appearance } => {
            let a = Appearance {
                ui_font_name: appearance.ui_font_name.trim().to_string(),
                mono_font_name: appearance.mono_font_name.trim().to_string(),
                ..appearance
            };
            if a.ui_font_name.chars().count() > MAX_FONT_NAME
                || a.mono_font_name.chars().count() > MAX_FONT_NAME
            {
                return Err(api_error(
                    ApiError::BAD_REQUEST,
                    format!("字体名称不能超过 {MAX_FONT_NAME} 个字。"),
                ));
            }
            let json = serde_json::to_string(&a).expect("外观设置只含字符串与枚举，序列化不会失败");
            db(state)?
                .meta_set(APPEARANCE_KEY, &json)
                .map_err(unavailable)?;
            Ok(Reply::Done)
        }
        Request::GetNotices => {
            let restored = db(state)?
                .meta_get(NOTICE_DB_RESTORED)
                .map_err(unavailable)?;
            Ok(Reply::Notices {
                notices: restored.map(|_| Notice::DbRestored).into_iter().collect(),
            })
        }
        Request::DismissNotice { notice } => {
            let key = match notice {
                Notice::DbRestored => NOTICE_DB_RESTORED,
            };
            db(state)?.meta_delete(key).map_err(unavailable)?;
            Ok(Reply::Done)
        }
        other => unreachable!("server::respond 只把设置相关的请求交给这里：{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use rummage_index::Db;
    use rummage_proto::FontMode;

    use super::*;

    fn open(dir: &std::path::Path) -> Arc<State> {
        State::new("t".into(), Db::open(dir))
    }

    #[test]
    fn appearance_persists() {
        let dir = crate::test_dir();
        let state = open(dir.path());
        assert_eq!(
            respond(&state, Request::GetAppearance),
            Ok(Reply::Appearance(Appearance::default()))
        );
        let body = Request::SetAppearance {
            appearance: Appearance {
                ui_font: FontMode::Custom,
                ui_font_name: "  霞鹜文楷 ".into(),
                mono_font: FontMode::System,
                mono_font_name: String::new(),
            },
        };
        assert_eq!(respond(&state, body), Ok(Reply::Done));
        drop(state);
        let state = open(dir.path());
        let Ok(Reply::Appearance(a)) = respond(&state, Request::GetAppearance) else {
            panic!("应返回外观设置");
        };
        assert_eq!(a.ui_font, FontMode::Custom);
        assert_eq!(a.ui_font_name, "霞鹜文楷");
        assert_eq!(a.mono_font, FontMode::System);

        let body = Request::SetAppearance {
            appearance: Appearance {
                ui_font_name: "长".repeat(MAX_FONT_NAME + 1),
                ..Appearance::default()
            },
        };
        assert_eq!(
            respond(&state, body).unwrap_err().code,
            ApiError::BAD_REQUEST
        );
        // 库中的值损坏时用默认值
        db(&state)
            .unwrap()
            .meta_set(APPEARANCE_KEY, "oops")
            .unwrap();
        assert_eq!(
            respond(&state, Request::GetAppearance),
            Ok(Reply::Appearance(Appearance::default()))
        );
    }

    #[test]
    fn notices_dismissed() {
        let dir = crate::test_dir();
        let state = open(dir.path());
        let none = Ok(Reply::Notices {
            notices: Vec::new(),
        });
        assert_eq!(respond(&state, Request::GetNotices), none);
        db(&state)
            .unwrap()
            .meta_set(NOTICE_DB_RESTORED, "1")
            .unwrap();
        assert_eq!(
            respond(&state, Request::GetNotices),
            Ok(Reply::Notices {
                notices: vec![Notice::DbRestored]
            })
        );
        let body = Request::DismissNotice {
            notice: Notice::DbRestored,
        };
        assert_eq!(respond(&state, body), Ok(Reply::Done));
        assert_eq!(respond(&state, Request::GetNotices), none);
    }

    #[test]
    fn excludes_validated_and_saved() {
        let dir = crate::test_dir();
        let docs = crate::test_dir();
        let id = {
            let db = Db::open(dir.path()).unwrap();
            db.add_root(&rummage_index::normalize_root(docs.path()).unwrap())
                .unwrap()
        };
        let state = open(dir.path());
        let body = Request::SetRootExcludes {
            id,
            excludes: vec!["*.tmp".into(), "{".into()],
        };
        let err = respond(&state, body).unwrap_err();
        assert_eq!(err.code, ApiError::BAD_REQUEST);
        assert!(err.message.contains("\"{\""), "{}", err.message);
        let body = Request::SetRootExcludes {
            id,
            excludes: vec!["node_modules".into()],
        };
        assert_eq!(respond(&state, body), Ok(Reply::Done));
        let roots = db(&state).unwrap().list_roots().unwrap();
        assert_eq!(roots[0].excludes, ["node_modules"]);
        let body = Request::SetRootExcludes {
            id: id + 1,
            excludes: Vec::new(),
        };
        assert_eq!(
            respond(&state, body).unwrap_err().code,
            ApiError::BAD_REQUEST
        );
    }
}
