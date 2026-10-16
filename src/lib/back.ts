// Android 返回键：打开着的弹窗、下拉菜单、详情页、设置等各自登记一层（`useBackLayer`），返回键只关闭最后登记的那一层；
// 一层都没有时把应用退到后台（`move_task_to_back`），不退出进程，后台建立索引继续。
// 用 Tauri 自带的 `onBackButtonPress`（@tauri-apps/api 2.9.0 起）：注册监听后 AppPlugin 的 OnBackPressedCallback
// 不再自行后退或关闭 Activity，每次按返回键都交给这里

import { invoke } from '@tauri-apps/api/core'
import { onBackButtonPress } from '@tauri-apps/api/app'

const layers: (() => void)[] = []

/** 登记一层，返回撤销登记的函数 */
export function pushBackLayer(close: () => void) {
  const layer = () => close()
  layers.push(layer)
  return () => {
    const idx = layers.lastIndexOf(layer)
    if (idx >= 0) layers.splice(idx, 1)
  }
}

/** 关闭最上面的一层；没有可关闭的层时返回 false */
export function handleBack() {
  // 全屏播放的视频先退出全屏
  if (document.fullscreenElement) {
    document.exitFullscreen().catch(() => {})
    return true
  }
  const top = layers[layers.length - 1]
  if (!top) return false
  top()
  return true
}

export async function installBackButton() {
  await onBackButtonPress(() => {
    if (handleBack()) return
    invoke('move_task_to_back').catch((err) => console.warn('无法把应用退到后台：', err))
  })
}
