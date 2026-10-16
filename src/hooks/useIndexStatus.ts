import { useEffect, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { IndexStatus } from '../types'
import { isTauri } from '../lib/platform'
import { useCoreEvent } from './useCoreEvent'

/**
 * 索引进度：核心连上（`connected` 变为 true）时用 `index_status` 取一次，之后跟随 `core-event` 中的 `IndexProgress`。
 * 核心未连接或索引尚未启动时为空
 */
export function useIndexStatus(connected: boolean) {
  const [status, setStatus] = useState<IndexStatus | null>(null)
  // 发出 `index_status` 之后收到过进度事件：命令结果已经过时，以事件为准
  const gotEvent = useRef(false)

  useCoreEvent((ev) => {
    if (ev.type !== 'IndexProgress') return
    gotEvent.current = true
    setStatus(ev)
  })

  useEffect(() => {
    if (!isTauri) return
    // 断开后旧进度已不可信，不再显示
    if (!connected) {
      setStatus(null)
      return
    }
    let active = true
    gotEvent.current = false
    invoke<IndexStatus>('index_status')
      .then((s) => {
        if (active && !gotEvent.current) setStatus(s)
      })
      .catch((err) => console.warn('无法获取索引进度：', err))
    return () => {
      active = false
    }
  }, [connected])

  return status
}
