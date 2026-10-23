//! Android 平台模块：后台服务在应用进程内运行，IPC、进程管理等沿用 `other.rs` 的占位实现；
//! 资源占用控制的检测（`sense`）在这里实现。
//! - Context 与 JavaVM 取自 `ndk-context`（tao 在 `Rust.onCreate` 中初始化），检测线程在第一次调用时永久附加到 JVM，
//!   每次查询都在 `attach_current_thread` 的局部引用帧中进行，局部引用不累积。
//! - 充电：`BatteryManager.isCharging()`（API 23），未在充电算使用电池；电量：系统粘性广播 `ACTION_BATTERY_CHANGED` 的
//!   `level / scale`（`registerReceiver(null, …)` 只取最近一次广播，不注册接收器），与状态栏显示的一致。
//!   不用 `getIntProperty(BATTERY_PROPERTY_CAPACITY)`：它直接读电池硬件，与 `isCharging` 的来源不同。
//! - 亮屏：`PowerManager.isInteractive()`（API 20）。
//! - 热状态：`PowerManager.getCurrentThermalStatus()`（API 29）；温度余量：`getThermalHeadroom(10)`（API 30），
//!   1.0 对应 `SEVERE`。按官方文档，设备不支持或调用明显快于每秒一次时返回 NaN，此时按空处理；检测每 5 秒一次，
//!   修改参数等操作额外触发的检测可能因此拿不到余量，不影响下一次。
//! - 剩余空间：`statvfs` 的 `f_bavail × f_frsize`（非特权进程可用的空间）。
//! - 上次退出是否为崩溃（`crashed`）：`ActivityManager.getHistoricalProcessExitReasons`（API 30）中本应用主进程
//!   最近一条记录在要查的这段时间内，且原因是 `REASON_CRASH` 或 `REASON_CRASH_NATIVE`。被系统冻结后结束、安装更新、
//!   用户划掉等都不算；手机重启等没有留下记录时，最近一条是更早的，不算。
//! - 任何一项查询失败都按“不暂停”处理，只记 debug 日志，与 Windows 相同。
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jni::objects::{JObject, JValue};
use jni::{Env, JavaVM, jni_sig, jni_str};

#[path = "other.rs"]
mod other;

pub use other::*;

use crate::resource::Sensed;

/// `ApplicationExitInfo.REASON_CRASH`（Java 异常）与 `REASON_CRASH_NATIVE`（原生代码崩溃）
const REASON_CRASH: i32 = 4;
const REASON_CRASH_NATIVE: i32 = 5;
/// 退出记录最多看这么多条：主进程与推理进程（`:embed`）的记录交错，最近一条主进程的记录在其中
const EXIT_RECORDS: i32 = 32;

/// 预测温度余量的时间（秒）：官方文档允许 0–60 秒，越远越不准；10 秒足够在升温前放慢，又不至于过早
const HEADROOM_FORECAST_S: i32 = 10;

/// 在已附加到 JVM 的当前线程上，用应用的 Context 调用 `f`
fn with_context<T>(
    f: impl FnOnce(&mut Env, &JObject) -> jni::errors::Result<T>,
) -> jni::errors::Result<T> {
    let ctx = ndk_context::android_context();
    // SAFETY: tao 在启动应用时用本进程的 JavaVM 初始化了 ndk-context，指针在进程内一直有效
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) };
    vm.attach_current_thread(|env| {
        // SAFETY: ndk-context 中的 Context 是 tao 持有的 Application 全局引用，进程内一直有效；这里只借用、不释放
        let obj = unsafe { JObject::from_raw(env, ctx.context().cast()) };
        f(env, &obj)
    })
}

/// `Context.getSystemService(name)`
fn service<'local>(
    env: &mut Env<'local>,
    ctx: &JObject,
    name: &str,
) -> jni::errors::Result<JObject<'local>> {
    let name = env.new_string(name)?;
    env.call_method(
        ctx,
        jni_str!("getSystemService"),
        jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
        &[JValue::Object(&name)],
    )?
    .l()
}

/// 查询失败时记 debug 日志并返回默认值
fn or_default<T: Default>(what: &str, res: jni::errors::Result<T>) -> T {
    res.unwrap_or_else(|err| {
        log::debug!("无法查询{what}：{err}");
        T::default()
    })
}

/// 系统最近一次广播的电量（%，见模块说明）；没有电池信息时为空
fn battery_level(env: &mut Env, ctx: &JObject) -> jni::errors::Result<Option<u8>> {
    let action = env.new_string("android.intent.action.BATTERY_CHANGED")?;
    let filter = env.new_object(
        jni_str!("android/content/IntentFilter"),
        jni_sig!("(Ljava/lang/String;)V"),
        &[JValue::Object(&action)],
    )?;
    let intent = env
        .call_method(
            ctx,
            jni_str!("registerReceiver"),
            jni_sig!(
                "(Landroid/content/BroadcastReceiver;Landroid/content/IntentFilter;)Landroid/content/Intent;"
            ),
            &[JValue::Object(&JObject::null()), JValue::Object(&filter)],
        )?
        .l()?;
    if env.is_same_object(&intent, JObject::null())? {
        return Ok(None);
    }
    let mut v = [0; 2];
    for (i, name) in ["level", "scale"].into_iter().enumerate() {
        let name = env.new_string(name)?;
        v[i] = env
            .call_method(
                &intent,
                jni_str!("getIntExtra"),
                jni_sig!("(Ljava/lang/String;I)I"),
                &[JValue::Object(&name), JValue::Int(-1)],
            )?
            .i()?;
    }
    let [level, scale] = v;
    if level < 0 || scale <= 0 {
        return Ok(None);
    }
    Ok(u8::try_from(level * 100 / scale).ok().filter(|p| *p <= 100))
}

/// 检测充电、亮屏、热状态与 `dir` 所在卷的剩余空间（见模块说明）
pub(crate) fn sense(dir: &Path) -> Sensed {
    let battery = with_context(|env, ctx| {
        let bm = service(env, ctx, "batterymanager")?;
        let charging = env
            .call_method(&bm, jni_str!("isCharging"), jni_sig!("()Z"), &[])?
            .z();
        Ok((charging, battery_level(env, ctx)))
    });
    let (battery, battery_percent) = match battery {
        Ok((c, p)) => (or_default("充电状态", c.map(|c| !c)), or_default("电量", p)),
        Err(err) => {
            log::debug!("无法查询电池状态：{err}");
            (false, None)
        }
    };
    let power = with_context(|env, ctx| {
        let pm = service(env, ctx, "power")?;
        let screen_on = env
            .call_method(&pm, jni_str!("isInteractive"), jni_sig!("()Z"), &[])?
            .z();
        let thermal = env
            .call_method(
                &pm,
                jni_str!("getCurrentThermalStatus"),
                jni_sig!("()I"),
                &[],
            )?
            .i();
        let headroom = env
            .call_method(
                &pm,
                jni_str!("getThermalHeadroom"),
                jni_sig!("(I)F"),
                &[JValue::Int(HEADROOM_FORECAST_S)],
            )?
            .f();
        Ok((screen_on, thermal, headroom))
    });
    let (screen_on, thermal, headroom) = match power {
        Ok((s, t, h)) => (
            or_default("屏幕状态", s),
            or_default("热状态", t),
            or_default("温度余量", h.map(Some)),
        ),
        Err(err) => {
            log::debug!("无法查询电源状态：{err}");
            (false, 0, None)
        }
    };
    Sensed {
        fullscreen: false,
        battery,
        battery_percent,
        screen_on,
        thermal,
        headroom: headroom.filter(|h| !h.is_nan()).map(f64::from),
        free_gb: free_gb(dir),
    }
}

/// `dir` 所在卷对非特权进程可用的空间（GB）；查询失败时为空
pub(crate) fn free_gb(dir: &Path) -> Option<f64> {
    let path = CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs 全为零是合法的初始值
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: path 是以 0 结尾的字符串；st 是输出参数，调用期间有效
    if unsafe { libc::statvfs(path.as_ptr(), &mut st) } != 0 {
        log::debug!(
            "无法查询 {} 的剩余空间：{}",
            dir.display(),
            std::io::Error::last_os_error()
        );
        return None;
    }
    Some(st.f_bavail as f64 * st.f_frsize as f64 / (1u64 << 30) as f64)
}

/// 本应用主进程最近 `window` 内的上一次退出是否为崩溃（见模块说明）；没有记录时为否
pub(crate) fn crashed(window: Duration) -> io::Result<bool> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let since = now.saturating_sub(window).as_millis() as i64;
    with_context(|env, ctx| {
        let am = service(env, ctx, "activity")?;
        let pkg = env
            .call_method(
                ctx,
                jni_str!("getPackageName"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .l()?;
        let list = env
            .call_method(
                &am,
                jni_str!("getHistoricalProcessExitReasons"),
                jni_sig!("(Ljava/lang/String;II)Ljava/util/List;"),
                &[
                    JValue::Object(&pkg),
                    JValue::Int(0),
                    JValue::Int(EXIT_RECORDS),
                ],
            )?
            .l()?;
        let n = env
            .call_method(&list, jni_str!("size"), jni_sig!("()I"), &[])?
            .i()?;
        // 按时间从新到旧
        for i in 0..n {
            let info = env
                .call_method(
                    &list,
                    jni_str!("get"),
                    jni_sig!("(I)Ljava/lang/Object;"),
                    &[JValue::Int(i)],
                )?
                .l()?;
            let name = env
                .call_method(
                    &info,
                    jni_str!("getProcessName"),
                    jni_sig!("()Ljava/lang/String;"),
                    &[],
                )?
                .l()?;
            let main = env
                .call_method(
                    &name,
                    jni_str!("equals"),
                    jni_sig!("(Ljava/lang/Object;)Z"),
                    &[JValue::Object(&pkg)],
                )?
                .z()?;
            if main {
                let at = env
                    .call_method(&info, jni_str!("getTimestamp"), jni_sig!("()J"), &[])?
                    .j()?;
                if at < since {
                    return Ok(false);
                }
                let reason = env
                    .call_method(&info, jni_str!("getReason"), jni_sig!("()I"), &[])?
                    .i()?;
                return Ok(reason == REASON_CRASH || reason == REASON_CRASH_NATIVE);
            }
        }
        Ok(false)
    })
    .map_err(io::Error::other)
}
