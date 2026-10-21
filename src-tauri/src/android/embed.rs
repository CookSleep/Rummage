//! GPU、NPU 推理所在的 `:embed` 进程（做法见 rummage-embed 的 Android 平台模块；Kotlin 部分见
//! crates/tauri-plugin-rummage-android 的 `EmbedService` 与 `EmbedProcess`）：
//! - 主进程：启动时登记启动方法（`register`）。启动时经 JNI 调 `EmbedProcess.start` 绑定服务，参数以换行连接成一个字符串
//!   （参数中只有路径与选项，没有换行）；结束时调 `EmbedProcess.stop` 解除绑定。插件的类不在系统类加载器中，用应用 Context
//!   的类加载器加载。
//! - `:embed` 进程：服务在自己的线程中调 `runWorker`（`Java_…_runWorker`），在那条连接上运行推理，返回退出码后服务结束整个进程。
use std::io;

use jni::objects::{JClass, JObject, JString, JValue};
use jni::{Env, jni_sig, jni_str};

use super::with_context;

/// `EmbedProcess` 的完整类名
const LAUNCHER_CLASS: &str = "com.cooksleep.rummage.storage.EmbedProcess";

/// 用应用 Context 的类加载器加载插件中的类
fn plugin_class<'local>(
    env: &mut Env<'local>,
    ctx: &JObject,
) -> jni::errors::Result<JClass<'local>> {
    let loader = env
        .call_method(
            ctx,
            jni_str!("getClassLoader"),
            jni_sig!("()Ljava/lang/ClassLoader;"),
            &[],
        )?
        .l()?;
    let name = env.new_string(LAUNCHER_CLASS)?;
    let class = env
        .call_method(
            &loader,
            jni_str!("loadClass"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/Class;"),
            &[JValue::Object(&name)],
        )?
        .l()?;
    env.cast_local::<JClass>(class)
}

fn start(socket: &str, args: &[String]) -> io::Result<()> {
    let ok = with_context(|env, ctx| {
        let class = plugin_class(env, ctx)?;
        let socket = env.new_string(socket)?;
        let args = env.new_string(args.join("\n"))?;
        env.call_static_method(
            &class,
            jni_str!("start"),
            jni_sig!("(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)Z"),
            &[
                JValue::Object(ctx),
                JValue::Object(&socket),
                JValue::Object(&args),
            ],
        )?
        .z()
    })
    .map_err(|err| io::Error::other(err.to_string()))?;
    if !ok {
        return Err(io::Error::other("系统拒绝启动推理进程"));
    }
    Ok(())
}

fn stop(socket: &str) {
    let res = with_context(|env, ctx| {
        let class = plugin_class(env, ctx)?;
        let socket = env.new_string(socket)?;
        env.call_static_method(
            &class,
            jni_str!("stop"),
            jni_sig!("(Landroid/content/Context;Ljava/lang/String;)V"),
            &[JValue::Object(ctx), JValue::Object(&socket)],
        )
        .map(|_| ())
    });
    if let Err(err) = res {
        log::warn!("无法解除推理进程的绑定：{err}");
    }
}

/// 登记 `:embed` 进程的启动方法；在启动后台服务之前调用
pub fn register() {
    rummage_embed::set_launcher(rummage_embed::Launcher { start, stop });
}

/// `:embed` 进程中 `EmbedService.runWorker(socket, args)` 的实现（见模块说明）
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_cooksleep_rummage_storage_EmbedService_runWorker<'local>(
    mut env: jni::EnvUnowned<'local>,
    _this: JObject<'local>,
    socket: JString<'local>,
    args: JString<'local>,
) -> jni::sys::jint {
    let strings = env
        .with_env(|env| -> jni::errors::Result<_> {
            Ok(Some((socket.try_to_string(env)?, args.try_to_string(env)?)))
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
    let Some((socket, args)) = strings else {
        return 1;
    };
    let args = args.split('\n').map(str::to_string).collect();
    rummage_embed::serve_embed_process(&socket, args)
}
