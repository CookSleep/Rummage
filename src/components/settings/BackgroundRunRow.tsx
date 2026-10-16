import { useCallback, useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { errorMessage } from '../../lib/errors'
import { showToast } from '../Toast'
import { HEADER_BUTTON, ROW_DESC, ROW_LABEL } from './styles'

/**
 * Android“资源”页顶部的“后台运行”：是否已不受电池优化限制，没有时可请用户允许。
 * 打开时读取一次，应用回到前台时再读一次（用户可能在系统设置中改过）；还没读到时按钮不可点
 */
export default function BackgroundRunRow() {
  const [granted, setGranted] = useState<boolean | null>(null)

  const refresh = useCallback(() => {
    invoke<boolean>('battery_unrestricted')
      .then(setGranted)
      .catch((err) => {
        // 按未允许处理，“允许”兼作重试
        console.warn('无法读取电池优化设置：', err)
        setGranted(false)
      })
  }, [])

  useEffect(() => {
    refresh()
    const onVisible = () => {
      if (document.visibilityState === 'visible') refresh()
    }
    document.addEventListener('visibilitychange', onVisible)
    return () => document.removeEventListener('visibilitychange', onVisible)
  }, [refresh])

  const request = () => {
    invoke<boolean>('request_battery_unrestricted')
      .then(setGranted)
      .catch((err) => {
        console.warn('无法打开电池优化设置：', err)
        showToast(errorMessage(err, '无法打开电池优化设置'), 'error')
      })
  }

  return (
    <div>
      <div className="mb-1 flex items-center justify-between gap-3">
        <span className={`min-w-0 truncate ${ROW_LABEL}`}>后台运行</span>
        <button type="button" disabled={granted !== false} onClick={request} className={HEADER_BUTTON}>
          {granted ? '已允许' : '允许'}
        </button>
      </div>
      <div data-selectable-text className={ROW_DESC}>允许 Rummage 在后台不受电池优化限制，建立索引不会被系统中断。</div>
      <div data-selectable-text className={`mt-1 ${ROW_DESC}`}>部分系统还需在系统的电池设置中允许 Rummage 后台高耗电。</div>
    </div>
  )
}
