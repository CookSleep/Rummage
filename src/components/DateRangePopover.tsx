import { useRef, useState } from 'react'
import { useClickOutside } from '../hooks/useClickOutside'
import { useEscape } from '../hooks/useEscape'
import { isReversed } from '../lib/filter'
import { SMALL_PLAIN_BUTTON, SMALL_PRIMARY_BUTTON } from './onboarding/styles'
import ViewportTooltip from './ViewportTooltip'

interface Props {
  /** 已设的起止日期（`YYYY-MM-DD`，空字符串表示不限） */
  from: string
  to: string
  onApply: (from: string, to: string) => void
  onClose: () => void
}

const INPUT_CLASS =
  'w-full rounded-lg border border-transparent bg-black/[0.04] px-2.5 py-1.5 text-xs text-gray-700 outline-none transition [color-scheme:light] focus-visible:ring-2 focus-visible:ring-blue-500/20 dark:bg-white/[0.06] dark:text-gray-200 dark:[color-scheme:dark]'

/**
 * 修改时间“自定义”的弹出层：选择起止日期，可只填一端；起点晚于终点时气泡提示，不能确定。
 * 日期用原生 `<input type="date">`：WebView2 与 Android WebView 自带日历，
 * 按 `color-scheme` 跟随深浅色，不必自己画日历。点外面或 Esc 收起
 */
export default function DateRangePopover({ from, to, onApply, onClose }: Props) {
  const [start, setStart] = useState(from)
  const [end, setEnd] = useState(to)
  const ref = useRef<HTMLDivElement>(null)
  const reversed = isReversed(start, end)

  useClickOutside(ref, true, onClose)

  useEscape(onClose)

  return (
    <div
      ref={ref}
      role="dialog"
      aria-label="自定义修改时间"
      className="absolute right-0 top-full z-50 mt-1.5 w-56 animate-dropdown-down rounded-xl border border-transparent bg-white/95 p-3 shadow-[0_8px_30px_rgb(0,0,0,0.12)] ring-1 ring-black/5 backdrop-blur-xl dark:bg-gray-800/95 dark:shadow-[0_8px_30px_rgb(0,0,0,0.4)] dark:ring-white/[0.06]"
    >
      <label className="block text-xs text-gray-500 dark:text-gray-400">
        开始日期
        <input type="date" value={start} max={end || undefined} onChange={(e) => setStart(e.target.value)} className={`mt-1 ${INPUT_CLASS}`} />
      </label>
      <label className="mt-2 block text-xs text-gray-500 dark:text-gray-400">
        结束日期
        <span className="relative mt-1 block">
          <input type="date" value={end} min={start || undefined} onChange={(e) => setEnd(e.target.value)} className={INPUT_CLASS} />
          <ViewportTooltip visible={reversed} className="whitespace-nowrap">结束日期不能早于开始日期</ViewportTooltip>
        </span>
      </label>
      <div className="mt-3 flex gap-2">
        <button type="button" onClick={onClose} className={`flex-1 ${SMALL_PLAIN_BUTTON}`}>
          取消
        </button>
        <button type="button" disabled={reversed || (!start && !end)} onClick={() => onApply(start, end)} className={`flex-1 ${SMALL_PRIMARY_BUTTON}`}>
          确定
        </button>
      </div>
    </div>
  )
}
