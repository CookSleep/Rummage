package com.cooksleep.rummage.storage

import android.app.Notification
import android.app.NotificationManager
import android.app.job.JobInfo
import android.app.job.JobParameters
import android.app.job.JobScheduler
import android.app.job.JobService
import android.content.ComponentName
import android.content.Context
import android.os.BatteryManager
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import app.tauri.Logger

/**
 * 因未接电源暂停建立索引时，前台服务停止（等待期间也会消耗每天 6 小时的后台额度），改由这个作业在接上电源时唤醒应用。
 * 由 Rust 侧（src-tauri/src/android/background.rs）经 [StoragePlugin] 安排与取消，不跨重启保留。
 * - 运行时向 Rust 侧报告 `charging`：后台服务随即发现已接上电源、继续建立索引，Rust 侧再启动前台服务
 *   （Android 12 起从后台启动前台服务要求应用不受电池优化限制）。作业等到前台服务启动后结束，最多等 1 分钟，期间进程不被冻结。
 * - 应用进程已被结束（作业在新进程中运行，Rust 侧没有加载），或等满 1 分钟前台服务仍没启动、而仍接着电源时，发一条通知请用户打开应用。
 *   后者同时报告 `power_job_done`，Rust 侧据此在仍未接电源暂停时重新安排。
 * - 作业运行中不取消也不重新安排：重新安排同一个作业会让系统结束正在运行的这一次。
 */
class PowerJob : JobService() {
  private val handler = Handler(Looper.getMainLooper())

  override fun onStartJob(params: JobParameters): Boolean {
    if (!BackgroundEvents.attached) {
      askToOpen(this)
      return false
    }
    running = true
    BackgroundEvents.post("charging")
    val since = SystemClock.elapsedRealtime()
    val check = object : Runnable {
      override fun run() {
        if (IndexService.isRunning) return finish(params)
        if (SystemClock.elapsedRealtime() - since >= WAIT_MS) {
          if (getSystemService(BatteryManager::class.java)?.isCharging == true) askToOpen(this@PowerJob)
          finish(params)
          BackgroundEvents.post("power_job_done")
          return
        }
        handler.postDelayed(this, CHECK_MS)
      }
    }
    handler.postDelayed(check, CHECK_MS)
    return true
  }

  override fun onStopJob(params: JobParameters): Boolean {
    handler.removeCallbacksAndMessages(null)
    running = false
    return false
  }

  private fun finish(params: JobParameters) {
    running = false
    jobFinished(params, false)
  }

  companion object {
    private const val JOB_ID = 1
    private const val NOTICE_ID = 3
    private const val WAIT_MS = 60_000L
    private const val CHECK_MS = 2000L

    @Volatile private var running = false

    /** 请用户打开应用继续 */
    private fun askToOpen(ctx: Context) {
      val nm = ctx.getSystemService(NotificationManager::class.java) ?: return
      IndexService.channels(nm)
      val n = Notification.Builder(ctx, IndexService.NOTICE_CHANNEL)
        .setSmallIcon(R.drawable.rummage_notification)
        .setContentTitle("已接上电源")
        .setContentText("打开 Rummage 即可继续建立索引")
        .setContentIntent(IndexService.openApp(ctx))
        .setAutoCancel(true)
        .build()
      nm.notify(NOTICE_ID, n)
    }

    /** 安排在接上电源时运行；已安排或正在运行时不变 */
    fun schedule(ctx: Context) {
      val js = ctx.getSystemService(JobScheduler::class.java) ?: return
      if (running || js.getPendingJob(JOB_ID) != null) return
      val job = JobInfo.Builder(JOB_ID, ComponentName(ctx, PowerJob::class.java))
        .setRequiresCharging(true)
        .build()
      if (js.schedule(job) != JobScheduler.RESULT_SUCCESS) {
        Logger.warn(Logger.tags("PowerJob"), "无法安排接上电源时继续建立索引的作业")
      }
    }

    /** 取消尚未运行的作业 */
    fun cancel(ctx: Context) {
      if (running) return
      ctx.getSystemService(JobScheduler::class.java)?.cancel(JOB_ID)
    }
  }
}
