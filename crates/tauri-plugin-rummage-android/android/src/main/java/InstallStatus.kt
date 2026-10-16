package com.cooksleep.rummage.storage

import android.content.pm.PackageInstaller

/** 安装会话的结果码（`PackageInstaller.STATUS_*`）交给 Rust 侧的分类与写进日志的名称；只用常量，可在 JVM 上测试 */
object InstallStatus {
  /** `pending`：等用户确认；`success`；`aborted`：用户取消；其余都是 `failure` */
  fun kind(status: Int): String = when (status) {
    PackageInstaller.STATUS_PENDING_USER_ACTION -> "pending"
    PackageInstaller.STATUS_SUCCESS -> "success"
    PackageInstaller.STATUS_FAILURE_ABORTED -> "aborted"
    else -> "failure"
  }

  fun name(status: Int): String = when (status) {
    PackageInstaller.STATUS_PENDING_USER_ACTION -> "STATUS_PENDING_USER_ACTION"
    PackageInstaller.STATUS_SUCCESS -> "STATUS_SUCCESS"
    PackageInstaller.STATUS_FAILURE -> "STATUS_FAILURE"
    PackageInstaller.STATUS_FAILURE_BLOCKED -> "STATUS_FAILURE_BLOCKED"
    PackageInstaller.STATUS_FAILURE_ABORTED -> "STATUS_FAILURE_ABORTED"
    PackageInstaller.STATUS_FAILURE_INVALID -> "STATUS_FAILURE_INVALID"
    PackageInstaller.STATUS_FAILURE_CONFLICT -> "STATUS_FAILURE_CONFLICT"
    PackageInstaller.STATUS_FAILURE_STORAGE -> "STATUS_FAILURE_STORAGE"
    PackageInstaller.STATUS_FAILURE_INCOMPATIBLE -> "STATUS_FAILURE_INCOMPATIBLE"
    PackageInstaller.STATUS_FAILURE_TIMEOUT -> "STATUS_FAILURE_TIMEOUT"
    else -> "STATUS_$status"
  }
}
