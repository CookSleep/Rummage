import type { ReactNode } from 'react'

/** 带文字的勾选框，样式同 Playground 的 `Checkbox`，焦点环只在键盘聚焦时显示（触屏点按后不留）；点文字也能切换。`disabled` 时变淡、不能切换 */
export default function Checkbox({ checked, onChange, label, disabled }: { checked: boolean; onChange: (on: boolean) => void; label: ReactNode; disabled?: boolean }) {
  return (
    <label className={`group flex items-center gap-2.5 ${disabled ? 'cursor-not-allowed opacity-50' : 'cursor-pointer'}`}>
      <span className="relative flex items-center justify-center">
        <input
          type="checkbox"
          checked={checked}
          disabled={disabled}
          onChange={(e) => onChange(e.target.checked)}
          className="peer h-4 w-4 cursor-pointer appearance-none disabled:cursor-not-allowed rounded-[4px] border border-gray-300 bg-white transition-all hover:border-gray-400 checked:border-blue-500 checked:bg-blue-500 focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500/20 focus-visible:ring-offset-1 focus-visible:ring-offset-white dark:border-white/15 dark:bg-white/5 dark:hover:border-white/30 dark:checked:border-transparent dark:checked:bg-blue-500 dark:focus-visible:ring-offset-gray-900"
        />
        <svg
          className="pointer-events-none absolute h-2.5 w-2.5 scale-50 text-white opacity-0 transition-all duration-200 peer-checked:scale-100 peer-checked:opacity-100"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          strokeWidth={2.5}
        >
          <path strokeLinecap="round" strokeLinejoin="round" d="M4.5 12.5l3.5 3.5L19 6.5" />
        </svg>
      </span>
      <span className="select-none text-[13px] font-medium text-gray-700 transition-colors group-hover:text-gray-900 dark:text-gray-300 dark:group-hover:text-gray-100">{label}</span>
    </label>
  )
}
