import { useCallback, useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { errorMessage } from '../lib/errors'
import { isAndroid } from '../lib/platform'
import { showToast } from '../components/Toast'

/**
 * Android 的“所有文件访问”权限：读取一次，应用回到前台时再读一次（用户可能在系统设置中改过）。
 * 还没读到时为空；桌面上视为已授权。`request` 打开系统的授权页面，用户返回后更新
 */
export function useStorageAccess() {
  const [granted, setGranted] = useState<boolean | null>(isAndroid ? null : true)

  const refresh = useCallback(() => {
    invoke<boolean>('storage_access')
      .then(setGranted)
      .catch((err) => {
        // 按未授权处理，显示授权卡片，“前往授权”兼作重试
        console.warn('无法读取存储权限：', err)
        setGranted(false)
      })
  }, [])

  useEffect(() => {
    if (!isAndroid) return
    refresh()
    const onVisible = () => {
      if (document.visibilityState === 'visible') refresh()
    }
    document.addEventListener('visibilitychange', onVisible)
    return () => document.removeEventListener('visibilitychange', onVisible)
  }, [refresh])

  const request = () => {
    invoke<boolean>('request_storage_access')
      .then(setGranted)
      .catch((err) => {
        console.warn('无法打开授权页面：', err)
        showToast(errorMessage(err, '无法打开授权页面'), 'error')
      })
  }

  return { granted, request }
}
