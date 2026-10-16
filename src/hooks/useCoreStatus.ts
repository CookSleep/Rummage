import { useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import type { CoreStatus } from '../types'
import { isTauri } from '../lib/platform'

/** 订阅核心进程状态：初值从 `core_status` 命令取，之后跟随 `core-status` 事件 */
export function useCoreStatus() {
  const [status, setStatus] = useState<CoreStatus>('starting')

  useEffect(() => {
    if (!isTauri) return
    let active = true
    // 事件先于命令结果到达时，以事件为准，命令结果已经过时
    let gotEvent = false
    const unlisten = listen<CoreStatus>('core-status', (e) => {
      gotEvent = true
      setStatus(e.payload)
    })
    invoke<CoreStatus>('core_status')
      .then((s) => {
        if (active && !gotEvent) setStatus(s)
      })
      .catch((err) => console.warn('无法获取核心进程状态：', err))
    return () => {
      active = false
      void unlisten.then((fn) => fn())
    }
  }, [])

  return status
}
