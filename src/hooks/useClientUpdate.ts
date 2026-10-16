import { invoke } from '@tauri-apps/api/core'
import { create } from 'zustand'
import type { SetupStatus } from '../types'
import type { UpdateProgress } from '../lib/clientUpdate'
import { downloadUpdate, getReadyUpdate, installUpdate } from '../lib/clientUpdate'
import { errorMessage } from '../lib/errors'
import { compareVersions } from '../lib/releaseUpdates'
import { showToast } from '../components/Toast'

/** 安装前的确认框，由更新弹窗显示 */
export interface InstallConfirm {
  title: string
  /** 空行分段 */
  message: string
  confirmText: string
  danger?: boolean
  extra?: { label: string; onClick: () => void }
  onConfirm: () => void
}

interface ClientUpdateState {
  status: 'idle' | 'downloading' | 'ready' | 'installing'
  progress: UpdateProgress | null
  // 本地有效安装包的版本，可能低于弹窗中的最新版本
  readyVersion: string
  confirm: InstallConfirm | null
}

/** 应用内更新的状态；放在模块级 store 中，关闭弹窗后下载继续，再次打开仍能看到进度 */
export const useClientUpdate = create<ClientUpdateState>(() => ({
  status: 'idle',
  progress: null,
  readyVersion: '',
  confirm: null,
}))

const isBusy = () => {
  const status = useClientUpdate.getState().status
  return status === 'downloading' || status === 'installing'
}

async function readReady() {
  try {
    return await getReadyUpdate()
  } catch (err) {
    console.warn('检查本地更新包失败：', err)
    return null
  }
}

/** 打开弹窗时检查本地是否已有校验通过的安装包，有则直接显示“安装” */
export async function refreshClientUpdate() {
  if (isBusy()) return
  const version = await readReady()
  // 校验期间可能已开始下载或安装，此时不能用旧结果覆盖状态
  if (isBusy()) return
  useClientUpdate.setState({ status: version ? 'ready' : 'idle', readyVersion: version ?? '' })
}

export async function startClientUpdateDownload(version: string) {
  useClientUpdate.setState({ status: 'downloading', progress: null })
  try {
    await downloadUpdate(version, (progress) => useClientUpdate.setState({ progress }))
    useClientUpdate.setState({ status: 'ready', readyVersion: version })
  } catch (err) {
    console.warn('下载更新失败：', err)
    // 保存失败时原有安装包可能已被删除，也可能仍在，按磁盘上的实际情况显示“安装”或“下载”
    const ready = await readReady()
    useClientUpdate.setState({ status: ready ? 'ready' : 'idle', readyVersion: ready ?? '' })
    showToast(errorMessage(err, '下载更新失败，请重试。'), 'error')
  }
}

/** 窗口状态由 Rust 侧在安装前写下，前端的设置都已即时保存 */
export async function startClientUpdateInstall() {
  useClientUpdate.setState({ status: 'installing' })
  try {
    // 成功时应用会退出并由安装程序重新打开，不会返回
    await installUpdate()
  } catch (err) {
    console.warn('安装更新失败：', err)
    // 安装包可能已在 Rust 侧校验失败并被删除，重新确认后决定显示“安装”还是“下载”
    const version = await readReady()
    useClientUpdate.setState({ status: version ? 'ready' : 'idle', readyVersion: version ?? '' })
    showToast(errorMessage(err, '安装更新失败，请重试。'), 'error')
  }
}

/** 后台服务正在下载、导入模型或检测设备；这些操作在退出后不能接着进行。读不到状态时按没有处理 */
async function hasInterruptibleOperations() {
  try {
    const s = await invoke<SetupStatus>('setup_status')
    const task = s.task?.state === 'running' && (s.task.kind === 'download' || s.task.kind === 'import')
    return task || s.selftest.running
  } catch {
    return false
  }
}

export const closeConfirm = () => useClientUpdate.setState({ confirm: null })

/**
 * 点击“安装”：安装会关闭 Rummage 和后台服务，有退出后无法接着进行的操作时先确认；
 * 已下载的版本低于最新版本时一并询问，可改为下载最新版本（覆盖已下载的安装包），取消时保留安装包
 */
export async function requestClientUpdateInstall(latestVersion: string) {
  if (isBusy()) return
  // 查询后台服务期间按钮已禁用，重复点击不会再次进入
  useClientUpdate.setState({ status: 'installing' })
  const readyVersion = useClientUpdate.getState().readyVersion
  const busy = await hasInterruptibleOperations()
  const outdated = compareVersions(readyVersion, latestVersion) < 0
  if (!busy && !outdated) return void startClientUpdateInstall()
  useClientUpdate.setState({ status: 'ready' })
  const warning = '后台服务正在下载、导入模型或检测设备。\n\n安装更新会关闭 Rummage 和后台服务，并中断这些操作。'
  const install = () => {
    closeConfirm()
    void startClientUpdateInstall()
  }
  if (!outdated) {
    useClientUpdate.setState({ confirm: { title: '安装更新', message: warning, confirmText: '安装', danger: true, onConfirm: install } })
    return
  }
  useClientUpdate.setState({
    confirm: {
      title: '发现更新的版本',
      message: `最新版本为 v${latestVersion}，已下载的是 v${readyVersion}。${busy ? `\n\n${warning}` : ''}`,
      confirmText: '继续安装',
      danger: busy,
      extra: {
        label: '下载新版本',
        onClick: () => {
          closeConfirm()
          void startClientUpdateDownload(latestVersion)
        },
      },
      onConfirm: install,
    },
  })
}
