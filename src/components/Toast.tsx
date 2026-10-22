import { useEffect, useState } from 'react'
import { trimPeriod } from '../lib/format'
import ConfirmDialog from './ConfirmDialog'

type ToastType = 'success' | 'error' | 'info'

interface ToastItem {
  id: number
  message: string
  type: ToastType
}

const ICON_PATH = {
  success: 'M5 13l4 4L19 7',
  error: 'M6 18L18 6M6 6l12 12',
  info: 'M13 16h-1v-4h-1m1-4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z',
}

const ICON_TILE = {
  success: 'bg-green-100 text-green-600 dark:bg-green-900/50 dark:text-green-400',
  error: 'bg-red-100 text-red-600 dark:bg-red-900/50 dark:text-red-400',
  info: 'bg-blue-100 text-blue-600 dark:bg-blue-900/50 dark:text-blue-400',
}

/** 过长的消息改用弹窗时的标题 */
const DIALOG_TITLE = { success: '已完成', error: '操作失败', info: '提示' }

/** 超过这个字数，胶囊通知 3 秒内读不完，手机宽度下也要折成两行以上 */
const LONG_TOAST_CHARS = 40

let nextId = 0
let listener: ((t: ToastItem) => void) | null = null

/**
 * 在底部显示一条胶囊通知 3 秒；同一时间只显示一条，新的替换旧的，不会互相重叠。
 * 多句、多行或过长的消息改用只有“知道了”的弹窗，关闭前一直显示
 */
export function showToast(message: string, type: ToastType = 'info') {
  listener?.({ id: ++nextId, message, type })
}

/**
 * 底部的胶囊通知（入场动画 `toast-enter`）。在 App 中放一个，各处用 `showToast` 显示；
 * 层级高于设置等弹窗。每条通知有自己的 id，连续两条相同的文字也各自计时。过长的消息显示为弹窗，依次显示，与胶囊通知互不替换
 */
export default function Toaster() {
  const [toast, setToast] = useState<ToastItem | null>(null)
  // 过长的消息排队，关闭一个再显示下一个
  const [dialogs, setDialogs] = useState<ToastItem[]>([])
  const close = () => setDialogs((d) => d.slice(1))

  useEffect(() => {
    listener = (t) => {
      const msg = trimPeriod(t.message)
      // 句末的标点不算，只看句中是否还有句号或换行
      if (/[。！？\n]/.test(msg.slice(0, -1)) || msg.length > LONG_TOAST_CHARS) setDialogs((d) => [...d, t])
      else setToast(t)
    }
    return () => {
      listener = null
    }
  }, [])

  useEffect(() => {
    if (!toast) return
    const timer = setTimeout(() => setToast(null), 3000)
    return () => clearTimeout(timer)
  }, [toast])

  return (
    <>
      {toast && (
        <div key={toast.id} role="status" className="toast-enter pointer-events-none fixed bottom-[calc(6rem+var(--safe-bottom))] left-1/2 z-[120]">
          <div className="flex w-max max-w-[calc(100vw-32px)] items-center gap-2.5 rounded-full border border-gray-200/60 bg-white/95 px-5 py-3.5 text-sm font-medium text-gray-700 shadow-[0_8px_30px_rgb(0,0,0,0.12)] ring-1 ring-black/5 backdrop-blur-xl dark:border-white/[0.08] dark:bg-gray-900/95 dark:text-gray-300 dark:shadow-[0_8px_30px_rgb(0,0,0,0.3)] dark:ring-white/10 sm:max-w-[min(44rem,80vw)]">
            <span className={`flex h-5 w-5 flex-shrink-0 items-center justify-center rounded-full ${ICON_TILE[toast.type]}`}>
              <svg className="h-3.5 w-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden>
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2.5} d={ICON_PATH[toast.type]} />
              </svg>
            </span>
            <span className="whitespace-pre-line text-center leading-5">{trimPeriod(toast.message)}</span>
          </div>
        </div>
      )}
      {dialogs[0] && (
        <ConfirmDialog
          key={dialogs[0].id}
          title={DIALOG_TITLE[dialogs[0].type]}
          // 多行的消息每行一段
          message={dialogs[0].message.replace(/\n+/g, '\n\n')}
          confirmText="知道了"
          hideCancel
          onConfirm={close}
          onCancel={close}
        />
      )}
    </>
  )
}
