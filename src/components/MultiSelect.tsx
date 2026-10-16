import { useRef, useState } from 'react'
import { useClickOutside } from '../hooks/useClickOutside'
import { useMenuPlacement } from '../hooks/useMenuPlacement'
import { CheckIcon, ChevronDownIcon } from './icons'
import { SELECT_CLASS } from './Select'
import { HoverTip } from './Tip'

interface Option<T> {
  label: string
  value: T
  /** 悬停时的气泡，如文件夹的完整路径 */
  tip?: string
}

interface Props<T> {
  /** 已勾选的值，至少一项 */
  values: T[]
  onChange: (values: T[]) => void
  options: Option<T>[]
  /** 触发器的无障碍名称 */
  label: string
  /** 全部勾选时的摘要，如“全部类型” */
  allText: string
  /** 勾选多项时的量词，如“种类型”显示为“4 种类型” */
  unit: string
  /** 触发器的说明气泡，悬停或触屏长按时显示 */
  tip?: string
}

/**
 * 摘要：全部勾选时为 `allText`；勾选一两项时列出名称；只少一项时为“不含某某”；其余为“N 种类型”。
 * 选项少于 4 个时“只少一项”就是勾选一两项，按列出名称处理
 */
export function summary(labels: string[], checked: string[], allText: string, unit: string) {
  if (checked.length === labels.length) return allText
  if (checked.length <= 2) return checked.join('、')
  if (checked.length === labels.length - 1) return `不含${labels.find((l) => !checked.includes(l))}`
  return `${checked.length} ${unit}`
}

/**
 * 可多选的下拉菜单，外观同 `Select`：点选一项时切换勾选、菜单不收起，最后一个勾选的不能取消。
 * 键盘：Tab 聚焦，Enter 或空格展开；展开后上下键移动、Enter 或空格切换勾选，Esc 或 Tab 收起
 */
export default function MultiSelect<T extends string | number>({ values, onChange, options, label, allText, unit, tip }: Props<T>) {
  const [open, setOpen] = useState(false)
  // 键盘移动到的选项
  const [active, setActive] = useState(-1)
  const containerRef = useRef<HTMLDivElement>(null)
  const triggerRef = useRef<HTMLDivElement>(null)
  const { maxHeight, placement } = useMenuPlacement(open, triggerRef)
  const checkedLabels = options.filter((o) => values.includes(o.value)).map((o) => o.label)
  const text = summary(options.map((o) => o.label), checkedLabels, allText, unit)

  useClickOutside(containerRef, open, () => setOpen(false))

  // 按选项顺序给出结果，与勾选的先后无关
  const toggle = (value: T) => {
    const on = values.includes(value)
    if (on && values.length === 1) return
    onChange(options.map((o) => o.value).filter((v) => (v === value ? !on : values.includes(v))))
  }

  return (
    <div ref={containerRef} className="relative w-full">
      <div
        ref={triggerRef}
        role="button"
        tabIndex={0}
        aria-label={label}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={(e) => {
          e.preventDefault()
          e.stopPropagation()
          if (!open) setActive(-1)
          setOpen(!open)
        }}
        onKeyDown={(e) => {
          // 处理过的按键不再交给外层（搜索页的全局快捷键）
          const handled = () => {
            e.preventDefault()
            e.stopPropagation()
          }
          if (e.key === 'Enter' || e.key === ' ') {
            handled()
            if (!open) {
              setActive(0)
              setOpen(true)
              return
            }
            if (options[active]) toggle(options[active].value)
            return
          }
          if (!open) return
          if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
            handled()
            const step = e.key === 'ArrowDown' ? 1 : -1
            setActive((a) => (a < 0 ? (step > 0 ? 0 : options.length - 1) : (a + step + options.length) % options.length))
          } else if (e.key === 'Escape') {
            handled()
            setOpen(false)
          } else if (e.key === 'Tab') {
            setOpen(false)
          }
        }}
        className={`flex w-full cursor-pointer select-none items-center justify-between gap-1 ${SELECT_CLASS}`}
      >
        <span className="truncate">{text}</span>
        <ChevronDownIcon className={`h-3.5 w-3.5 flex-shrink-0 text-gray-400 transition-transform duration-200 dark:text-gray-500 ${open ? 'rotate-180' : ''}`} />
        {tip && <HoverTip label={tip} hide={open} longPress />}
      </div>

      {open && (
        <div
          role="listbox"
          aria-multiselectable
          className={`absolute z-50 w-full overflow-y-auto rounded-xl border border-transparent bg-white/95 shadow-[0_8px_30px_rgb(0,0,0,0.12)] ring-1 ring-black/5 backdrop-blur-xl dark:bg-gray-800/95 dark:shadow-[0_8px_30px_rgb(0,0,0,0.4)] dark:ring-white/[0.06] ${placement === 'top' ? 'bottom-full mb-1.5 animate-dropdown-up' : 'top-full mt-1.5 animate-dropdown-down'}`}
          style={{ maxHeight }}
        >
          {options.map((o, i) => {
            const checked = values.includes(o.value)
            const locked = checked && values.length === 1
            return (
              <div
                key={o.value}
                role="option"
                aria-selected={checked}
                aria-disabled={locked || undefined}
                // 不抢走触发器的焦点，点选后键盘仍可继续操作
                onMouseDown={(e) => e.preventDefault()}
                onClick={(e) => {
                  e.preventDefault()
                  toggle(o.value)
                }}
                onMouseMove={() => setActive(i)}
                // 选项多到出现滚动条时，键盘移到的选项保持可见
                ref={i === active ? (el) => el?.scrollIntoView({ block: 'nearest' }) : undefined}
                className={`flex min-h-9 items-center justify-between gap-2 px-3 py-2 text-xs transition-colors ${locked ? 'cursor-not-allowed' : 'cursor-pointer'} ${checked ? 'font-medium text-blue-600 dark:text-blue-400' : 'text-gray-700 dark:text-gray-300'} ${i === active ? 'bg-gray-50 dark:bg-white/[0.06]' : ''}`}
              >
                <span className="min-w-0 truncate">{o.label}</span>
                {checked && <CheckIcon className="h-3.5 w-3.5 flex-shrink-0" />}
                {o.tip && <HoverTip label={o.tip} wrap />}
              </div>
            )
          })}
        </div>
      )}
    </div>
  )
}
