import { useCallback, useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { errorMessage } from '../../lib/errors'
import { renderMessage } from '../ConfirmDialog'
import { showToast } from '../Toast'
import { HEADER_BUTTON, ROW_DESC, ROW_LABEL } from './styles'

/**
 * Android“资源”页顶部的“后台运行”：是否已不受电池优化限制，没有时可请用户允许。
 * 打开时读取一次，应用回到前台时再读一次（用户可能在系统设置中改过）；还没读到时按钮不可点。
 * 部分系统另有自己的后台管控，白名单挡不住，读不到它的状态，只能说明并打开应用信息页由用户放行；引导的最后一步也显示
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

  const openSettings = () => {
    invoke('open_app_settings').catch((err) => {
      console.warn('无法打开应用信息：', err)
      showToast(errorMessage(err, '无法打开应用信息'), 'error')
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
      <div className="mb-1 mt-4 flex items-center justify-between gap-3">
        <span className={`min-w-0 truncate ${ROW_LABEL}`}>系统的后台管控</span>
        <button type="button" onClick={openSettings} className={HEADER_BUTTON}>
          打开应用信息
        </button>
      </div>
      <div data-selectable-text className={`space-y-1 ${ROW_DESC}`}>
        <p>部分手机系统会在 Rummage 退到后台几秒后暂停它，不受上面的设置影响。</p>
        <p>{renderMessage('如果锁屏或切到其他应用后进度不动，请在应用信息的「电量」或「电池」中允许 Rummage 在后台运行。', 'font-semibold')}</p>
        <p>各系统的叫法不同，如“允许后台耗电”“无限制”。</p>
      </div>
    </div>
  )
}
