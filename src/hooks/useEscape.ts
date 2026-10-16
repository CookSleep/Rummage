import { useEffect, useRef } from 'react'
import { useBackLayer } from './useBackLayer'

/**
 * Esc 关闭弹窗或浮层：在捕获阶段处理并阻止传播，只关闭最上层的这一个，不连带关闭下面的设置或清空搜索。
 * Android 的返回键同样关闭它（`useBackLayer`）。`enabled` 为否时不处理（如上面又打开了确认框，由它处理）
 */
export function useEscape(onClose: () => void, enabled = true) {
  const ref = useRef(onClose)
  ref.current = onClose
  useBackLayer(() => ref.current(), enabled)

  useEffect(() => {
    if (!enabled) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return
      e.preventDefault()
      e.stopPropagation()
      ref.current()
    }
    window.addEventListener('keydown', onKey, true)
    return () => window.removeEventListener('keydown', onKey, true)
  }, [enabled])
}
