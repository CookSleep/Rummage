import { useEffect, useRef, useState, type ReactNode } from 'react'
import { createPortal } from 'react-dom'
import { onDismissTooltips } from '../lib/tooltipDismiss'

interface Props {
  visible: boolean
  children: ReactNode
  className?: string
}

/**
 * 说明气泡：挂在父元素上，放在父元素上方（空间不够时放下方），不超出视口；
 * 父元素被弹窗挡住时不显示，点到别处或打开弹窗（`dismissAllTooltips`）时收起，直到下一次显示
 */
export default function ViewportTooltip({ visible, children, className = '' }: Props) {
  const anchorRef = useRef<HTMLSpanElement>(null)
  const tooltipRef = useRef<HTMLDivElement>(null)
  const [position, setPosition] = useState<{ left: number; top: number; arrowLeft: number; placement: 'top' | 'bottom' } | null>(null)
  const [suppressed, setSuppressed] = useState(false)

  useEffect(() => {
    if (!visible) {
      setSuppressed(false)
      return
    }
    return onDismissTooltips(() => setSuppressed(true))
  }, [visible])

  const shown = visible && !suppressed

  useEffect(() => {
    if (!shown) return
    const hideIfOutside = (e: PointerEvent | TouchEvent) => {
      const target = e.target
      if (!(target instanceof Node)) return
      if (anchorRef.current?.parentElement?.contains(target) || tooltipRef.current?.contains(target)) return
      setSuppressed(true)
    }
    document.addEventListener('pointerdown', hideIfOutside, true)
    document.addEventListener('touchstart', hideIfOutside, true)
    return () => {
      document.removeEventListener('pointerdown', hideIfOutside, true)
      document.removeEventListener('touchstart', hideIfOutside, true)
    }
  }, [shown])

  useEffect(() => {
    if (!shown) {
      setPosition(null)
      return
    }
    const update = () => {
      const anchor = anchorRef.current?.parentElement
      const el = tooltipRef.current
      if (!anchor || !el) return
      const margin = 8
      const gap = 8
      const rect = anchor.getBoundingClientRect()
      // 父元素不可见，或中心点被弹窗等挡住
      const x = rect.left + rect.width / 2
      const y = rect.top + rect.height / 2
      const hit = document.elementFromPoint(x, y)
      if (!anchor.getClientRects().length || !hit || !anchor.contains(hit)) {
        setPosition(null)
        return
      }
      const tip = el.getBoundingClientRect()
      const maxLeft = Math.max(margin, window.innerWidth - tip.width - margin)
      const left = Math.min(Math.max(x - tip.width / 2, margin), maxLeft)
      const above = rect.top - tip.height - gap
      const placement = above >= margin ? 'top' : 'bottom'
      setPosition({ left, top: placement === 'top' ? above : rect.bottom + gap, arrowLeft: x - left, placement })
    }
    update()
    window.addEventListener('resize', update)
    window.addEventListener('scroll', update, true)
    return () => {
      window.removeEventListener('resize', update)
      window.removeEventListener('scroll', update, true)
    }
  }, [shown, children])

  return (
    <>
      <span ref={anchorRef} className="hidden" aria-hidden />
      {shown && createPortal(
        <div
          ref={tooltipRef}
          className={`pointer-events-none fixed rounded-lg bg-gray-800 px-3 py-2 text-xs font-normal text-white shadow-lg ${className}`}
          style={{ left: position?.left ?? 0, top: position?.top ?? 0, visibility: position ? 'visible' : 'hidden', zIndex: 120 }}
        >
          {children}
          <div
            className={`absolute left-0 border-4 border-transparent ${position?.placement === 'bottom' ? 'bottom-full border-b-gray-800' : 'top-full border-t-gray-800'}`}
            style={{ left: position?.arrowLeft ?? 0, transform: 'translateX(-50%)' }}
          />
        </div>,
        document.body,
      )}
    </>
  )
}
