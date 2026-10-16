import { useEffect, useState } from 'react'
import { createPortal } from 'react-dom'
import { useEscape } from '../../hooks/useEscape'
import { CloseIcon } from '../icons'

/** 第三方许可清单（npm run notices 生成）。Esc 或点遮罩关闭 */
export default function FontLicensesModal({ onClose }: { onClose: () => void }) {
  useEscape(onClose)
  const [text, setText] = useState<string | null>(null)
  const [failed, setFailed] = useState(false)

  // 清单较大，打开弹窗时才加载，不进主包
  useEffect(() => {
    import('../../assets/third-party-notices.txt?raw')
      .then((mod) => setText(mod.default))
      .catch((err) => {
        console.error('加载第三方许可清单失败', err)
        setFailed(true)
      })
  }, [])

  return createPortal(
    <div className="fixed inset-0 z-[110] flex items-center justify-center modal-pad" onClick={onClose}>
      <div className="absolute inset-0 animate-overlay-in bg-black/20 backdrop-blur-md dark:bg-black/40" />
      <div
        role="dialog"
        aria-modal="true"
        className="relative z-10 flex max-h-[85vh] w-full max-w-2xl animate-confirm-in flex-col rounded-3xl border border-white/50 bg-white/90 shadow-[0_8px_40px_rgb(0,0,0,0.12)] ring-1 ring-black/5 backdrop-blur-xl dark:border-white/[0.08] dark:bg-gray-900/90 dark:shadow-[0_8px_40px_rgb(0,0,0,0.4)] dark:ring-white/10 sm:max-h-[90vh]"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="shrink-0 p-6 pb-2">
          <div className="flex items-center justify-between gap-4">
            <h3 className="text-base font-bold text-gray-800 dark:text-gray-100">第三方许可</h3>
            <button
              type="button"
              onClick={onClose}
              className="shrink-0 rounded-full p-1 text-gray-400 transition hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-white/[0.06] dark:hover:text-gray-200"
              aria-label="关闭"
            >
              <CloseIcon className="h-5 w-5" />
            </button>
          </div>
        </div>

        <div className="flex-1 overflow-y-auto px-6 pb-6 pt-2 min-h-0">
          {text === null ? (
            <div className="py-8 text-center text-sm text-gray-500 dark:text-gray-400">{failed ? '许可清单加载失败' : '正在加载'}</div>
          ) : (
            <pre
              data-selectable-text
              className="whitespace-pre-wrap break-words rounded-xl bg-black/[0.04] p-3 font-mono text-[11px] leading-relaxed text-gray-600 dark:bg-white/[0.06] dark:text-gray-300"
            >
              {text}
            </pre>
          )}
        </div>
      </div>
    </div>,
    document.body,
  )
}
