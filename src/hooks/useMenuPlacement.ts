import { useEffect, useState, type RefObject } from 'react'

/** 下拉菜单的最大高度 */
const MENU_MAX_HEIGHT = 240

/**
 * 下拉菜单展开的方向与最大高度（Select、MultiSelect 共用）：下方空间不够（含滚动容器的边界）时向上展开
 */
export function useMenuPlacement(open: boolean, triggerRef: RefObject<HTMLElement | null>) {
  const [maxHeight, setMaxHeight] = useState(MENU_MAX_HEIGHT)
  const [placement, setPlacement] = useState<'bottom' | 'top'>('bottom')

  useEffect(() => {
    if (!open) return
    const update = () => {
      const trigger = triggerRef.current
      if (!trigger) return
      const rect = trigger.getBoundingClientRect()
      let below = window.innerHeight - rect.bottom - 8
      let above = rect.top - 8
      // 设置页的内容区可滚动，菜单不能超出它
      for (let el = trigger.parentElement; el && el !== document.body; el = el.parentElement) {
        const style = window.getComputedStyle(el)
        if (!/(auto|scroll|hidden|clip)/.test(`${style.overflow} ${style.overflowY}`)) continue
        const box = el.getBoundingClientRect()
        below = Math.min(below, box.bottom - rect.bottom - 8)
        above = Math.min(above, rect.top - box.top - 8)
      }
      const up = below < 120 && above > below
      setPlacement(up ? 'top' : 'bottom')
      setMaxHeight(Math.max(0, Math.floor(Math.min(MENU_MAX_HEIGHT, up ? above : below))))
    }
    update()
    window.addEventListener('resize', update)
    window.addEventListener('scroll', update, true)
    return () => {
      window.removeEventListener('resize', update)
      window.removeEventListener('scroll', update, true)
    }
  }, [open, triggerRef])

  return { maxHeight, placement }
}
