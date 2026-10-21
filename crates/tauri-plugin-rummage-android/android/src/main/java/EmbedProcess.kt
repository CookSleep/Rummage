package com.cooksleep.rummage.storage

import android.app.Service
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.net.Uri
import android.os.Binder
import android.os.IBinder
import android.os.Process
import app.tauri.Logger
import java.util.concurrent.ConcurrentHashMap

/**
 * GPU、NPU 推理所在的 `:embed` 进程的启动与结束，由 Rust 侧（src-tauri/src/android/embed.rs）经 JNI 调用。
 * 每个推理进程绑定一次 [EmbedService]：Intent 的 data 带套接字名，使每次绑定都是不同的 Intent（extras 不参与比较），
 * 服务都会收到 onBind。结束时先解除绑定，否则推理进程被结束后系统会按绑定重新拉起服务。
 */
object EmbedProcess {
  private val bindings = ConcurrentHashMap<String, ServiceConnection>()

  /** 绑定服务，交给它套接字名与以换行连接的参数；系统拒绝时返回 false */
  @JvmStatic
  fun start(ctx: Context, socket: String, args: String): Boolean {
    val intent = Intent(ctx, EmbedService::class.java)
      .setData(Uri.parse("rummage-embed://$socket"))
      .putExtra(EXTRA_SOCKET, socket)
      .putExtra(EXTRA_ARGS, args)
    val conn = object : ServiceConnection {
      override fun onServiceConnected(name: ComponentName?, service: IBinder?) {}
      override fun onServiceDisconnected(name: ComponentName?) {}
    }
    val ok = ctx.bindService(intent, conn, Context.BIND_AUTO_CREATE or Context.BIND_IMPORTANT)
    if (ok) {
      bindings[socket] = conn
    } else {
      try {
        ctx.unbindService(conn)
      } catch (e: IllegalArgumentException) {
        Logger.warn(Logger.tags("EmbedProcess"), "绑定推理进程失败：${e.message}")
      }
    }
    return ok
  }

  @JvmStatic
  fun stop(ctx: Context, socket: String) {
    val conn = bindings.remove(socket) ?: return
    try {
      ctx.unbindService(conn)
    } catch (e: IllegalArgumentException) {
      Logger.warn(Logger.tags("EmbedProcess"), "推理进程已解除绑定：${e.message}")
    }
  }

  const val EXTRA_SOCKET = "socket"
  const val EXTRA_ARGS = "args"
}

/**
 * 运行 GPU、NPU 推理的服务，在单独的 `:embed` 进程中（见清单）。收到绑定后在自己的线程中加载应用的原生库并运行推理。
 * 主进程解除绑定时结束整个进程，释放加速器占用的内存；推理先结束（出错）时不自行结束：还被绑定着的服务所在进程退出后，
 * 系统会按绑定重新拉起它。
 */
class EmbedService : Service() {
  override fun onBind(intent: Intent): IBinder {
    val socket = intent.getStringExtra(EmbedProcess.EXTRA_SOCKET)
    val args = intent.getStringExtra(EmbedProcess.EXTRA_ARGS)
    if (socket != null && args != null) {
      Thread({
        // 与 src-tauri/Cargo.toml 的 [lib] name 相同
        System.loadLibrary("rummage_lib")
        val code = runWorker(socket, args)
        Logger.info(Logger.tags("EmbedService"), "推理结束：$code")
      }, "embed-worker").start()
    }
    return Binder()
  }

  override fun onUnbind(intent: Intent): Boolean {
    Process.killProcess(Process.myPid())
    return false
  }

  private external fun runWorker(socket: String, args: String): Int
}
