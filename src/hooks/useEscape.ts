import { useEffect, useRef } from 'react'
import { useBackLayer } from './useBackLayer'

/** 正在处理 Esc 的各层，后打开的在后 */
const layers: { current: () => void }[] = []

/** 只有一个监听：同一次按键只交给按下时最上层的那一个 */
function onKey(e: KeyboardEvent) {
  if (e.key !== 'Escape' || !layers.length) return
  e.preventDefault()
  e.stopPropagation()
  layers[layers.length - 1].current()
}

/**
 * Esc 关闭弹窗或浮层：在捕获阶段处理并阻止传播，只关闭最上层（最后打开）的这一个，不连带关闭下面的弹窗、设置或清空搜索。
 * Android 的返回键同样关闭它（`useBackLayer`）。`enabled` 为否时不处理（如上面又打开了确认框，由它处理）
 */
export function useEscape(onClose: () => void, enabled = true) {
  const ref = useRef(onClose)
  ref.current = onClose
  useBackLayer(() => ref.current(), enabled)

  useEffect(() => {
    if (!enabled) return
    if (!layers.length) window.addEventListener('keydown', onKey, true)
    layers.push(ref)
    return () => {
      layers.splice(layers.indexOf(ref), 1)
      if (!layers.length) window.removeEventListener('keydown', onKey, true)
    }
  }, [enabled])
}
