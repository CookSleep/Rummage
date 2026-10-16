import { useEffect, useState } from 'react'
import { trimPeriod } from '../lib/format'

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

let nextId = 0
let listener: ((t: ToastItem) => void) | null = null

/** 在底部显示一条胶囊通知 3 秒；同一时间只显示一条，新的替换旧的，不会互相重叠 */
export function showToast(message: string, type: ToastType = 'info') {
  listener?.({ id: ++nextId, message, type })
}

/**
 * 底部的胶囊通知（入场动画 `toast-enter`）。在 App 中放一个，各处用 `showToast` 显示；
 * 层级高于设置等弹窗。每条通知有自己的 id，连续两条相同的文字也各自计时
 */
export default function Toaster() {
  const [toast, setToast] = useState<ToastItem | null>(null)

  useEffect(() => {
    listener = setToast
    return () => {
      listener = null
    }
  }, [])

  useEffect(() => {
    if (!toast) return
    const timer = setTimeout(() => setToast(null), 3000)
    return () => clearTimeout(timer)
  }, [toast])

  if (!toast) return null

  return (
    <div key={toast.id} role="status" className="toast-enter pointer-events-none fixed bottom-24 left-1/2 z-[120]">
      <div className="flex w-max max-w-[calc(100vw-32px)] items-center gap-2.5 rounded-full border border-gray-200/60 bg-white/95 px-5 py-3.5 text-sm font-medium text-gray-700 shadow-[0_8px_30px_rgb(0,0,0,0.12)] ring-1 ring-black/5 backdrop-blur-xl dark:border-white/[0.08] dark:bg-gray-900/95 dark:text-gray-300 dark:shadow-[0_8px_30px_rgb(0,0,0,0.3)] dark:ring-white/10 sm:max-w-[min(44rem,80vw)]">
        <span className={`flex h-5 w-5 flex-shrink-0 items-center justify-center rounded-full ${ICON_TILE[toast.type]}`}>
          <svg className="h-3.5 w-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden>
            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2.5} d={ICON_PATH[toast.type]} />
          </svg>
        </span>
        <span className="whitespace-pre-line text-center leading-5">{trimPeriod(toast.message)}</span>
      </div>
    </div>
  )
}
