import { useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { CoreStatus } from '../types'
import { errorMessage } from '../lib/errors'
import { SpinnerIcon } from './icons'
import { BUTTON } from './onboarding/styles'
import { showToast } from './Toast'

interface Props {
  status: CoreStatus
  /** 外层的版面（高度、留白），文案与样式各处一致 */
  className?: string
}

/**
 * 后台服务不可用时的统一提示，居中的灰字：
 * 无法启动时说明并提供“重试”（`restart_core`，失败原因用胶囊通知）；
 * 其余都是正在启动或重新启动（断开后守护会自动拉起，状态为 `restarting`），只显示加载指示。
 * `running` 只出现在启动时“已连上、还没取到引导状态”（App），按正在启动显示
 */
export default function CoreStatusNotice({ status, className = '' }: Props) {
  const [busy, setBusy] = useState(false)

  const retry = () => {
    setBusy(true)
    invoke('restart_core')
      .catch((err) => {
        console.warn('无法重新启动后台服务：', err)
        showToast(errorMessage(err, '无法启动后台服务，请重新启动 Rummage。'), 'error')
      })
      .finally(() => setBusy(false))
  }

  if (status === 'failed') {
    return (
      <div className={`flex flex-col items-center justify-center gap-3 text-center text-sm text-gray-500 dark:text-gray-400 ${className}`}>
        <div data-selectable-text className="space-y-1">
          <p className="font-medium text-gray-700 dark:text-gray-200">后台服务无法启动</p>
          <p className="text-xs">重试仍然失败时，请附上数据目录中“logs”文件夹里的日志反馈问题。</p>
        </div>
        <button type="button" className={BUTTON} disabled={busy} onClick={retry}>
          重试
        </button>
      </div>
    )
  }
  return (
    <div className={`flex items-center justify-center gap-2 text-center text-sm text-gray-500 dark:text-gray-400 ${className}`}>
      <SpinnerIcon className="h-4 w-4 flex-shrink-0 animate-spin text-blue-500" />
      {status === 'restarting' ? '后台服务正在重新启动，请稍候' : '正在启动后台服务，请稍候'}
    </div>
  )
}
