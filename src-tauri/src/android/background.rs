//! Android 上的后台建立索引：前台服务的启停、进度通知、后台额度与电池优化白名单
//! （Kotlin 部分见 crates/tauri-plugin-rummage-android 的 `IndexService`）。
//! - 启停：按后台服务的 `IndexProgress` 事件决定（`wanted`）。有作业要处理且没有暂停时运行；因未接电源、亮屏、过热暂停时
//!   也保持运行，通知改为显示暂停原因（`note`，文字与界面的索引提示一致）：这几种暂停会在应用留在后台时自行解除
//!   （如夜里接上电源、息屏），而 Android 12 起应用在后台不能再启动前台服务。其余情况停止服务。
//! - 系统拒绝从后台启动（`ForegroundServiceStartNotAllowedException`）或服务没能进入前台时记录警告，不重试，
//!   等应用回到前台（Activity 的 resume，插件转发为 `resume` 事件）再启动。
//! - 后台额度（Android 15 起 `mediaProcessing` 每 24 小时共 6 小时，应用回到前台时重新计时）用完时系统回调 `onTimeout`：
//!   Kotlin 侧先报告 `timeout` 再停止服务，这里向后台服务发送 `BackgroundQuota { exhausted: true }` 暂停领取作业；
//!   应用回到前台时发送 `exhausted: false` 解除。
//! - 通知中的数字与界面的索引提示一致（`shown`）：有待处理的文件时为本轮的进度（`IndexStatus::round`，由后台服务统一计算），
//!   只剩按新设置重建时为重建的进度。
//! - 事件与进度在同一个任务中按到达顺序处理，插件调用不会交错。
//! - 后台运行：电池优化白名单（`request_battery_unrestricted`）；部分系统另有自己的后台管控，前台服务、唤醒锁与白名单都挡不住，
//!   退到后台几秒后冻结整个应用，只能由用户在应用信息的电量或电池设置中放行（`open_app_settings`）。
//! - 通知权限（Android 13 起）在完成引导时请求一次（`request_notifications`），被拒绝时服务照常运行，不再追问。
use rummage_proto::{IndexState, IndexStatus, PauseReason};

#[cfg(target_os = "android")]
use {
    crate::core,
    rummage_proto::{ApiError, Event, Request},
    tauri::{AppHandle, Listener, Manager, Runtime},
    tauri_plugin_rummage_android::Android,
    tokio::sync::mpsc,
};

/// 暂停期间保持服务运行的原因在通知中的文字，与界面的索引提示（`src/components/IndexHint.tsx`）一致；其他原因为空
pub fn note(p: PauseReason) -> Option<&'static str> {
    match p {
        PauseReason::Battery => Some("未接电源，已暂停"),
        PauseReason::ScreenOn => Some("屏幕亮着，已暂停"),
        PauseReason::Hot => Some("手机温度过高，已暂停"),
        _ => None,
    }
}

/// 通知中的已完成数与总数（见模块说明）
pub fn shown(s: &IndexStatus) -> (u64, u64) {
    if let Some(r) = s.round {
        return (r.done, r.total);
    }
    if let Some(rb) = &s.rebuild {
        return (rb.done, rb.total);
    }
    (s.done, s.total)
}

/// 前台服务是否应当运行（见模块说明）
pub fn wanted(s: &IndexStatus) -> bool {
    match s.paused {
        None => s.state == IndexState::Running,
        Some(p) if note(p).is_some() => {
            s.pending > 0 && matches!(s.state, IndexState::Running | IndexState::Idle)
        }
        Some(_) => false,
    }
}

#[cfg(target_os = "android")]
enum Msg {
    Status(IndexStatus),
    Plugin(String),
}

/// 服务的状态
#[cfg(target_os = "android")]
#[derive(Default)]
struct Service {
    running: bool,
    /// 系统拒绝了启动，应用回到前台之前不再尝试
    blocked: bool,
    last: Option<IndexStatus>,
}

#[cfg(target_os = "android")]
async fn set_quota<R: Runtime>(app: &AppHandle<R>, exhausted: bool) {
    match core::done(app, Request::BackgroundQuota { exhausted }).await {
        // 刚启动时后台服务还没连上，它本来就没有暂停，不必告知
        Err(err) if !exhausted && err.code == ApiError::UNAVAILABLE => {}
        Err(err) => log::warn!("无法告知后台服务后台额度的状态：{}", err.message),
        Ok(()) => {}
    }
}

#[cfg(target_os = "android")]
async fn sync<R: Runtime>(plugin: &Android<R>, svc: &mut Service) {
    let Some(s) = &svc.last else {
        return;
    };
    let want = wanted(s);
    let text = s.paused.and_then(note);
    let (done, total) = shown(s);
    if want && !svc.running {
        if svc.blocked {
            return;
        }
        match plugin.start_service(done, total, text).await {
            Ok(true) => svc.running = true,
            Ok(false) => {
                log::warn!("系统不允许在后台启动建立索引的前台服务，应用回到前台后再启动");
                svc.blocked = true;
            }
            Err(err) => {
                log::warn!("无法启动建立索引的前台服务：{err}");
                svc.blocked = true;
            }
        }
        return;
    }
    if !want && svc.running {
        if let Err(err) = plugin.stop_service().await {
            log::warn!("无法停止建立索引的前台服务：{err}");
        }
        svc.running = false;
        return;
    }
    if svc.running
        && let Err(err) = plugin.update_service(done, total, text).await
    {
        log::warn!("无法更新建立索引的通知：{err}");
    }
}

/// 开始跟随索引进度启停前台服务；在 setup 中调用一次
#[cfg(target_os = "android")]
pub fn start<R: Runtime>(app: &AppHandle<R>) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let progress = tx.clone();
    app.listen_any("core-event", move |ev| {
        if let Ok(Event::IndexProgress(s)) = serde_json::from_str::<Event>(ev.payload()) {
            let _ = progress.send(Msg::Status(s));
        }
    });
    let h = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match h.state::<Android<R>>().next_event().await {
                Ok(kind) => {
                    if tx.send(Msg::Plugin(kind)).is_err() {
                        return;
                    }
                }
                Err(err) => {
                    log::error!("无法接收前台服务的事件，后台建立索引将不受控制：{err}");
                    return;
                }
            }
        }
    });
    let h = app.clone();
    tauri::async_runtime::spawn(async move {
        let plugin = h.state::<Android<R>>();
        let mut svc = Service::default();
        while let Some(msg) = rx.recv().await {
            match msg {
                Msg::Status(s) => svc.last = Some(s),
                Msg::Plugin(kind) => match kind.as_str() {
                    "resume" => {
                        svc.blocked = false;
                        set_quota(&h, false).await;
                    }
                    "timeout" => {
                        log::warn!("今日后台运行额度已用完，前台服务已停止");
                        svc.running = false;
                        svc.blocked = true;
                        set_quota(&h, true).await;
                    }
                    "stopped" => {
                        log::warn!("建立索引的前台服务没能进入前台，应用回到前台后再启动");
                        svc.running = false;
                        svc.blocked = true;
                    }
                    other => log::warn!("未知的前台服务事件：{other}"),
                },
            }
            sync(&plugin, &mut svc).await;
        }
    });
}

/// 完成引导、开始建立索引时请求通知权限（见模块说明）
#[cfg(target_os = "android")]
pub async fn request_notifications<R: Runtime>(app: &AppHandle<R>) {
    if let Err(err) = app.state::<Android<R>>().request_notifications().await {
        log::warn!("无法请求通知权限：{err}");
    }
}

#[cfg(target_os = "android")]
#[tauri::command]
pub async fn battery_unrestricted<R: Runtime>(app: AppHandle<R>) -> Result<bool, ApiError> {
    app.state::<Android<R>>()
        .battery_unrestricted()
        .await
        .map_err(|err| {
            log::warn!("无法读取电池优化设置：{err}");
            crate::search::error(ApiError::INTERNAL, "无法读取电池优化设置，请重试。")
        })
}

/// 请用户允许不受电池优化限制，返回后报告是否已允许
#[cfg(target_os = "android")]
#[tauri::command]
pub async fn request_battery_unrestricted<R: Runtime>(app: AppHandle<R>) -> Result<bool, ApiError> {
    app.state::<Android<R>>()
        .request_battery_unrestricted()
        .await
        .map_err(|err| {
            log::warn!("无法打开电池优化设置：{err}");
            crate::search::error(
                ApiError::INTERNAL,
                "无法打开电池优化设置，请在系统设置中允许 Rummage 不受电池优化限制。",
            )
        })
}

/// 打开 Rummage 的系统应用信息页，用户在其中允许后台运行（部分系统在电池优化之外另有后台管控）
#[cfg(target_os = "android")]
#[tauri::command]
pub async fn open_app_settings<R: Runtime>(app: AppHandle<R>) -> Result<(), ApiError> {
    app.state::<Android<R>>()
        .open_app_settings()
        .await
        .map_err(|err| {
            log::warn!("无法打开应用信息：{err}");
            crate::search::error(
                ApiError::INTERNAL,
                "无法打开应用信息，请在系统设置的应用列表中找到 Rummage。",
            )
        })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn status(state: IndexState, paused: Option<PauseReason>, pending: u64) -> IndexStatus {
        IndexStatus {
            state,
            paused,
            slow: false,
            total: 10,
            done: 10 - pending,
            failed: 0,
            pending,
            by_kind: BTreeMap::new(),
            backend: None,
            error: None,
            eta_s: None,
            rebuild: None,
            round: None,
        }
    }

    #[test]
    fn runs_while_indexing() {
        assert!(wanted(&status(IndexState::Running, None, 3)));
        // 空闲（含只剩离线等领取不了的作业）、没有模型、出错时停止
        assert!(!wanted(&status(IndexState::Idle, None, 0)));
        assert!(!wanted(&status(IndexState::Idle, None, 3)));
        assert!(!wanted(&status(IndexState::NoModel, None, 3)));
        assert!(!wanted(&status(IndexState::Error, None, 3)));
        assert!(!wanted(&status(IndexState::NoMmproj, None, 3)));
    }

    #[test]
    fn keeps_running_through_transient_pauses() {
        for p in [
            PauseReason::Battery,
            PauseReason::ScreenOn,
            PauseReason::Hot,
        ] {
            assert!(note(p).is_some(), "{p:?}");
            assert!(wanted(&status(IndexState::Idle, Some(p), 3)), "{p:?}");
            assert!(wanted(&status(IndexState::Running, Some(p), 3)), "{p:?}");
            assert!(!wanted(&status(IndexState::Idle, Some(p), 0)), "{p:?}");
            assert!(!wanted(&status(IndexState::NoModel, Some(p), 3)), "{p:?}");
        }
        for p in [
            PauseReason::Manual { until: None },
            PauseReason::Sleeping,
            PauseReason::DiskLow { free_gb: 1.0 },
            PauseReason::Quota,
            PauseReason::Fullscreen,
        ] {
            assert!(note(p).is_none(), "{p:?}");
            assert!(!wanted(&status(IndexState::Running, Some(p), 3)), "{p:?}");
        }
    }

    #[test]
    fn notification_counts_match_index_hint() {
        // 有待处理的文件：显示本轮的进度
        let indexing = IndexStatus {
            round: Some(rummage_proto::RoundStatus { done: 2, total: 5 }),
            ..status(IndexState::Running, None, 3)
        };
        assert_eq!(shown(&indexing), (2, 5));
        // 只剩按新设置重建：显示重建的进度
        let rebuilding = IndexStatus {
            rebuild: Some(rummage_proto::RebuildStatus {
                done: 4,
                total: 20,
                by_kind: BTreeMap::new(),
            }),
            ..status(IndexState::Running, None, 0)
        };
        assert_eq!(shown(&rebuilding), (4, 20));
    }
}
