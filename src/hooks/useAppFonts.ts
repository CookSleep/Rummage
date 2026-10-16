import { useCallback, useEffect, useLayoutEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { Appearance } from '../types'
import { applyAppearance, loadCachedAppearance, saveCachedAppearance } from '../lib/fonts'
import { isTauri } from '../lib/platform'

/**
 * 外观（字体）：先用本地缓存，首次绘制前就应用；核心连上后取 `get_appearance`，以核心为准并更新缓存。
 * `update` 立即应用并写入缓存与核心，保存失败时抛出，由设置页显示原因
 */
export function useAppFonts(connected: boolean) {
  const [appearance, setAppearance] = useState(loadCachedAppearance)

  // 用 layout effect 在首次绘制前应用字体，避免先用默认字体再切换
  useLayoutEffect(() => applyAppearance(appearance), [appearance])

  useEffect(() => {
    if (!isTauri || !connected) return
    invoke<Appearance>('get_appearance')
      .then((a) => {
        setAppearance(a)
        saveCachedAppearance(a)
      })
      .catch((err) => console.warn('无法读取外观设置：', err))
  }, [connected])

  const update = useCallback(async (next: Appearance) => {
    setAppearance(next)
    saveCachedAppearance(next)
    await invoke('set_appearance', { appearance: next })
  }, [])

  return { appearance, update }
}
