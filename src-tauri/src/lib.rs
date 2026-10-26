//! 界面进程的应用外壳：数据目录准备、单实例、日志、窗口状态、外部链接，
//! 以及由代码创建主窗口（WebView2 数据目录随数据目录走、按系统主题设背景色避免白闪、拒绝新窗口）、关闭兜底和核心进程的连接与守护。
//! 关闭主窗口只销毁窗口，界面进程留在托盘（`tray`），从托盘再打开时重新创建；只有托盘的“退出”等入口会退出进程。
//! 用户在设置中选了关闭窗口时退出的，由前端在关闭时调用 `exit_app`（同托盘的“退出”），设置存在前端的本地存储
#[cfg(desktop)]
mod agent;
#[cfg(target_os = "android")]
mod android;
// 前台服务的启停判定，其他平台上只为测试编译
#[cfg(all(test, not(target_os = "android")))]
#[path = "android/background.rs"]
mod background;
// Android 更新包的校验与保存，其他平台上只为测试编译
#[cfg(all(test, not(target_os = "android")))]
#[path = "android/update.rs"]
mod android_update;
mod close_watch;
mod config;
mod core;
mod data;
#[cfg(not(target_os = "android"))]
mod data_dir;
mod diagnostics;
mod failures;
mod folder_filter;
mod image_query;
mod open_at;
mod pause;
mod releases;
mod search;
mod settings;
mod setup;
#[cfg(desktop)]
mod tray;
#[cfg(desktop)]
mod update;
mod update_store;
mod zip;

#[cfg(not(windows))]
#[path = "other.rs"]
mod platform;
#[cfg(windows)]
#[path = "windows.rs"]
mod platform;

use tauri::{AppHandle, Manager, Runtime};
#[cfg(not(target_os = "android"))]
use tauri_plugin_dialog::{DialogExt, FileDialogBuilder};
#[cfg(desktop)]
use tauri_plugin_window_state::{AppHandleExt, StateFlags};

/// 保存与恢复的窗口状态：边框由 tauri.conf.json 决定（不用系统边框，见 src/components/TitleBar.tsx），不跟随保存的旧值
#[cfg(desktop)]
pub(crate) const WINDOW_STATE: StateFlags = StateFlags::all().difference(StateFlags::DECORATIONS);

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 检查更新（releases、update）用的 rustls 需要进程默认的 ring 加密实现
    rummage_core::models::install_crypto();
    // 日志和 WebView2 都使用数据目录，需最先确定；有待迁移的更改时在此完成迁移，
    // 指向的目录无法使用时在创建窗口前询问用户。Android 上为应用私有目录，同时初始化 TLS
    #[cfg(not(target_os = "android"))]
    let startup = data_dir::prepare();
    #[cfg(target_os = "android")]
    let startup = android::prepare();
    // 同时启动的另一个实例正在迁移并显示进度，本实例直接退出
    if matches!(startup.dir, Err(rummage_datadir::Error::Busy)) {
        std::process::exit(0);
    }
    let data_dir = startup.dir.as_ref().ok().cloned();
    let data = data::DataDir::new(data_dir.clone(), startup.notices.clone());
    let builder = tauri::Builder::default();
    // single-instance 需最先注册，第二个实例启动时把已有主窗口带到前台后退出
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        tray::show_main(app)
    }));
    // 紧随 single-instance 注册：第二个实例在此之前已退出，不会与已有实例同时写日志文件
    let builder = builder.plugin(diagnostics::plugin(data_dir.as_deref()));
    #[cfg(desktop)]
    let builder = builder.plugin(
        tauri_plugin_window_state::Builder::default()
            .with_state_flags(WINDOW_STATE)
            .build(),
    );
    let builder = builder
        .plugin(tauri_plugin_opener::init())
        // 只供 Rust 侧选择以图搜图的图片、引导中的文件夹与模型文件、配置文件，不向前端开放插件命令
        .plugin(tauri_plugin_dialog::init());
    // 只供 Rust 侧的更新命令使用，不向前端开放插件命令；Android 的应用内更新见 android::update
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    // 存储权限与选择文件夹、文件（android::storage），同样只供 Rust 侧使用
    #[cfg(target_os = "android")]
    let builder = builder.plugin(tauri_plugin_rummage_android::init());
    builder
        .manage(core::Core::default())
        .manage(image_query::ImageTokens::default())
        .manage(config::PendingConfig::default())
        .manage(data)
        // 搜索结果的缩略图、预览图与应用内播放的音视频，只按索引中的 ID 提供
        .register_asynchronous_uri_scheme_protocol("media", |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            let path = request.uri().path().to_string();
            let range = request
                .headers()
                .get(tauri::http::header::RANGE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            tauri::async_runtime::spawn(async move {
                responder.respond(search::media(app, path, range).await);
            });
        })
        .invoke_handler(tauri::generate_handler![
            close_watch::close_ack,
            core::core_status,
            core::restart_core,
            search::search,
            search::index_status,
            pause::pause_indexing,
            pause::resume_indexing,
            failures::failed_files,
            failures::retry_failed,
            failures::retry_all_failed,
            failures::ignore_failed,
            failures::ignore_all_failed,
            search::segment_text,
            search::file_details,
            #[cfg(not(target_os = "android"))]
            search::open_file,
            #[cfg(target_os = "android")]
            android::ui::open_file,
            #[cfg(target_os = "android")]
            android::ui::share_file,
            #[cfg(target_os = "android")]
            android::ui::move_task_to_back,
            #[cfg(target_os = "android")]
            android::ui::safe_area,
            search::can_open_at,
            #[cfg(not(target_os = "android"))]
            search::reveal_file,
            #[cfg(target_os = "android")]
            android::ui::reveal_file,
            search::get_history,
            search::add_history,
            search::remove_history,
            search::clear_history,
            folder_filter::get_folder_filter,
            folder_filter::set_folder_filter,
            #[cfg(desktop)]
            folder_filter::pick_subfolder,
            #[cfg(target_os = "android")]
            android::storage::pick_subfolder,
            #[cfg(not(target_os = "android"))]
            image_query::pick_image,
            #[cfg(target_os = "android")]
            android::storage::pick_image,
            setup::setup_status,
            setup::finish_setup,
            setup::set_onboarding_step,
            setup::list_roots,
            #[cfg(desktop)]
            setup::add_root,
            #[cfg(target_os = "android")]
            android::storage::add_root,
            #[cfg(target_os = "android")]
            android::storage::add_common_root,
            #[cfg(target_os = "android")]
            android::storage::storage_access,
            #[cfg(target_os = "android")]
            android::storage::request_storage_access,
            #[cfg(target_os = "android")]
            android::background::battery_unrestricted,
            #[cfg(target_os = "android")]
            android::background::request_battery_unrestricted,
            #[cfg(target_os = "android")]
            android::background::open_app_settings,
            setup::remove_root,
            setup::get_params,
            setup::set_param,
            setup::download_models,
            #[cfg(not(target_os = "android"))]
            setup::import_models,
            #[cfg(target_os = "android")]
            android::storage::import_models,
            setup::cancel_model_task,
            setup::verify_model,
            setup::verify_models,
            setup::delete_model,
            setup::set_proxy,
            setup::run_selftest,
            setup::set_preferred_device,
            setup::unblock_gpu,
            setup::index_estimate,
            settings::param_defs,
            settings::set_root_excludes,
            settings::get_appearance,
            settings::set_appearance,
            settings::get_notices,
            settings::dismiss_notice,
            data::storage_info,
            data::open_data_dir,
            #[cfg(desktop)]
            data::data_dir_pick,
            data::data_dir_change,
            data::data_dir_notices,
            data::db_status,
            data::reset_index,
            data::clear_index,
            data::exit_app,
            diagnostics::open_log_dir,
            diagnostics::export_diagnostics,
            config::export_config,
            config::import_config,
            config::apply_config,
            releases::fetch_releases,
            #[cfg(desktop)]
            agent::agent_info,
            #[cfg(desktop)]
            agent::mcp_add,
            #[cfg(desktop)]
            agent::mcp_remove,
            #[cfg(desktop)]
            agent::skill_install,
            #[cfg(desktop)]
            agent::skill_remove,
            #[cfg(desktop)]
            agent::open_skill_dir,
            #[cfg(desktop)]
            update::update_ready,
            #[cfg(desktop)]
            update::update_download,
            #[cfg(desktop)]
            update::update_install,
            #[cfg(target_os = "android")]
            android::update::update_ready,
            #[cfg(target_os = "android")]
            android::update::update_download,
            #[cfg(target_os = "android")]
            android::update::update_install
        ])
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { .. } => close_watch::on_close_requested(window),
            // 窗口状态平时只在退出时写盘；界面进程可能在托盘中一直留到关机，关窗时就写下来
            #[cfg(desktop)]
            tauri::WindowEvent::Destroyed => {
                if let Err(err) = window.app_handle().save_window_state(WINDOW_STATE) {
                    log::warn!("无法保存窗口状态：{err}");
                }
            }
            // 拖入的图片路径留在 Rust 侧，前端只收到令牌（image_query）
            tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) => {
                image_query::on_drop(window, paths)
            }
            _ => {}
        })
        .setup(move |app| {
            // 准备数据目录时日志尚未初始化，在此补记；数据目录不可用时窗口照常打开
            if let Err(err) = &startup.dir {
                log::error!("无法确定数据目录：{err}");
            }
            // 迁移结果一律按警告级别记录，成功的结果也要能在 app.log 中查到
            for notice in &startup.notices {
                let paths: Vec<String> = notice
                    .paths
                    .iter()
                    .map(|(label, path)| format!("{label}：{path}"))
                    .collect();
                log::warn!(
                    "{}：{} {} {}",
                    notice.title,
                    notice.message,
                    paths.join(" "),
                    notice.details.join(" ")
                );
            }
            #[cfg(desktop)]
            {
                let updates = app
                    .state::<data::DataDir>()
                    .dir()
                    .ok()
                    .map(|dir| dir.join("updates"));
                let info = app.package_info().clone();
                tauri::async_runtime::spawn_blocking(move || {
                    update::cleanup(updates.as_deref(), &info.name, &info.version)
                });
            }
            #[cfg(target_os = "android")]
            if let Ok(dir) = app.state::<data::DataDir>().dir() {
                let updates = dir.join("updates");
                let version = app.package_info().version.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    android::update::cleanup(&updates, &version)
                });
            }
            // 托盘先于守护创建，连上核心后的第一次状态更新不会落空
            #[cfg(desktop)]
            tray::create(app.handle())?;
            // 在后台连接或拉起核心进程，不阻塞窗口创建
            core::supervise(app.handle());
            #[cfg(target_os = "android")]
            android::background::start(app.handle());
            create_main_window(app.handle())?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("启动应用失败")
        .run(|_app, event| {
            // 最后一个窗口关闭时（code 为空）不退出，留在托盘；`exit`、`request_restart` 带退出码，照常退出
            if let tauri::RunEvent::ExitRequested {
                code: None, api, ..
            } = event
            {
                api.prevent_exit();
            }
        })
}

/// 设好标题的文件对话框，主窗口在时挂到主窗口上；Android 上选择与保存文件走插件（`android::storage`）
#[cfg(not(target_os = "android"))]
pub(crate) fn file_dialog<R: Runtime>(app: &AppHandle<R>, title: &str) -> FileDialogBuilder<R> {
    let dialog = app.dialog().file().set_title(title);
    #[cfg(desktop)]
    if let Some(win) = app.get_webview_window("main") {
        return dialog.set_parent(&win);
    }
    dialog
}

/// 创建主窗口：启动时和从托盘重新打开时
pub(crate) fn create_main_window<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    // WebView2 数据放在数据目录的 webview\ 下，随数据目录走；不可用时用默认位置，不影响窗口创建
    let data = app.state::<data::DataDir>();
    let webview_dir = data
        .dir()
        .ok()
        .map(|dir| dir.join("webview"))
        .filter(|dir| match std::fs::create_dir_all(dir) {
            Ok(()) => true,
            Err(err) => {
                log::warn!("无法创建 WebView 数据目录，改用默认位置：{err}");
                false
            }
        });
    // WebView2 在页面加载出来之前是白色背景，深色模式下启动会白闪。页面只跟随系统主题，
    // 创建窗口时就按系统主题设为 body 的背景色（bg-gray-50 / dark:bg-gray-950）；窗口建好后再设置为时已晚。
    // 读取失败时按浅色处理
    #[cfg(windows)]
    let dark = windows_registry::CURRENT_USER
        .open(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|key| key.get_u32("AppsUseLightTheme"))
        .is_ok_and(|light| light == 0);
    #[cfg(not(windows))]
    let dark = false;
    let bg = if dark {
        tauri::window::Color(9, 9, 11, 255)
    } else {
        tauri::window::Color(250, 250, 250, 255)
    };
    // 主窗口在 tauri.conf.json 中设为 create: false，由代码创建以便拦截新窗口。
    // target="_blank" 和 window.open 一律拒绝，避免出现不受管理的 WebView2 弹窗
    let builder = tauri::WebviewWindowBuilder::from_config(app, &app.config().app.windows[0])?
        .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny)
        .background_color(bg);
    match webview_dir {
        Some(dir) => builder.data_directory(dir).build()?,
        None => builder.build()?,
    };
    Ok(())
}
