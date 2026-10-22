import type { ReactNode } from 'react'
import { createPortal } from 'react-dom'
import { useEscape } from '../hooks/useEscape'

/**
 * 「」中的内容（如路径）连同括号加粗，`**…**` 中的内容去掉星号加粗。
 * 提示框等处也用它；`strong` 为加粗部分的类名，默认加深颜色
 */
export function renderMessage(message: string, strong = 'font-semibold text-gray-700 dark:text-gray-200') {
  return message.split(/(「[^」]+」|\*\*[^*]+\*\*)/g).map((part, idx) => {
    if (part.startsWith('「') && part.endsWith('」')) return <strong key={idx} className={strong}>{part}</strong>
    if (part.startsWith('**') && part.endsWith('**')) return <strong key={idx} className={strong}>{part.slice(2, -2)}</strong>
    return part
  })
}

interface Props {
  title: string
  /** 空行分段，「」与 `**…**` 中的内容加粗；后台服务的消息中用英文引号括起的路径换成「」 */
  message: string
  /** 消息下方的补充内容，如路径框 */
  children?: ReactNode
  confirmText: string
  /** 危险操作的确认按钮为红色 */
  danger?: boolean
  /** 只有确认按钮（用作提示框） */
  hideCancel?: boolean
  /** 取消与确认之间的另一个选择 */
  extra?: { label: string; onClick: () => void }
  /** 执行中的状态文字：按钮禁用，Esc 与点遮罩不再取消 */
  busy?: string
  onConfirm: () => void
  onCancel: () => void
}

/**
 * 确认对话框，玻璃质感面板与遮罩；Esc 或点遮罩取消。
 * 挂到 body 上：放在设置弹窗里时，弹窗的 transform 动画会让 fixed 定位以弹窗为准，遮罩盖不满窗口
 */
export default function ConfirmDialog({ title, message, children, confirmText, danger, hideCancel, extra, busy, onConfirm, onCancel }: Props) {
  useEscape(() => {
    if (!busy) onCancel()
  })

  return createPortal(
    <div className="fixed inset-0 z-[110] flex items-center justify-center modal-pad" onClick={() => !busy && onCancel()}>
      <div className="absolute inset-0 animate-overlay-in bg-black/20 backdrop-blur-md dark:bg-black/40" />
      <div
        role="dialog"
        aria-modal="true"
        className={`relative z-10 flex max-h-full w-full ${children ? 'max-w-md' : 'max-w-sm'} animate-confirm-in flex-col rounded-3xl border border-white/50 bg-white/90 p-6 shadow-[0_8px_40px_rgb(0,0,0,0.12)] ring-1 ring-black/5 backdrop-blur-xl dark:border-white/[0.08] dark:bg-gray-900/90 dark:shadow-[0_8px_40px_rgb(0,0,0,0.4)] dark:ring-white/10`}
        onClick={(e) => e.stopPropagation()}
      >
        <h3 className="mb-2 shrink-0 text-base font-bold text-gray-800 dark:text-gray-100">{title}</h3>
        {/* 空行分段；text-pretty 避免末行只剩一两个字。消息过长时在这里滚动，标题和按钮始终可见 */}
        <div data-selectable-text className={`${children ? 'mb-3' : 'mb-6'} -mx-2 min-h-0 overflow-y-auto overscroll-contain px-2 space-y-2 whitespace-pre-line text-pretty text-sm leading-relaxed text-gray-500 dark:text-gray-400`}>
          {message.replace(/"([^"]+)"/g, '「$1」').split(/\n{2,}/).map((para, idx) => <p key={idx}>{renderMessage(para)}</p>)}
        </div>
        {/* 内容过长时只滚动这一区域，标题和按钮始终可见 */}
        {children && <div className="-mx-2 mb-6 max-h-[min(60vh,32rem)] min-h-0 overflow-y-auto overscroll-contain px-2">{children}</div>}
        <div className="flex shrink-0 gap-2">
          {!hideCancel && (
            <button
              type="button"
              disabled={Boolean(busy)}
              onClick={onCancel}
              className="flex-1 rounded-xl border border-transparent bg-black/[0.04] py-2 text-sm text-gray-600 transition hover:bg-black/[0.07] disabled:cursor-not-allowed disabled:opacity-60 dark:bg-white/[0.06] dark:text-gray-400 dark:hover:bg-white/[0.1]"
            >
              取消
            </button>
          )}
          {extra && (
            <button
              type="button"
              disabled={Boolean(busy)}
              onClick={extra.onClick}
              className="flex-1 rounded-xl border border-transparent bg-black/[0.04] py-2 text-sm text-gray-600 transition hover:bg-black/[0.07] disabled:cursor-not-allowed disabled:opacity-60 dark:bg-white/[0.06] dark:text-gray-400 dark:hover:bg-white/[0.1]"
            >
              {extra.label}
            </button>
          )}
          <button
            type="button"
            autoFocus
            disabled={Boolean(busy)}
            onClick={onConfirm}
            className={`flex-1 rounded-xl py-2 text-sm font-medium text-white transition disabled:cursor-not-allowed disabled:opacity-60 ${danger ? 'bg-red-500 hover:bg-red-600' : 'bg-blue-500 hover:bg-blue-600'}`}
          >
            {busy || confirmText}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  )
}
