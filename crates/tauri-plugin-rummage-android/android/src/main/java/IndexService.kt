package com.cooksleep.rummage.storage

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import android.os.SystemClock
import app.tauri.Logger
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject

/**
 * 建立索引的前台服务（Android 15 起类型为 `mediaProcessing`，Android 14 为 `dataSync`），只负责让系统在应用退到后台时保留进程，索引本身在应用进程的后台服务中进行。
 * 由 Rust 侧（src-tauri/src/android/background.rs）按索引进度经 [StoragePlugin] 启停。
 * - 常驻通知显示进度，渠道为低重要度；点击打开应用。更新至少间隔 2 秒，避免被系统限流。
 *   因亮屏、过热暂停期间服务保持运行，通知标题改为“已暂停建立索引”，正文为暂停原因（`note`）；因未接电源暂停时服务停止，
 *   接上电源时由 [PowerJob] 唤醒。
 * - 正在建立索引（没有暂停原因）时持有部分唤醒锁：前台服务只保留进程，不阻止息屏后 CPU 休眠；暂停与停止时释放。
 *   锁有时限，每次进度更新时续期：处理卡住、进度长时间不动时自行释放，不让 CPU 空等。
 * - Android 15 起 `mediaProcessing` 每 24 小时共有 6 小时额度，用完时系统回调 [onTimeout]，必须几秒内停止：
 *   先报告 `timeout` 事件，立即 `stopSelf`，再发一条普通通知说明原因。
 */
class IndexService : Service() {
  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    try {
      val n = progress(this)
      // 清单中的类型由 aapt 按 compileSdk 编成位标志（mediaProcessing|dataSync）；startForeground 传入的类型只需是
      // 清单类型的子集（不是子集时抛出 IllegalArgumentException）。mediaProcessing 是 Android 15 新增的类型，
      // Android 14 起又要求传入清单中的类型，所以 Android 14 传 dataSync；官方文档也写明了 mediaProcessing 服务
      // 在 Android 14 及更早版本上的行为，清单中声明它不影响旧版本。Android 13 及更早的版本不传类型
      when {
        Build.VERSION.SDK_INT >= 35 -> startForeground(PROGRESS_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROCESSING)
        Build.VERSION.SDK_INT == 34 -> startForeground(PROGRESS_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
        else -> startForeground(PROGRESS_ID, n)
      }
      running = true
      hold(this)
    } catch (e: Exception) {
      // ForegroundServiceStartNotAllowedException（应用在后台、额度已用完）等
      Logger.warn(Logger.tags("IndexService"), "前台服务没能进入前台：${e.message}")
      BackgroundEvents.post("stopped")
      stopSelf()
    }
    return START_NOT_STICKY
  }

  override fun onTimeout(startId: Int, fgsType: Int) {
    BackgroundEvents.post("timeout")
    stopSelf()
    val nm = getSystemService(NotificationManager::class.java) ?: return
    channels(nm)
    val n = Notification.Builder(this, NOTICE_CHANNEL)
      .setSmallIcon(R.drawable.rummage_notification)
      .setContentTitle("已暂停建立索引")
      .setContentText("系统限制应用每天在后台最多运行 6 小时。打开 Rummage 即可继续。")
      .setStyle(Notification.BigTextStyle().bigText("系统限制应用每天在后台最多运行 6 小时。打开 Rummage 即可继续。"))
      .setContentIntent(openApp(this))
      .setAutoCancel(true)
      .build()
    nm.notify(QUOTA_ID, n)
  }

  override fun onDestroy() {
    running = false
    hold(this)
    handler.removeCallbacksAndMessages(null)
    super.onDestroy()
  }

  companion object {
    private const val PROGRESS_CHANNEL = "indexing"
    const val NOTICE_CHANNEL = "notices"
    private const val PROGRESS_ID = 1
    private const val QUOTA_ID = 2
    private const val MIN_UPDATE_MS = 2000L
    /** 唤醒锁的时限：比处理单个文件（含长视频）的时间长，又不至于在卡住时整夜持有 */
    private const val WAKE_MS = 30 * 60 * 1000L

    private val handler = Handler(Looper.getMainLooper())

    @Volatile private var running = false
    val isRunning get() = running
    @Volatile private var done = 0L
    @Volatile private var total = 0L
    /** 暂停原因；为空时显示进度 */
    @Volatile private var note: String? = null
    private var shownAt = 0L
    private var updateQueued = false
    /** 只在主线程上读写（[hold]） */
    private var wake: PowerManager.WakeLock? = null

    /** 按是否正在建立索引持有或释放唤醒锁；在主线程上调用 */
    private fun hold(ctx: Context) {
      val want = running && note == null
      val w = wake
      if (want) {
        // 不计引用：再次 acquire 只是续期
        val lock = w ?: ctx.getSystemService(PowerManager::class.java)
          ?.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "Rummage:index")
          ?.apply { setReferenceCounted(false) }
        lock?.acquire(WAKE_MS)
        wake = lock
      } else if (w != null) {
        if (w.isHeld) w.release()
        wake = null
      }
    }

    fun channels(nm: NotificationManager) {
      nm.createNotificationChannel(NotificationChannel(PROGRESS_CHANNEL, "建立索引", NotificationManager.IMPORTANCE_LOW))
      nm.createNotificationChannel(NotificationChannel(NOTICE_CHANNEL, "提示", NotificationManager.IMPORTANCE_DEFAULT))
    }

    fun openApp(ctx: Context): PendingIntent? {
      val intent = ctx.packageManager.getLaunchIntentForPackage(ctx.packageName) ?: return null
      return PendingIntent.getActivity(ctx, 0, intent, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
    }

    private fun progress(ctx: Context): Notification {
      val nm = ctx.getSystemService(NotificationManager::class.java)
      if (nm != null) channels(nm)
      shownAt = SystemClock.elapsedRealtime()
      val max = total.coerceAtMost(Int.MAX_VALUE.toLong()).toInt()
      val cur = done.coerceAtMost(total).coerceAtMost(Int.MAX_VALUE.toLong()).toInt()
      val b = Notification.Builder(ctx, PROGRESS_CHANNEL)
        .setSmallIcon(R.drawable.rummage_notification)
        .setContentIntent(openApp(ctx))
        .setOngoing(true)
        .setOnlyAlertOnce(true)
      val paused = note
      if (paused != null) return b.setContentTitle("已暂停建立索引").setContentText(paused).build()
      return b.setContentTitle("正在建立索引").setContentText("已完成 $done / 共 $total").setProgress(max, cur, max == 0).build()
    }

    /** 启动服务；系统不允许从后台启动时返回 false */
    fun start(ctx: Context, done: Long, total: Long, note: String?): Boolean {
      this.done = done
      this.total = total
      this.note = note
      return try {
        ctx.startForegroundService(Intent(ctx, IndexService::class.java))
        true
      } catch (e: IllegalStateException) {
        // Android 12 起从后台启动抛出 ForegroundServiceStartNotAllowedException（IllegalStateException 的子类）
        Logger.warn(Logger.tags("IndexService"), "无法启动前台服务：${e.message}")
        false
      }
    }

    /** 更新进度或暂停原因；距上次显示不到 2 秒时推迟到满 2 秒，期间的更新合并为一次 */
    fun update(ctx: Context, done: Long, total: Long, note: String?) {
      this.done = done
      this.total = total
      this.note = note
      if (!running) return
      val app = ctx.applicationContext
      handler.post {
        hold(app)
        if (updateQueued) return@post
        val wait = shownAt + MIN_UPDATE_MS - SystemClock.elapsedRealtime()
        updateQueued = true
        handler.postDelayed({
          updateQueued = false
          if (!running) return@postDelayed
          app.getSystemService(NotificationManager::class.java)?.notify(PROGRESS_ID, progress(app))
        }, wait.coerceAtLeast(0))
      }
    }

    fun stop(ctx: Context) {
      ctx.stopService(Intent(ctx, IndexService::class.java))
    }
  }
}

/**
 * 交给 Rust 侧的事件（`resume`、`timeout`、`stopped`、`charging`、`power_job_done`）：Rust 侧循环调用 `nextEvent` 等待（[StoragePlugin.nextEvent]），没有等待者时排队；
 * 同一种事件在队列中只留一个
 */
object BackgroundEvents {
  private val queue = ArrayDeque<String>()
  private var waiting: Invoke? = null
  /** Rust 侧已开始等待事件；进程由 [PowerJob] 新启动时没有 Rust 侧，为 false */
  @Volatile var attached = false
    private set

  private fun resolve(invoke: Invoke, type: String) {
    val ret = JSObject()
    ret.put("type", type)
    invoke.resolve(ret)
  }

  @Synchronized
  fun post(type: String) {
    val w = waiting
    if (w != null) {
      waiting = null
      resolve(w, type)
      return
    }
    if (type !in queue) queue.addLast(type)
  }

  @Synchronized
  fun next(invoke: Invoke) {
    attached = true
    val type = queue.removeFirstOrNull()
    if (type == null) waiting = invoke else resolve(invoke, type)
  }
}
