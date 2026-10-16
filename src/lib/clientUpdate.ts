// 应用内更新：调用下载与安装命令（Windows 为 src-tauri/src/update.rs，Android 为 src-tauri/src/android/update.rs，两者同名同参数）

import { Channel, invoke } from '@tauri-apps/api/core'

/** 对应 Rust `update::Progress` */
export interface UpdateProgress {
  downloaded: number
  /** 服务器未返回大小时为 null */
  total: number | null
}

/** 本地有效安装包（版本高于当前运行版本且通过 SHA-256 校验）的版本，没有时为 null；校验失败的文件会被删除 */
export function getReadyUpdate() {
  return invoke<string | null>('update_ready')
}

/** 下载并校验 version 对应的更新包后保存到本地（覆盖旧包）；更新信息（latest.json、android.json）尚未同步到该版本时报错 */
export function downloadUpdate(version: string, onProgress: (progress: UpdateProgress) => void) {
  const channel = new Channel<UpdateProgress>()
  channel.onmessage = onProgress
  return invoke<void>('update_download', { version, onProgress: channel })
}

/** 安装已下载的更新包：Windows 先停后台服务，成功时应用退出并由安装程序重新打开；Android 交给系统安装器，用户取消时报错；无论远端是否已有更新的版本，安装的都是已下载的版本 */
export function installUpdate() {
  return invoke<void>('update_install')
}

/** 下载按钮上显示的进度，未知总大小时显示已下载的 MB */
export function formatUpdateProgress(progress: UpdateProgress) {
  if (progress.total) return `${Math.min(100, Math.floor((progress.downloaded / progress.total) * 100))}%`
  return `${(progress.downloaded / 1024 / 1024).toFixed(1)} MB`
}
