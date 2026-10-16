import { useEffect, useRef } from 'react'
import { pushBackLayer } from '../lib/back'

/**
 * `active` 期间登记为返回键的一层（src/lib/back.ts），按返回键时调用 `onBack`。
 * 层的先后按变为 `active` 的时间：后打开的先关闭
 */
export function useBackLayer(onBack: () => void, active = true) {
  const ref = useRef(onBack)
  ref.current = onBack

  useEffect(() => {
    if (!active) return
    return pushBackLayer(() => ref.current())
  }, [active])
}
