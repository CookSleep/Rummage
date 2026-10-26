import { useRef, useState, type CSSProperties, type ReactNode } from 'react'
import { useClickOutside } from '../hooks/useClickOutside'
import { useMenuPlacement } from '../hooks/useMenuPlacement'
import { ChevronDownIcon } from './icons'
import { HoverTip } from './Tip'

interface Option<T> {
  label: string
  value: T
  /** 选项文字的样式，如用对应字体渲染字体名 */
  style?: CSSProperties
  /** 选中时触发器上显示的文字，默认同 `label` */
  display?: string
  /** 选项右侧的图标按钮；点击后收起菜单 */
  action?: { label: string; icon: ReactNode; onClick: () => void }
}

interface Props<T> {
  value: T
  onChange: (value: T) => void
  options: Option<T>[]
  disabled?: boolean
  className?: string
  /** 触发器的无障碍名称 */
  label?: string
  /** 触发器的说明气泡，如不可用的原因；悬停或触屏长按时显示 */
  tip?: string
}

export const SELECT_CLASS =
  'w-full rounded-xl border border-transparent bg-black/[0.04] px-3 py-1.5 text-xs text-gray-700 outline-none transition-all duration-200 hover:bg-black/[0.07] focus-visible:ring-2 focus-visible:ring-blue-500/20 dark:bg-white/[0.06] dark:text-gray-200 dark:hover:bg-white/[0.1]'

/**
 * 下拉选择：点外面收起；下方空间不够时向上展开（`useMenuPlacement`）。
 * 宽度按最长的选项自适应：所有选项叠在触发器的同一格中，只显示选中的一项；外层给了宽度时填满外层。
 * 键盘：Tab 聚焦，Enter 或空格展开；展开后上下键移动、Enter 或空格选定，Esc 或 Tab 收起
 */
export default function Select<T extends string | number>({ value, onChange, options, disabled, className = SELECT_CLASS, label, tip }: Props<T>) {
  const [open, setOpen] = useState(false)
  // 键盘移动到的选项
  const [active, setActive] = useState(-1)
  const containerRef = useRef<HTMLDivElement>(null)
  const triggerRef = useRef<HTMLDivElement>(null)
  const selected = options.find((o) => o.value === value)
  const { maxHeight, placement } = useMenuPlacement(open, triggerRef)

  useClickOutside(containerRef, open, () => setOpen(false))

  return (
    <div ref={containerRef} className="relative w-full">
      <div
        ref={triggerRef}
        role="button"
        tabIndex={disabled ? -1 : 0}
        aria-label={label}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-disabled={disabled || undefined}
        onClick={(e) => {
          if (disabled) return
          e.preventDefault()
          e.stopPropagation()
          if (!open) setActive(options.findIndex((o) => o.value === value))
          setOpen(!open)
        }}
        onKeyDown={(e) => {
          if (disabled) return
          // 处理过的按键不再交给外层（搜索页的全局快捷键、设置弹窗的 Esc）
          const handled = () => {
            e.preventDefault()
            e.stopPropagation()
          }
          if (e.key === 'Enter' || e.key === ' ') {
            handled()
            if (open && options[active]) onChange(options[active].value)
            if (!open) setActive(options.findIndex((o) => o.value === value))
            setOpen(!open)
            return
          }
          if (!open) return
          if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
            handled()
            const step = e.key === 'ArrowDown' ? 1 : -1
            // 还没有选中项时，下键从第一项、上键从最后一项开始
            setActive((a) => (a < 0 ? (step > 0 ? 0 : options.length - 1) : (a + step + options.length) % options.length))
          } else if (e.key === 'Escape') {
            handled()
            setOpen(false)
          } else if (e.key === 'Tab') {
            setOpen(false)
          }
        }}
        className={`flex w-full cursor-pointer select-none items-center justify-between gap-1 ${className} ${disabled ? '!cursor-not-allowed !bg-gray-100/50 !opacity-50 dark:!bg-white/[0.05]' : ''}`}
      >
        <span className="grid min-w-0">
          {options.map((o) => (
            <span key={o.value} aria-hidden={o.value !== value} className={`col-start-1 row-start-1 truncate ${o.value === value ? '' : 'invisible'}`} style={o.style}>
              {o.display ?? o.label}
            </span>
          ))}
          {!selected && <span className="col-start-1 row-start-1 truncate">{value}</span>}
        </span>
        <ChevronDownIcon className={`h-3.5 w-3.5 flex-shrink-0 text-gray-400 transition-transform duration-200 dark:text-gray-500 ${open ? 'rotate-180' : ''}`} />
        {tip && <HoverTip label={tip} hide={open} longPress />}
      </div>

      {open && (
        <div
          role="listbox"
          className={`absolute z-50 w-full overflow-y-auto rounded-xl border border-transparent bg-white/95 shadow-[0_8px_30px_rgb(0,0,0,0.12)] ring-1 ring-black/5 backdrop-blur-xl dark:bg-gray-800/95 dark:shadow-[0_8px_30px_rgb(0,0,0,0.4)] dark:ring-white/[0.06] ${placement === 'top' ? 'bottom-full mb-1.5 animate-dropdown-up' : 'top-full mt-1.5 animate-dropdown-down'}`}
          style={{ maxHeight }}
        >
          {options.map((o, i) => (
            <div
              key={o.value}
              role="option"
              aria-selected={o.value === value}
              onClick={(e) => {
                e.preventDefault()
                onChange(o.value)
                setOpen(false)
              }}
              onMouseMove={() => setActive(i)}
              // 选项多到出现滚动条时，键盘移到的选项保持可见
              ref={i === active ? (el) => el?.scrollIntoView({ block: 'nearest' }) : undefined}
              className={`flex min-h-9 cursor-pointer items-center justify-between gap-2 px-3 py-2 text-xs transition-colors ${o.value === value ? 'bg-black/[0.05] font-medium text-blue-600 dark:bg-white/[0.08] dark:text-blue-400' : `text-gray-700 hover:bg-gray-50 dark:text-gray-300 dark:hover:bg-white/[0.06] ${i === active ? 'bg-gray-50 dark:bg-white/[0.06]' : ''}`}`}
            >
              <span className="min-w-0 truncate" style={o.style}>{o.label}</span>
              {o.action && (
                <button
                  type="button"
                  aria-label={o.action.label}
                  onClick={(e) => {
                    e.preventDefault()
                    e.stopPropagation()
                    o.action?.onClick()
                    setOpen(false)
                  }}
                  className="relative -my-1 -mr-1.5 flex h-6 w-6 flex-shrink-0 items-center justify-center rounded-md text-gray-400 transition hover:bg-black/[0.06] hover:text-gray-600 dark:text-gray-500 dark:hover:bg-white/[0.08] dark:hover:text-gray-200"
                >
                  {o.action.icon}
                  <HoverTip label={o.action.label} />
                </button>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  )
}
