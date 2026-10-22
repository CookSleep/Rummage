import { useEffect, useState } from 'react'
import { createPortal } from 'react-dom'
import { invoke } from '@tauri-apps/api/core'
import type { DbStatus } from '../types'
import { errorMessage } from '../lib/errors'
import { isTauri } from '../lib/platform'
import ConfirmDialog from './ConfirmDialog'
import { showToast } from './Toast'

/**
 * “索引数据库无法打开”：核心连上后查一次数据库状态，
 * 打不开时弹出，不能关闭。原因（核心给出的整句，含用户能做的事）之外：
 * - 清除后重建能解决的（损坏、从备份恢复失败）多一个“清除并重建”，确认后只删除索引数据库，迁移备份保留，
 *   核心随后重启并新建；文件夹与设置也在库中，需要重新引导。
 * - 其余（磁盘未连接、不可写、版本过新、被占用、升级失败等）只提供“打开数据目录”和“退出”，处理好后重新打开 Rummage。
 * “退出”连同核心一起退出，下次打开时重新读取数据库；默认焦点在“退出”上，避免误按回车清除
 */
export default function DbErrorDialog({ connected }: { connected: boolean }) {
  // 只在打不开时记下
  const [status, setStatus] = useState<{ error: string; rebuildable: boolean } | null>(null)
  const [confirming, setConfirming] = useState(false)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    if (!isTauri || !connected) return
    invoke<DbStatus>('db_status')
      .then((s) => setStatus(s.error ? { error: s.error, rebuildable: s.rebuildable } : null))
      .catch((err) => console.warn('无法读取索引数据库状态：', err))
  }, [connected])

  if (!status) return null

  const fail = (fallback: string) => (err: unknown) => {
    console.warn(fallback, err)
    showToast(errorMessage(err, fallback), 'error')
  }

  const rebuild = () => {
    setBusy(true)
    invoke('reset_index')
      .then(() => {
        setConfirming(false)
        setStatus(null)
      })
      .catch((err) => {
        setConfirming(false)
        fail('清除索引数据库失败，请重试。')(err)
      })
      .finally(() => setBusy(false))
  }

  const buttonBase = 'w-full rounded-xl py-2 text-sm transition disabled:cursor-not-allowed disabled:opacity-50'
  const secondary = `${buttonBase} border border-transparent bg-black/[0.04] text-gray-600 hover:bg-black/[0.07] dark:bg-white/[0.06] dark:text-gray-300 dark:hover:bg-white/[0.1]`

  return createPortal(
    <>
      <div className="fixed inset-0 z-[110] flex items-center justify-center modal-pad">
        <div className="absolute inset-0 animate-overlay-in bg-black/20 backdrop-blur-md dark:bg-black/40" />
        <div
          role="alertdialog"
          aria-modal="true"
          aria-label="索引数据库无法打开"
          className="relative z-10 flex max-h-full w-full max-w-sm animate-confirm-in flex-col rounded-3xl border border-white/50 bg-white/90 p-6 shadow-[0_8px_40px_rgb(0,0,0,0.12)] ring-1 ring-black/5 backdrop-blur-xl dark:border-white/[0.08] dark:bg-gray-900/90 dark:shadow-[0_8px_40px_rgb(0,0,0,0.4)] dark:ring-white/10"
        >
          <h3 className="mb-2 shrink-0 text-base font-bold text-gray-800 dark:text-gray-100">索引数据库无法打开</h3>
          {/* 原因可能以空行分段，过长时在这里滚动，同 ConfirmDialog */}
          <div data-selectable-text className="-mx-2 mb-3 min-h-0 space-y-2 overflow-y-auto overscroll-contain px-2 text-pretty text-sm leading-relaxed text-gray-500 dark:text-gray-400">
            {status.error.split(/\n{2,}/).map((para, idx) => <p key={idx}>{para}</p>)}
          </div>
          <p className="mb-6 shrink-0 text-xs leading-relaxed text-gray-500 dark:text-gray-400">
            {status.rebuildable
              ? '清除并重建只删除索引数据库，不会删除你的文件，备份文件会保留；之后需要重新选择要索引的文件夹，已下载的模型仍然保留。'
              : '你的文件和索引都没有被改动。处理好上面的问题后，重新打开 Rummage 即可。'}
          </p>
          <div className="flex shrink-0 flex-col gap-2">
            {status.rebuildable && (
              <button type="button" disabled={busy} onClick={() => setConfirming(true)} className={`${buttonBase} bg-red-500 font-medium text-white hover:bg-red-600`}>
                清除并重建
              </button>
            )}
            <button type="button" disabled={busy} onClick={() => invoke('open_data_dir').catch(fail('无法打开数据目录。'))} className={secondary}>
              打开数据目录
            </button>
            <button type="button" autoFocus disabled={busy} onClick={() => invoke('exit_app').catch(fail('无法退出，请直接关闭窗口。'))} className={secondary}>
              退出
            </button>
          </div>
        </div>
      </div>
      {confirming && (
        <ConfirmDialog
          title="清除并重建索引"
          message={'将删除索引数据库并重新建立。\n\n不会删除你的文件，备份文件会保留。文件夹列表与设置需要在引导中重新选择。'}
          confirmText="清除并重建"
          danger
          busy={busy ? '正在清除' : undefined}
          onConfirm={rebuild}
          onCancel={() => setConfirming(false)}
        />
      )}
    </>,
    document.body,
  )
}
