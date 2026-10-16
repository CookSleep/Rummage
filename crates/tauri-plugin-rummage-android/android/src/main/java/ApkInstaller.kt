package com.cooksleep.rummage.storage

import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageInstaller
import androidx.core.content.ContextCompat
import androidx.core.content.IntentCompat
import app.tauri.Logger
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import java.io.File

/**
 * 用 `PackageInstaller` 会话安装下载好的更新 APK（应用内更新，Rust 侧见 src-tauri/src/android/update.rs）。
 * - 不用 `ACTION_VIEW` + FileProvider：`ACTION_INSTALL_PACKAGE` 已在 API 29 弃用，官方建议改用 `PackageInstaller`；
 *   会话方式能拿到安装结果（成功、用户取消、失败原因），不必向安装器授予文件的读取权限。两种方式都需要
 *   `REQUEST_INSTALL_PACKAGES` 权限，且用户在“安装未知应用”中允许了本应用（`canRequestPackageInstalls`）。
 * - 流程：建会话 → `openWrite` 写入 APK 并 `fsync` → `commit` 时带上接收结果的 `PendingIntent`。
 *   系统需要用户确认时先送来 `STATUS_PENDING_USER_ACTION`，`Intent.EXTRA_INTENT` 是系统的确认界面，由当前 Activity 打开；
 *   用户确认后安装进行，覆盖安装会结束本应用的进程，通常收不到成功的结果。用户取消时送来 `STATUS_FAILURE_ABORTED`。
 * - `PendingIntent` 必须可变（系统要往里填结果），Android 14 起可变的 `PendingIntent` 不能用隐式 Intent，用 `setPackage` 限定到本应用；
 *   接收器以 `RECEIVER_NOT_EXPORTED` 注册，本应用自己的 `PendingIntent` 发来的广播照样能收到。
 * - 最终结果以 `{ status, code, message }` 交给 Rust 侧：`status` 见 [InstallStatus.kind]，`message` 是系统给出的原因。
 */
object ApkInstaller {
  private const val ACTION = "com.cooksleep.rummage.storage.INSTALL_STATUS"

  /** 在后台线程中调用：写入 APK 要读整个文件 */
  fun install(activity: Activity, apk: File, invoke: Invoke) {
    val ctx = activity.applicationContext
    val installer = ctx.packageManager.packageInstaller
    val id: Int
    try {
      val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
      params.setSize(apk.length())
      id = installer.createSession(params)
    } catch (e: Exception) {
      Logger.error(Logger.tags("ApkInstaller"), "无法创建安装会话", e)
      invoke.reject(e.message ?: e.toString())
      return
    }
    val receiver = object : BroadcastReceiver() {
      override fun onReceive(context: Context, intent: Intent) {
        if (intent.getIntExtra(PackageInstaller.EXTRA_SESSION_ID, -1) != id) return
        val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)
        var message = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE)
        if (status == PackageInstaller.STATUS_PENDING_USER_ACTION) {
          val confirm = IntentCompat.getParcelableExtra(intent, Intent.EXTRA_INTENT, Intent::class.java)
          try {
            if (confirm != null) {
              activity.startActivity(confirm)
              return
            }
            message = "系统没有给出确认界面"
          } catch (e: Exception) {
            message = "无法打开系统的确认界面：${e.message}"
          }
          try {
            installer.abandonSession(id)
          } catch (e: Exception) {
            Logger.warn(Logger.tags("ApkInstaller"), "无法放弃安装会话：${e.message}")
          }
        }
        ctx.unregisterReceiver(this)
        val ret = JSObject()
        ret.put("status", if (status == PackageInstaller.STATUS_PENDING_USER_ACTION) "failure" else InstallStatus.kind(status))
        ret.put("code", InstallStatus.name(status))
        ret.put("message", message)
        invoke.resolve(ret)
      }
    }
    ContextCompat.registerReceiver(ctx, receiver, IntentFilter(ACTION), ContextCompat.RECEIVER_NOT_EXPORTED)
    try {
      installer.openSession(id).use { session ->
        apk.inputStream().use { input ->
          session.openWrite("base.apk", 0, apk.length()).use { out ->
            input.copyTo(out)
            session.fsync(out)
          }
        }
        val result = Intent(ACTION).setPackage(ctx.packageName)
        val pending = PendingIntent.getBroadcast(ctx, id, result, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE)
        session.commit(pending.intentSender)
      }
    } catch (e: Exception) {
      Logger.error(Logger.tags("ApkInstaller"), "无法写入或提交安装会话", e)
      ctx.unregisterReceiver(receiver)
      try {
        installer.abandonSession(id)
      } catch (e2: Exception) {
        Logger.warn(Logger.tags("ApkInstaller"), "无法放弃安装会话：${e2.message}")
      }
      invoke.reject(e.message ?: e.toString())
    }
  }
}
