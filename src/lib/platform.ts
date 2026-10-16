import { invoke, isTauri as detectTauri } from '@tauri-apps/api/core'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { readLocal } from './storage'

export const isTauri = detectTauri()

/** Android 客户端：WebView 的 User-Agent 中带 `Android` */
export const isAndroid = navigator.userAgent.includes('Android')

// 刷新、前进后退、打印、查看源码、保存页面等浏览器快捷键，在客户端中会刷掉进行中的操作或打开无关界面；
// 缩放（Ctrl+加减号/0）、开发者工具（F12、Ctrl+Shift+I）和页面内查找（Ctrl+F，以及查找下一个 F3、Ctrl+G）不在其中
const BLOCKED_KEYS = new Set(['f5', 'f7', 'browserrefresh', 'browserback', 'browserforward', 'browsersearch'])
const BLOCKED_CTRL_KEYS = new Set(['r', 'p', 'u', 's'])
// 单按 Alt 或 F10 会让宿主窗口进入系统菜单模式，期间所有 IPC 停住；按下和松开都要阻止。
// 只拦截这两个键本身，Alt+F4、Alt+Tab、Alt+Space 和 Alt+滚轮不受影响
const MENU_KEYS = new Set(['alt', 'f10'])
const EDITABLE_INPUT_TYPES = new Set(['text', 'search', 'url', 'tel', 'email', 'password', 'number'])

/**
 * 客户端中屏蔽浏览器自带的快捷键和右键菜单。
 * 不用 WebView2 的 AreBrowserAcceleratorKeysEnabled，它会连同 F12 和缩放快捷键一起禁用；
 * 这里只阻止默认行为，不阻止事件传播，应用自己的快捷键和右键菜单照常收到事件
 */
export function installBrowserBehaviorGuards() {
  window.addEventListener('keydown', (e) => {
    // 自动填充等派发的 keydown 可能没有 key
    const key = e.key?.toLowerCase()
    // Shift+F10 用于键盘呼出右键菜单，不拦截
    if (BLOCKED_KEYS.has(key) || (MENU_KEYS.has(key) && !e.shiftKey) || ((e.ctrlKey || e.metaKey) && !e.altKey && BLOCKED_CTRL_KEYS.has(key))) e.preventDefault()
  }, true)
  window.addEventListener('keyup', (e) => {
    if (MENU_KEYS.has(e.key?.toLowerCase())) e.preventDefault()
  }, true)

  window.addEventListener('contextmenu', (e) => {
    const el = e.target
    // 输入框中保留剪切、复制、粘贴；复选框等非文本输入弹出的是整页菜单（含刷新、打印），同样屏蔽
    const editable = el instanceof HTMLElement && (
      el.isContentEditable
      || el instanceof HTMLTextAreaElement
      || (el instanceof HTMLInputElement && EDITABLE_INPUT_TYPES.has(el.type))
    )
    if (editable) return
    // 在选中的文字上右键时保留菜单，方便复制；在选区外右键弹出的是整页菜单，仍然屏蔽
    const sel = window.getSelection()
    const onSelection = sel && !sel.isCollapsed && sel.rangeCount > 0 && Array.from(sel.getRangeAt(0).getClientRects()).some((r) =>
      e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom)
    if (!onSelection) e.preventDefault()
  }, true)
}

/** “关闭窗口时”的本地存储键（设置 → 外观）：`quit` 时关闭窗口等同托盘的“退出”，其余留在托盘 */
export const CLOSE_ACTION_KEY = 'rummage.closeAction'

let beforeClose: (() => Promise<void>) | null = null

/** 关闭窗口前要做的事（如第一次关闭时的托盘提示），处理完才关闭；同一时间只有一个 */
export function setBeforeClose(fn: (() => Promise<void>) | null) {
  beforeClose = fn
}

/**
 * 接管关闭：总是阻止 Tauri 自带的销毁，处理完后自己销毁窗口（界面进程留在托盘，见 src-tauri/src/lib.rs）；
 * 设置为关闭时退出的，改为连同后台服务一起退出（`exit_app`，同托盘的“退出”）。开始处理时先回执，
 * Rust 侧（src-tauri/src/close_watch.rs）约 8 秒内收不到回执会认为页面卡死并强制关闭窗口。
 * Tauri 对每个 onCloseRequested 监听各自销毁窗口，所以只注册这一个，其他关闭前的处理经 setBeforeClose 交给它；
 * 处理期间再次请求关闭时沿用同一次处理，不会销毁两次
 */
export async function installCloseWatch() {
  const win = getCurrentWindow()
  let closing: Promise<void> | null = null
  await win.onCloseRequested((e) => {
    e.preventDefault()
    invoke('close_ack').catch((err) => console.warn('关闭请求回执失败：', err))
    if (closing) return closing
    closing = (async () => {
      if (readLocal(CLOSE_ACTION_KEY) === 'quit') {
        try {
          await invoke('exit_app')
          return
        } catch (err) {
          // 退出失败时至少关掉窗口，界面进程留在托盘
          console.error('退出失败，改为只关闭窗口：', err)
        }
      }
      await beforeClose?.()
      await win.destroy()
    })()
      .catch((err) => console.error('关闭窗口失败：', err))
      .finally(() => {
        closing = null
      })
    return closing
  })
}

/** 用系统浏览器打开外部链接：只允许 https，与 capabilities 中 opener 的范围一致 */
export function openExternal(url: string) {
  if (!isTauri) {
    window.open(url, '_blank', 'noopener,noreferrer')
    return
  }
  invoke('plugin:opener|open_url', { url }).catch((err) => console.error('打开外部链接失败：', err))
}
