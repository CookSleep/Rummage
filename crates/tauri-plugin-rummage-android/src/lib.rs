//! Android 上的存储权限、文件选择与后台建立索引，Kotlin 部分在 `android/`（`StoragePlugin`、`IndexService`）。
//! - 只供 src-tauri 在 Rust 侧调用（`app.state::<Android<R>>()`），不向前端开放命令。
//! - 读取被索引的文件靠“所有文件访问”权限（`MANAGE_EXTERNAL_STORAGE`），选择的文件夹换算成真实路径交给核心，
//!   不持有文档提供者的 URI 授权；换算不了的位置（网盘等其他提供者）报告为不支持。
//! - 选择文件时能换算成可读的真实路径就直接用，否则复制到调用方给的目录（同名加序号），由调用方用完后删除。
//!   保存文件（导出配置、诊断包）先由调用方生成到应用私有目录，再用系统的“创建文件”选择位置，由 Kotlin 侧复制过去。
//! - 前台服务（`mediaProcessing`）与常驻通知由 Rust 侧按索引进度启停；应用回到前台、后台额度用完（`onTimeout`）、
//!   服务没能进入前台等情况由 `next_event` 报告（Kotlin 侧排队，Rust 侧循环等待）。
//! - 电池优化白名单（`ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`）与通知权限（Android 13 起的 `POST_NOTIFICATIONS`）。
//! - 应用内更新：“安装未知应用”权限（`canRequestPackageInstalls`、`ACTION_MANAGE_UNKNOWN_APP_SOURCES`）与用
//!   `PackageInstaller` 会话安装 APK（Kotlin 侧的 `ApkInstaller`）。
//! - 界面：用其他应用打开文件（`FileProvider` 的 content URI + `ACTION_VIEW`）、把应用退到后台（`moveTaskToBack`）、
//!   全面屏的安全区（按 `WindowInsets` 算出，变化时 Kotlin 侧直接写进页面的 CSS 变量，`safe_area` 取当前值）。
//!
//! 其他平台上为空。
#![cfg(target_os = "android")]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::plugin::mobile::PluginInvokeError;
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::{Manager, Runtime};

pub type Error = PluginInvokeError;

pub struct Android<R: Runtime>(PluginHandle<R>);

#[derive(Deserialize)]
struct Granted {
    granted: bool,
}

#[derive(Deserialize)]
struct Folder {
    path: Option<PathBuf>,
}

#[derive(Deserialize)]
struct Root {
    path: PathBuf,
}

#[derive(Deserialize)]
struct Files {
    paths: Vec<PathBuf>,
}

#[derive(Deserialize)]
struct Started {
    started: bool,
}

#[derive(Deserialize)]
struct Opened {
    opened: bool,
}

#[derive(Deserialize)]
struct Saved {
    name: String,
}

/// 安全区四边的宽度（CSS 像素）
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Insets {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

/// 安装会话的最终结果
#[derive(Debug, Deserialize)]
pub struct Installed {
    /// `success`、`aborted`（用户取消）或 `failure`
    pub status: String,
    /// 系统的结果码名称（如 `STATUS_FAILURE_CONFLICT`），写进日志
    pub code: String,
    /// 系统给出的原因
    pub message: Option<String>,
}

#[derive(Serialize)]
struct PathArgs<'a> {
    path: &'a Path,
}

#[derive(Deserialize)]
struct PluginEvent {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Serialize)]
struct Progress<'a> {
    done: u64,
    total: u64,
    note: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SaveFile<'a> {
    name: &'a str,
    mime_type: &'a str,
    path: &'a Path,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PickFiles<'a> {
    mime_types: &'a [&'a str],
    multiple: bool,
    copy_to: &'a Path,
}

impl<R: Runtime> Android<R> {
    /// 是否已有“所有文件访问”权限
    pub async fn all_files_access(&self) -> Result<bool, Error> {
        let res: Granted = self.0.run_mobile_plugin_async("allFilesAccess", ()).await?;
        Ok(res.granted)
    }

    /// 打开系统的授权页面，用户返回后报告是否已授权
    pub async fn request_all_files_access(&self) -> Result<bool, Error> {
        let res: Granted = self
            .0
            .run_mobile_plugin_async("requestAllFilesAccess", ())
            .await?;
        Ok(res.granted)
    }

    /// 选择文件夹：取消时为 `None`，选中了无法换算成真实路径的位置时为 `Some(None)`
    pub async fn pick_folder(&self) -> Result<Option<Option<PathBuf>>, Error> {
        let res: Option<Folder> = self.0.run_mobile_plugin_async("pickFolder", ()).await?;
        Ok(res.map(|f| f.path))
    }

    /// 选择文件，返回可直接读取的路径（真实路径或复制到 `copy_to` 中的副本）；取消时为 `None`
    pub async fn pick_files(
        &self,
        mime_types: &[&str],
        multiple: bool,
        copy_to: &Path,
    ) -> Result<Option<Vec<PathBuf>>, Error> {
        let args = PickFiles {
            mime_types,
            multiple,
            copy_to,
        };
        let res: Option<Files> = self.0.run_mobile_plugin_async("pickFiles", args).await?;
        Ok(res.map(|f| f.paths))
    }

    /// 用系统的“创建文件”（SAF）选择保存位置，把应用私有目录中的 `path` 复制过去，`name` 为建议的文件名；
    /// 返回所选文档的显示名，取消时为 `None`
    pub async fn save_file(
        &self,
        name: &str,
        mime_type: &str,
        path: &Path,
    ) -> Result<Option<String>, Error> {
        let args = SaveFile {
            name,
            mime_type,
            path,
        };
        let res: Option<Saved> = self.0.run_mobile_plugin_async("saveFile", args).await?;
        Ok(res.map(|s| s.name))
    }

    /// 主存储的根目录（`Environment.getExternalStorageDirectory()`）
    pub async fn storage_root(&self) -> Result<PathBuf, Error> {
        let res: Root = self.0.run_mobile_plugin_async("storageRoot", ()).await?;
        Ok(res.path)
    }

    /// 启动前台服务并显示进度通知（`note` 不为空时显示它而不是进度）；系统不允许从后台启动时返回否
    pub async fn start_service(
        &self,
        done: u64,
        total: u64,
        note: Option<&str>,
    ) -> Result<bool, Error> {
        let args = Progress { done, total, note };
        let res: Started = self
            .0
            .run_mobile_plugin_async("startIndexing", args)
            .await?;
        Ok(res.started)
    }

    /// 更新进度通知（`note` 同 `start_service`）；服务没有运行时不做任何事
    pub async fn update_service(
        &self,
        done: u64,
        total: u64,
        note: Option<&str>,
    ) -> Result<(), Error> {
        let args = Progress { done, total, note };
        self.0.run_mobile_plugin_async("updateIndexing", args).await
    }

    pub async fn stop_service(&self) -> Result<(), Error> {
        self.0.run_mobile_plugin_async("stopIndexing", ()).await
    }

    /// 等下一个事件：`resume`（应用回到前台）、`timeout`（后台额度已用完，服务已停止）、
    /// `stopped`（服务没能进入前台，已停止）
    pub async fn next_event(&self) -> Result<String, Error> {
        let res: PluginEvent = self.0.run_mobile_plugin_async("nextEvent", ()).await?;
        Ok(res.kind)
    }

    /// 是否已不受电池优化限制
    pub async fn battery_unrestricted(&self) -> Result<bool, Error> {
        let res: Granted = self
            .0
            .run_mobile_plugin_async("batteryUnrestricted", ())
            .await?;
        Ok(res.granted)
    }

    /// 请用户允许不受电池优化限制，返回后报告结果
    pub async fn request_battery_unrestricted(&self) -> Result<bool, Error> {
        let res: Granted = self
            .0
            .run_mobile_plugin_async("requestBatteryUnrestricted", ())
            .await?;
        Ok(res.granted)
    }

    /// 用其他应用打开文件；没有能打开它的应用时返回否
    pub async fn open_file(&self, path: &Path) -> Result<bool, Error> {
        let res: Opened = self
            .0
            .run_mobile_plugin_async("openFile", PathArgs { path })
            .await?;
        Ok(res.opened)
    }

    /// 在文件管理器中打开文件所在的文件夹；没有能打开它的应用时返回否
    pub async fn reveal_file(&self, path: &Path) -> Result<bool, Error> {
        let res: Opened = self
            .0
            .run_mobile_plugin_async("revealFile", PathArgs { path })
            .await?;
        Ok(res.opened)
    }

    /// 用系统分享面板分享文件
    pub async fn share_file(&self, path: &Path) -> Result<(), Error> {
        self.0
            .run_mobile_plugin_async("shareFile", PathArgs { path })
            .await
    }

    /// 是否已在“安装未知应用”中允许本应用
    pub async fn can_install_packages(&self) -> Result<bool, Error> {
        let res: Granted = self
            .0
            .run_mobile_plugin_async("canInstallPackages", ())
            .await?;
        Ok(res.granted)
    }

    /// 打开本应用的“安装未知应用”设置页，用户返回后报告是否已允许
    pub async fn request_install_packages(&self) -> Result<bool, Error> {
        let res: Granted = self
            .0
            .run_mobile_plugin_async("requestInstallPackages", ())
            .await?;
        Ok(res.granted)
    }

    /// 安装 APK：需要用户确认时打开系统的确认界面，等到最终结果；安装成功时本进程通常已被系统结束
    pub async fn install_apk(&self, path: &Path) -> Result<Installed, Error> {
        self.0
            .run_mobile_plugin_async("installApk", PathArgs { path })
            .await
    }

    /// 把应用退到后台，不结束 Activity
    pub async fn move_to_back(&self) -> Result<(), Error> {
        self.0.run_mobile_plugin_async("moveToBack", ()).await
    }

    /// 当前的安全区（状态栏、导航栏与挖孔）
    pub async fn safe_area(&self) -> Result<Insets, Error> {
        self.0.run_mobile_plugin_async("safeArea", ()).await
    }

    /// Android 13 起请求通知权限（已授予或更早的版本不做任何事），不等用户的选择
    pub async fn request_notifications(&self) -> Result<(), Error> {
        self.0
            .run_mobile_plugin_async("requestNotifications", ())
            .await
    }
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("rummage-android")
        .setup(|app, api| {
            let handle =
                api.register_android_plugin("com.cooksleep.rummage.storage", "StoragePlugin")?;
            app.manage(Android(handle));
            Ok(())
        })
        .build()
}
