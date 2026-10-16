import { useCallback, useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { SetupStatus } from '../types'
import { errorMessage } from '../lib/errors'
import { isTauri } from '../lib/platform'
import { useCoreEvent } from './useCoreEvent'

/**
 * 引导状态：核心连上时用 `setup_status` 取一次，之后跟随 `core-event` 中的 `ModelTask`、`Selftest` 更新对应部分。
 * 第一次连上之前为空；断开后保留上次的值（App 据此决定显示引导还是搜索页，置空会卸载当前页面）。模型任务结束后再取一次，更新模型文件是否可用。`error` 为读取失败的原因（如索引数据库不可用），没有时为空串
 */
export function useSetup(connected: boolean) {
  const [status, setStatus] = useState<SetupStatus | null>(null)
  const [error, setError] = useState('')

  const refresh = useCallback(() => {
    invoke<SetupStatus>('setup_status')
      .then((s) => {
        setStatus(s)
        setError('')
      })
      .catch((err) => {
        console.warn('无法获取引导状态：', err)
        setError(errorMessage(err, '无法读取设备与模型的状态，请重试。'))
      })
  }, [])

  useCoreEvent((ev) => {
    if (ev.type === 'ModelTask') {
      const { type: _, ...task } = ev
      setStatus((s) => s && { ...s, task })
      if (task.state !== 'running') refresh()
      return
    }
    if (ev.type === 'Selftest') {
      const { type: _, ...selftest } = ev
      setStatus((s) => s && { ...s, selftest })
    }
  })

  useEffect(() => {
    if (isTauri && connected) refresh()
  }, [connected, refresh])

  // 还没取到时不改
  const update = useCallback((fn: (s: SetupStatus) => SetupStatus) => setStatus((s) => s && fn(s)), [])

  return { status, error, update, refresh }
}
