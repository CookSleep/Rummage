import { useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { DataDirChange } from '../types'
import { errorMessage } from '../lib/errors'
import { formatSize } from '../lib/format'
import ConfirmDialog from './ConfirmDialog'
import PathBox from './PathBox'
import { showToast } from './Toast'

type Dialog = { type: 'blocked'; message: string; rows?: [string, string][] } | { type: 'change'; change: DataDirChange }

interface Props {
  label: string
  className: string
  disabled?: boolean
}

/**
 * 更改数据目录的按钮与确认流程，设置“数据”页与引导的预估页共用：
 * 目录由 Rust 侧的对话框选择，不能使用时弹窗说明原因；确认后重启，下次启动时迁移并显示进度，重启失败用胶囊通知
 */
export default function ChangeDataDirButton({ label, className, disabled }: Props) {
  const [dialog, setDialog] = useState<Dialog | null>(null)
  const [restarting, setRestarting] = useState(false)

  const pick = () => {
    invoke<DataDirChange | null>('data_dir_pick')
      .then((change) => {
        if (!change) return
        if (change.blocked) {
          setDialog({ type: 'blocked', message: change.blocked, rows: [['当前目录', change.from], ['新目录', change.to]] })
          return
        }
        setDialog({ type: 'change', change })
      })
      .catch((err) => {
        console.warn('选择数据目录失败：', err)
        setDialog({ type: 'blocked', message: errorMessage(err, '无法选择这个目录，请重试。') })
      })
  }

  const confirmChange = () => {
    setRestarting(true)
    invoke('data_dir_change').catch((err) => {
      console.warn('更改数据目录失败：', err)
      setRestarting(false)
      setDialog(null)
      showToast(errorMessage(err, '更改数据目录失败，请重试。'), 'error')
    })
  }

  return (
    <>
      <button type="button" className={className} disabled={disabled} onClick={pick}>
        {label}
      </button>
      {dialog?.type === 'blocked' && (
        <ConfirmDialog title="无法使用该目录" message={dialog.message} confirmText="知道了" hideCancel onConfirm={() => setDialog(null)} onCancel={() => setDialog(null)}>
          {dialog.rows && <PathBox rows={dialog.rows} />}
        </ConfirmDialog>
      )}
      {dialog?.type === 'change' && (
        <ConfirmDialog
          title="更改数据目录"
          message={`将 ${dialog.change.files} 个文件（${formatSize(dialog.change.bytes)}）复制到新目录。\n\nRummage 会先重新启动，复制完成后删除原目录。`}
          confirmText="重新启动并更改"
          busy={restarting ? '正在重新启动' : undefined}
          onConfirm={confirmChange}
          onCancel={() => setDialog(null)}
        >
          <div className="space-y-3">
            <PathBox rows={[['当前目录', dialog.change.from], ['新目录', dialog.change.to]]} />
            <p className="text-xs leading-relaxed text-gray-500 dark:text-gray-400">建立索引会暂停，重新启动后继续。复制失败时继续使用原目录。</p>
          </div>
        </ConfirmDialog>
      )}
    </>
  )
}
