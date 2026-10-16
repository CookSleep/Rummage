import { useEffect, useRef } from 'react'
import { listen } from '@tauri-apps/api/event'
import type { CoreEvent } from '../types'
import { isTauri } from '../lib/platform'

/** 订阅后台服务的 `core-event`：只订阅一次，事件到达时调用最新一次渲染传入的 `onEvent` */
export function useCoreEvent(onEvent: (ev: CoreEvent) => void) {
  const ref = useRef(onEvent)
  ref.current = onEvent

  useEffect(() => {
    if (!isTauri) return
    const unlisten = listen<CoreEvent>('core-event', (e) => ref.current(e.payload))
    return () => {
      void unlisten.then((fn) => fn())
    }
  }, [])
}
