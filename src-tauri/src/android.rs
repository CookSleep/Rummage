//! Android 上的启动准备与进程内后台服务：
//! - 数据目录为应用私有目录 `Context.getFilesDir()`，不支持更改、不迁移；llama.cpp 运行库目录为
//!   `ApplicationInfo.nativeLibraryDir`（以旧方式打包：.so 压缩存放，安装时解压到 nativeLibraryDir，ggml 才能按目录加载 CPU 后端）。
//!   两者在 `prepare` 中经 JNI 取得：`run` 由 tao 在 `Rust.onCreate` 初始化 ndk-context 之后才调用。
//! - TLS：`prepare` 中初始化 rustls-platform-verifier（用系统的证书校验，需要应用的 Context；其 Kotlin 部分由 Gradle 引入），
//!   模型下载与检查更新都靠它。
//! - 黄金样本打包在 APK 的 `assets/golden` 中：`<数据目录>/golden` 不存在或与当前安装不一致（`PackageInfo.lastUpdateTime`，
//!   记在其中的 `.installed`）时整目录拷出，先拷到临时目录再换上。
//! - 后台服务（`engine`）：不拉起核心进程，第一次连接时在阻塞线程中做与核心进程相同的初始化（`rummage_core::start`），
//!   令牌只在内存中；之后断开重连都连到同一个 `State`。收到退出请求时放回处理中的作业后结束进程，同核心进程退出。
//! - 存储权限与选择文件夹、文件的命令在 `storage` 中，前台服务与电池优化白名单在 `background` 中，
//!   用其他应用打开文件、返回键与安全区在 `ui` 中，应用内更新在 `update` 中。
pub mod background;
pub mod storage;
pub mod ui;
pub mod update;

use std::ffi::{CString, c_void};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::{Arc, OnceLock};

use jni::objects::{JObject, JObjectArray, JString, JValue};
use jni::{Env, JavaVM, jni_sig, jni_str};
use ndk::asset::AssetManager;
use rummage_core::State;
use rummage_core::pipeline::Config;
use rummage_datadir::Startup;
use rummage_embed::space::{MMPROJ, MODEL, MODEL_DIR};
use tokio::sync::OnceCell;

#[link(name = "android")]
unsafe extern "C" {
    fn AAssetManager_fromJava(
        env: *mut jni::sys::JNIEnv,
        asset_manager: jni::sys::jobject,
    ) -> *mut c_void;
}

/// `ApplicationInfo.nativeLibraryDir`，`prepare` 取得
static NATIVE_LIBS: OnceLock<PathBuf> = OnceLock::new();
static ENGINE: OnceCell<Arc<State>> = OnceCell::const_new();

/// 在已附加到 JVM 的当前线程上，用应用的 Context 调用 `f`
fn with_context<T>(
    f: impl FnOnce(&mut Env, &JObject) -> jni::errors::Result<T>,
) -> jni::errors::Result<T> {
    let ctx = ndk_context::android_context();
    // SAFETY: tao 在调用 `run` 之前用本进程的 JavaVM 初始化了 ndk-context，指针在进程内一直有效
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) };
    vm.attach_current_thread(|env| {
        // SAFETY: ndk-context 中的 Context 是 tao 持有的 Application 全局引用，进程内一直有效；这里只借用、不释放
        let obj = unsafe { JObject::from_raw(env, ctx.context().cast()) };
        f(env, &obj)
    })
}

fn string(env: &mut Env, obj: JObject) -> jni::errors::Result<String> {
    env.cast_local::<JString>(obj)?.try_to_string(env)
}

/// 确定数据目录、取得运行库目录并初始化 TLS；在创建任何窗口之前调用
pub fn prepare() -> Startup {
    // 推理线程与后台服务向已关闭的管道写入时，默认的 SIGPIPE 会结束整个应用；忽略后写入返回 BrokenPipe 错误
    // SAFETY: 只把 SIGPIPE 的处理方式设为忽略，不注册任何处理函数；此时还没有其他线程依赖它的默认行为
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
    let res = with_context(|env, ctx| {
        let files = env
            .call_method(
                ctx,
                jni_str!("getFilesDir"),
                jni_sig!("()Ljava/io/File;"),
                &[],
            )?
            .l()?;
        let files = env
            .call_method(
                &files,
                jni_str!("getAbsolutePath"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .l()?;
        let files = string(env, files)?;
        let info = env
            .call_method(
                ctx,
                jni_str!("getApplicationInfo"),
                jni_sig!("()Landroid/content/pm/ApplicationInfo;"),
                &[],
            )?
            .l()?;
        let libs = env
            .get_field(
                &info,
                jni_str!("nativeLibraryDir"),
                jni_sig!("Ljava/lang/String;"),
            )?
            .l()?;
        let libs = string(env, libs)?;
        // SAFETY: 同 `with_context`，再借用一次交给 rustls-platform-verifier，它自己另建全局引用
        let owned = unsafe { JObject::from_raw(env, ctx.as_raw()) };
        if let Err(err) = rustls_platform_verifier::android::init_with_env(env, owned) {
            log::error!("无法初始化系统证书校验，模型下载与检查更新将无法连接：{err}");
        }
        Ok((PathBuf::from(files), PathBuf::from(libs)))
    });
    let dir = match res {
        Ok((files, libs)) => {
            let _ = NATIVE_LIBS.set(libs);
            Ok(files)
        }
        Err(err) => Err(rummage_datadir::Error::Base(io::Error::other(format!(
            "无法取得应用的数据目录：{err}"
        )))),
    };
    Startup {
        dir,
        notices: Vec::new(),
    }
}

/// 当前安装的标记（`PackageInfo.lastUpdateTime`），覆盖安装后变化
fn installed(env: &mut Env, ctx: &JObject) -> jni::errors::Result<String> {
    let pm = env
        .call_method(
            ctx,
            jni_str!("getPackageManager"),
            jni_sig!("()Landroid/content/pm/PackageManager;"),
            &[],
        )?
        .l()?;
    let name = env
        .call_method(
            ctx,
            jni_str!("getPackageName"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )?
        .l()?;
    let info = env
        .call_method(
            &pm,
            jni_str!("getPackageInfo"),
            jni_sig!("(Ljava/lang/String;I)Landroid/content/pm/PackageInfo;"),
            &[JValue::Object(&name), JValue::Int(0)],
        )?
        .l()?;
    let time = env
        .get_field(&info, jni_str!("lastUpdateTime"), jni_sig!("J"))?
        .j()?;
    Ok(time.to_string())
}

/// `assets/<dir>` 下的条目名（文件与子目录）；在单独的局部引用帧中调用，递归时局部引用不累积
fn list(env: &mut Env, am: &JObject, dir: &str) -> jni::errors::Result<Vec<String>> {
    env.with_local_frame(16, |env| {
        let path = env.new_string(dir)?;
        let names = env
            .call_method(
                am,
                jni_str!("list"),
                jni_sig!("(Ljava/lang/String;)[Ljava/lang/String;"),
                &[JValue::Object(&path)],
            )?
            .l()?;
        let names = env.cast_local::<JObjectArray<JString>>(names)?;
        let mut out = Vec::new();
        for i in 0..names.len(env)? {
            out.push(names.get_element(env, i)?.try_to_string(env)?);
        }
        Ok(out)
    })
}

/// 把 `assets/<from>` 整个目录拷到 `to`；打不开的条目是子目录，递归拷贝。每个文件落盘后才返回
fn copy_assets(
    env: &mut Env,
    am: &JObject,
    native: &AssetManager,
    from: &str,
    to: &Path,
) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|err| format!("无法创建 {}：{err}", to.display()))?;
    let names = list(env, am, from).map_err(|err| format!("无法列出 assets/{from}：{err}"))?;
    // 既打不开又列不出内容：不是 APK 中的目录（黄金样本中没有空目录）
    if names.is_empty() {
        return Err(format!("无法打开 assets/{from}"));
    }
    for name in names {
        let src = format!("{from}/{name}");
        let dst = to.join(&name);
        let c = CString::new(src.as_str()).map_err(|err| err.to_string())?;
        let Some(mut asset) = native.open(&c) else {
            copy_assets(env, am, native, &src, &dst)?;
            continue;
        };
        let mut file =
            fs::File::create(&dst).map_err(|err| format!("无法创建 {}：{err}", dst.display()))?;
        io::copy(&mut asset, &mut file)
            .and_then(|_| file.sync_all())
            .map_err(|err| format!("无法写入 {}：{err}", dst.display()))?;
    }
    Ok(())
}

/// `<数据目录>/golden` 与当前安装不一致时从 APK 拷出（见模块说明）
fn extract_golden(dir: &Path) -> Result<(), String> {
    let marker = dir.join(".installed");
    with_context(|env, ctx| {
        let stamp = installed(env, ctx)?;
        if fs::read_to_string(&marker).is_ok_and(|s| s == stamp) {
            return Ok(Ok(()));
        }
        let am = env
            .call_method(
                ctx,
                jni_str!("getAssets"),
                jni_sig!("()Landroid/content/res/AssetManager;"),
                &[],
            )?
            .l()?;
        // SAFETY: env 是当前线程有效的 JNIEnv，am 是有效的 AssetManager 局部引用；返回的指针随 Java 对象存活，
        // 应用的 AssetManager 在进程内一直存在
        let ptr = unsafe { AAssetManager_fromJava(env.get_raw(), am.as_raw()) };
        let Some(ptr) = NonNull::new(ptr.cast()) else {
            return Ok(Err("无法取得 AssetManager。".to_string()));
        };
        // SAFETY: ptr 是上面取得的非空 AAssetManager
        let native = unsafe { AssetManager::from_ptr(ptr) };
        let tmp = dir.with_extension("tmp");
        let _ = fs::remove_dir_all(&tmp);
        let res = copy_assets(env, &am, &native, "golden", &tmp)
            .and_then(|()| {
                let mut f =
                    fs::File::create(tmp.join(".installed")).map_err(|err| err.to_string())?;
                f.write_all(stamp.as_bytes())
                    .and_then(|()| f.sync_all())
                    .map_err(|err| err.to_string())
            })
            .and_then(|()| match fs::remove_dir_all(dir) {
                Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err.to_string()),
                _ => fs::rename(&tmp, dir).map_err(|err| err.to_string()),
            });
        Ok(res)
    })
    .map_err(|err| err.to_string())?
}

/// 与核心进程相同的初始化（见模块说明）；会读写数据库、拷文件，在阻塞线程中调用
fn start(dir: &Path) -> Result<Arc<State>, String> {
    let libs = NATIVE_LIBS.get().ok_or("没有取得运行库目录。")?.clone();
    storage::clear_copies(dir);
    let golden = dir.join("golden");
    if let Err(err) = extract_golden(&golden) {
        log::error!("无法拷出黄金样本，设备检测将无法进行：{err}");
    }
    let models = dir.join("models").join(MODEL_DIR);
    let cfg = Config {
        exe: std::env::current_exe().unwrap_or_default(),
        // 没有 Vulkan 运行库，只用 CPU；ffmpeg 不随附，音视频按其他平台的方式处理
        vulkan_runtime: libs.join("vulkan"),
        ffmpeg: libs.join("ffmpeg"),
        cpu_runtime: libs,
        model: models.join(MODEL.name),
        mmproj: models.join(MMPROJ.name),
        selftest_cache: dir.join("cache").join("selftest.json"),
        golden,
        embed_tmp: Config::embed_tmp_dir(dir),
        gpus: rummage_embed::selftest::gpu_adapters,
    };
    let token = rummage_core::new_token().map_err(|err| format!("无法生成连接令牌：{err}"))?;
    Ok(rummage_core::start(token, dir, cfg))
}

/// 本进程中的后台服务，第一次调用时启动（见模块说明）
pub async fn engine(dir: &Path) -> Result<Arc<State>, String> {
    ENGINE
        .get_or_try_init(|| async {
            let dir = dir.to_path_buf();
            let state = tauri::async_runtime::spawn_blocking(move || start(&dir))
                .await
                .map_err(|err| err.to_string())??;
            let s = state.clone();
            tauri::async_runtime::spawn(async move {
                s.shutdown_requested().await;
                if let Err(err) =
                    tauri::async_runtime::spawn_blocking(move || s.stop_pipeline()).await
                {
                    log::error!("停止索引时出错：{err}");
                }
                std::process::exit(0);
            });
            Ok(state)
        })
        .await
        .cloned()
}
