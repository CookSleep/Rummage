import { useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { StartupNotice } from '../types'
import { isTauri } from '../lib/platform'
import ConfirmDialog from './ConfirmDialog'
import PathBox from './PathBox'
import { showToast } from './Toast'

/**
 * 启动时确定数据目录的结果，前端加载后取一次。
 * 成功的以胶囊通知显示，其余（迁移失败、上次未完成、改用默认目录等）逐条弹窗说明
 */
export default function DataDirNotices() {
  const [queue, setQueue] = useState<StartupNotice[]>([])

  useEffect(() => {
    if (!isTauri) return
    invoke<StartupNotice[]>('data_dir_notices')
      .then((list) => {
        const ok = list.find((n) => n.success)
        if (ok) showToast(ok.message, 'success')
        // 开发时 StrictMode 会调用两次，第二次取到的为空，追加而不是覆盖
        setQueue((q) => [...q, ...list.filter((n) => !n.success)])
      })
      .catch((err) => console.warn('无法读取数据目录提示：', err))
  }, [])

  const cur = queue[0]
  const next = () => setQueue((q) => q.slice(1))

  return (
    <>
      {cur && (
        <ConfirmDialog title={cur.title} message={cur.message} confirmText="知道了" hideCancel onConfirm={next} onCancel={next}>
          {(cur.paths.length > 0 || cur.details.length > 0) && (
            <div className="space-y-3">
              {cur.paths.length > 0 && <PathBox rows={cur.paths} />}
              {cur.details.map((d) => (
                <p key={d} data-selectable-text className="text-xs leading-relaxed text-gray-500 dark:text-gray-400">{d}</p>
              ))}
            </div>
          )}
        </ConfirmDialog>
      )}
    </>
  )
}
