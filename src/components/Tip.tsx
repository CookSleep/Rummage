import { useEffect, useRef, useState, type ReactNode } from 'react'
import ViewportTooltip from './ViewportTooltip'

/** 悬停或聚焦时在上方显示文字说明的包装，用于只有图标的状态、标签 */
export default function Tip({ label, children, className = '' }: { label: string; children: ReactNode; className?: string }) {
  const [visible, setVisible] = useState(false)
  return (
    <span
      tabIndex={0}
      aria-label={label}
      onMouseEnter={() => setVisible(true)}
      onMouseLeave={() => setVisible(false)}
      onFocus={() => setVisible(true)}
      onBlur={() => setVisible(false)}
      className={`relative inline-flex items-center outline-none ${className}`}
    >
      {children}
      <ViewportTooltip visible={visible} className="whitespace-nowrap">{label}</ViewportTooltip>
    </span>
  )
}

/** 触屏长按多久显示气泡（毫秒） */
const LONG_PRESS_MS = 500

/**
 * 放在按钮、截断文字等元素内部，悬停或键盘聚焦该元素时显示说明气泡；不包一层元素，不影响定位与布局。
 * wrap 用于长路径，气泡可换行；hide 用于菜单展开等时暂不显示；longPress 时触屏长按也显示，
 * 长按松手后不再触发该元素的点击
 */
export function HoverTip({ label, wrap, hide, longPress }: { label: string; wrap?: boolean; hide?: boolean; longPress?: boolean }) {
  const ref = useRef<HTMLSpanElement>(null)
  const [visible, setVisible] = useState(false)

  useEffect(() => {
    const el = ref.current?.parentElement
    if (!el) return
    const show = () => setVisible(true)
    const hide = () => setVisible(false)
    // 触屏轻点时浏览器会补发 mouseenter，而 mouseleave 要等点到别处才有；轻点后的 click 收起气泡，长按没有 click，气泡照常留着
    let touch = false
    const down = (e: PointerEvent) => {
      touch = e.pointerType === 'touch'
    }
    const tapped = () => touch && hide()
    // 鼠标点击也会聚焦按钮，只在键盘聚焦时显示
    const focus = () => el.matches(':focus-visible') && show()
    el.addEventListener('pointerdown', down)
    el.addEventListener('click', tapped)
    el.addEventListener('mouseenter', show)
    el.addEventListener('mouseleave', hide)
    el.addEventListener('focus', focus)
    el.addEventListener('blur', hide)
    let timer: number | undefined
    let pressed = false
    const clear = () => window.clearTimeout(timer)
    const start = () => {
      clear()
      pressed = false
      timer = window.setTimeout(() => {
        pressed = true
        show()
      }, LONG_PRESS_MS)
    }
    const end = (e: TouchEvent) => {
      clear()
      if (pressed && e.cancelable) e.preventDefault()
      pressed = false
    }
    if (longPress) {
      el.addEventListener('touchstart', start, { passive: true })
      el.addEventListener('touchmove', clear, { passive: true })
      el.addEventListener('touchend', end)
      el.addEventListener('touchcancel', clear)
    }
    return () => {
      clear()
      el.removeEventListener('pointerdown', down)
      el.removeEventListener('click', tapped)
      el.removeEventListener('mouseenter', show)
      el.removeEventListener('mouseleave', hide)
      el.removeEventListener('focus', focus)
      el.removeEventListener('blur', hide)
      el.removeEventListener('touchstart', start)
      el.removeEventListener('touchmove', clear)
      el.removeEventListener('touchend', end)
      el.removeEventListener('touchcancel', clear)
    }
  }, [longPress])

  return (
    <>
      <span ref={ref} className="hidden" aria-hidden />
      <ViewportTooltip visible={visible && !hide} className={wrap ? 'max-w-[min(36rem,calc(100vw-16px))] whitespace-normal break-all' : 'whitespace-nowrap'}>{label}</ViewportTooltip>
    </>
  )
}
