import { useEffect, useRef, type RefObject } from 'react'

/** 弹出层（下拉菜单、暂停菜单、日期范围）打开时，在 `ref` 之外按下鼠标就调用 `onOutside`，通常是收起 */
export function useClickOutside(ref: RefObject<HTMLElement | null>, open: boolean, onOutside: () => void) {
  // 回调每次渲染都是新的，存起来，免得反复重新监听
  const cb = useRef(onOutside)
  cb.current = onOutside

  useEffect(() => {
    if (!open) return
    const onDown = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) cb.current()
    }
    document.addEventListener('mousedown', onDown)
    return () => document.removeEventListener('mousedown', onDown)
  }, [open, ref])
}
