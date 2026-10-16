import { useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { ParamValues } from '../types'
import { isTauri } from '../lib/platform'
import { useCoreEvent } from './useCoreEvent'

/**
 * 参数的当前值：核心连上（`connected` 变为 true）时用 `get_params` 取一次，之后跟随 `core-event` 中的 `ParamsChanged`。
 * 取到之前为空
 */
export function useParamValues(connected: boolean) {
  const [values, setValues] = useState<ParamValues | null>(null)

  useCoreEvent((ev) => {
    if (ev.type === 'ParamsChanged') setValues(ev.values)
  })

  useEffect(() => {
    if (!isTauri || !connected) return
    let active = true
    invoke<ParamValues>('get_params')
      .then((v) => {
        if (active) setValues(v)
      })
      .catch((err) => console.warn('无法读取设置：', err))
    return () => {
      active = false
    }
  }, [connected])

  return values
}
