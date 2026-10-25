import { useCallback, useEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { invoke } from '@tauri-apps/api/core'
import type { FailedCursor, FailedFile, FailedPage } from '../types'
import { copyWithToast } from '../lib/clipboard'
import { errorMessage } from '../lib/errors'
import { useEscape } from '../hooks/useEscape'
import ConfirmDialog, { renderMessage } from './ConfirmDialog'
import { CloseIcon, KindIcon } from './icons'
import { BUTTON, ERROR_TEXT, ICON_TILE, PRIMARY_BUTTON, SMALL_BUTTON } from './onboarding/styles'
import PathText from './PathText'
import { HoverTip } from './Tip'
import { showToast } from './Toast'

/** 可点击的文件名与路径：换行显示完整 */
const LINK_TEXT = 'text-left [overflow-wrap:anywhere] transition duration-200 hover:text-blue-500 dark:hover:text-blue-400'
/** 还没量过的行按这个高度（px）估算 */
const EST_HEIGHT = 76
/** 可视区域上下各多渲染这么高（px）的行 */
const OVERSCAN = 400
/** 每次读取的条数；滚动到已读取部分的末尾附近时读下一页 */
const PAGE = 200

/**
 * “失败文件”列表：建立索引失败的文件、所在路径与原因，可逐个或全部重试或忽略；点文件名打开文件，点路径在文件管理器中打开所在的文件夹，打不开时复制路径。
 * 忽略即把文件加入所属文件夹的排除规则并从索引中移除，全部忽略前先确认；操作出错时用胶囊通知。
 * 样式沿用 FontLicensesModal（ConfirmDialog 的玻璃面板）；文件名与路径换行显示完整，行高不定，按量得的行高虚拟化。按游标分页读取，合并时按文件 ID 去重，
 * 读取期间重试了某一行也不会漏行；读取失败时停止自动读取，显示原因与“重试”。加密文档没有“重试”。Esc 或点遮罩关闭
 */
export default function FailedFilesDialog({ onClose }: { onClose: () => void }) {
  const [files, setFiles] = useState<FailedFile[]>([])
  const [total, setTotal] = useState<number | null>(null)
  const [retryable, setRetryable] = useState(0)
  const [hasMore, setHasMore] = useState(true)
  const [pageError, setPageError] = useState('')
  // 进行中的操作：某一行，或全部重试、全部忽略
  const [busy, setBusy] = useState<number | 'all' | 'ignoreAll' | null>(null)
  const [confirmIgnore, setConfirmIgnore] = useState(false)
  const [view, setView] = useState({ top: 0, height: 0 })
  const listRef = useRef<HTMLDivElement>(null)
  const loading = useRef(false)
  // 已读取的最后一页的最后一行；与当前列表无关，重试移除了这一行也照常从它之后取
  const cursor = useRef<FailedCursor | null>(null)
  // 量过的行高，按文件 ID；变化时 measured 加一触发重新排布
  const heights = useRef(new Map<number, number>())
  const [, setMeasured] = useState(0)
  const [ro] = useState(
    () =>
      new ResizeObserver((entries) => {
        let changed = false
        for (const e of entries) {
          const el = e.target as HTMLElement
          const id = Number(el.dataset.id)
          if (heights.current.get(id) === el.offsetHeight) continue
          heights.current.set(id, el.offsetHeight)
          changed = true
        }
        if (changed) setMeasured((n) => n + 1)
      }),
  )
  useEffect(() => () => ro.disconnect(), [ro])
  const observe = useCallback(
    (el: HTMLDivElement | null) => {
      if (!el) return
      ro.observe(el)
      return () => ro.unobserve(el)
    },
    [ro],
  )

  // 确认对话框打开时由它处理 Esc
  useEscape(onClose, !confirmIgnore)

  const load = useCallback(() => {
    if (loading.current) return
    loading.current = true
    setPageError('')
    invoke<FailedPage>('failed_files', { after: cursor.current, limit: PAGE })
      .then((page) => {
        const last = page.files[page.files.length - 1]
        if (last) cursor.current = { failedAt: last.failedAt, fileId: last.fileId }
        setFiles((prev) => {
          const seen = new Set(prev.map((f) => f.fileId))
          return [...prev, ...page.files.filter((f) => !seen.has(f.fileId))]
        })
        setTotal(page.total)
        setRetryable(page.retryable)
        setHasMore(page.files.length === PAGE)
      })
      .catch((err) => {
        console.warn('无法读取失败的文件：', err)
        setPageError(errorMessage(err, '无法读取失败的文件，请稍后再试。'))
      })
      .finally(() => {
        loading.current = false
      })
  }, [])

  useEffect(load, [load])

  useEffect(() => {
    const el = listRef.current
    if (!el) return
    const measure = () => setView({ top: el.scrollTop, height: el.clientHeight })
    measure()
    const ro = new ResizeObserver(measure)
    ro.observe(el)
    return () => ro.disconnect()
  }, [])

  let y = 0
  const tops = files.map((f) => {
    const top = y
    y += heights.current.get(f.fileId) ?? EST_HEIGHT
    return top
  })
  const height = y
  const range = { start: 0, end: 0 }
  while (range.start < files.length && tops[range.start] + (heights.current.get(files[range.start].fileId) ?? EST_HEIGHT) < view.top - OVERSCAN) range.start++
  range.end = range.start
  while (range.end < files.length && tops[range.end] < view.top + view.height + OVERSCAN) range.end++

  // 快滚到已读取部分的末尾时读下一页；读取失败后等用户点“重试”
  useEffect(() => {
    if (total !== null && hasMore && !pageError && range.end >= files.length - 20) load()
  }, [range.end, files.length, total, hasMore, pageError, load])

  /** 重试或忽略一行，成功后从列表中移除 */
  const act = (file: FailedFile, ignore: boolean) => {
    setBusy(file.fileId)
    invoke(ignore ? 'ignore_failed' : 'retry_failed', { fileId: file.fileId })
      .then(() => {
        setFiles((prev) => prev.filter((f) => f.fileId !== file.fileId))
        setTotal((n) => (n === null ? n : Math.max(0, n - 1)))
        if (file.retryable) setRetryable((n) => Math.max(0, n - 1))
      })
      .catch((err) => {
        console.warn(ignore ? '忽略失败的文件时出错：' : '重试失败的文件时出错：', err)
        showToast(errorMessage(err, ignore ? '无法忽略，请稍后再试' : '无法重试，请稍后再试'), 'error')
      })
      .finally(() => setBusy(null))
  }

  const open = (file: FailedFile) => {
    invoke('open_file', { fileId: file.fileId }).catch((err) => {
      console.warn('无法打开这个文件：', err)
      showToast(errorMessage(err, '无法打开这个文件'), 'error')
    })
  }

  const ignoreAll = () => {
    setConfirmIgnore(false)
    setBusy('ignoreAll')
    invoke('ignore_all_failed')
      .then(onClose)
      .catch((err) => {
        console.warn('忽略全部失败的文件时出错：', err)
        showToast(errorMessage(err, '无法忽略，请稍后再试'), 'error')
      })
      .finally(() => setBusy(null))
  }

  const retryAll = () => {
    setBusy('all')
    invoke('retry_all_failed')
      .then(onClose)
      .catch((err) => {
        console.warn('重试全部失败的文件时出错：', err)
        showToast(errorMessage(err, '无法重试，请稍后再试'), 'error')
      })
      .finally(() => setBusy(null))
  }

  return createPortal(
    <div className="fixed inset-0 z-[110] flex items-center justify-center modal-pad" onClick={onClose}>
      <div className="absolute inset-0 animate-overlay-in bg-black/20 backdrop-blur-md dark:bg-black/40" />
      <div
        role="dialog"
        aria-modal="true"
        aria-label="失败的文件"
        className="relative z-10 flex h-[85vh] max-h-[min(640px,100%)] w-full max-w-2xl animate-confirm-in flex-col rounded-3xl border border-white/50 bg-white/90 shadow-[0_8px_40px_rgb(0,0,0,0.12)] ring-1 ring-black/5 backdrop-blur-xl dark:border-white/[0.08] dark:bg-gray-900/90 dark:shadow-[0_8px_40px_rgb(0,0,0,0.4)] dark:ring-white/10"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="shrink-0 px-6 pb-3 pt-6">
          <div className="flex items-center justify-between gap-4">
            <h3 className="text-base font-bold text-gray-800 dark:text-gray-100">
              失败的文件
              {total !== null && <span className="ml-2 text-sm font-normal text-gray-400 dark:text-gray-500">共 {total} 个</span>}
            </h3>
            <button
              type="button"
              onClick={onClose}
              className="shrink-0 rounded-full p-1 text-gray-400 transition hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-white/[0.06] dark:hover:text-gray-200"
              aria-label="关闭"
            >
              <CloseIcon className="h-5 w-5" />
            </button>
          </div>
          <div className="mt-2 space-y-1.5 text-sm leading-relaxed text-gray-500 dark:text-gray-400">
            <p>这些文件建立索引时出错，没有另外说明的只能按文件名搜索。连续失败 3 次后不再自动处理，文件修改后会重新尝试。</p>
            <p>排除问题后可以重试，加密文档去掉密码后会自动重新处理；不需要索引的文件可以忽略，加入排除规则并从索引中移除。</p>
          </div>
        </div>

        <div
          ref={listRef}
          onScroll={(e) => setView({ top: e.currentTarget.scrollTop, height: e.currentTarget.clientHeight })}
          className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-6 [scrollbar-gutter:stable]"
        >
          {total === 0 && <p className="py-10 text-center text-sm text-gray-400 dark:text-gray-500">没有失败的文件</p>}
          <div className="relative" style={{ height }}>
            {files.slice(range.start, range.end).map((f, i) => {
              const dir = f.path.slice(0, Math.max(0, f.path.length - f.name.length - 1))
              return (
                <div
                  key={f.fileId}
                  data-id={f.fileId}
                  ref={observe}
                  className="absolute inset-x-0 flex items-start gap-3 border-b border-black/[0.05] py-3 dark:border-white/[0.06]"
                  style={{ top: tops[range.start + i] }}
                >
                  <span className={ICON_TILE}>
                    <KindIcon kind={f.kind} className="h-4 w-4" />
                  </span>
                  <span className="min-w-0 flex-1">
                    <button type="button" onClick={() => open(f)} className={`block text-sm text-gray-800 dark:text-gray-100 ${LINK_TEXT}`}>
                      {f.name}
                    </button>
                    <button type="button" onClick={() => void copyWithToast(f.path, '路径')} className={`mt-0.5 block text-xs text-gray-400 dark:text-gray-500 ${LINK_TEXT}`}>
                      <PathText path={dir} />
                      <HoverTip label="复制路径" />
                    </button>
                    {f.reason && (
                      <span data-selectable-text className="mt-0.5 block text-xs text-red-500 [overflow-wrap:anywhere] dark:text-red-400">
                        {renderMessage(f.reason, 'font-semibold')}
                      </span>
                    )}
                    <span className="mt-2 flex justify-end gap-1.5">
                      {f.retryable && (
                        <button type="button" className={SMALL_BUTTON} disabled={busy !== null} onClick={() => act(f, false)}>
                          重试
                        </button>
                      )}
                      <button type="button" className={SMALL_BUTTON} disabled={busy !== null} onClick={() => act(f, true)}>
                        忽略
                      </button>
                    </span>
                  </span>
                </div>
              )
            })}
          </div>
          {pageError && (
            <div className="flex items-center gap-3 py-3">
              <p data-selectable-text className={`flex-1 ${ERROR_TEXT}`}>{pageError}</p>
              <button type="button" className={`flex-shrink-0 ${SMALL_BUTTON}`} onClick={load}>
                重试
              </button>
            </div>
          )}
        </div>

        <div className="shrink-0 px-6 pb-6 pt-4">
          <div className="flex gap-2">
            <button type="button" className={`flex-1 whitespace-nowrap max-sm:px-2 ${BUTTON}`} onClick={onClose}>
              关闭
            </button>
            <button type="button" className={`flex-1 whitespace-nowrap max-sm:px-2 ${BUTTON}`} disabled={busy !== null || !total} onClick={() => setConfirmIgnore(true)}>
              {busy === 'ignoreAll' ? '正在忽略' : '全部忽略'}
            </button>
            <button type="button" className={`flex-1 whitespace-nowrap max-sm:px-2 ${PRIMARY_BUTTON}`} disabled={busy !== null || !retryable} onClick={retryAll}>
              {busy === 'all' ? '正在重试' : '全部重试'}
            </button>
          </div>
        </div>
        {/* 放在面板内：点遮罩取消时不连带关闭本弹窗 */}
        {confirmIgnore && (
          <ConfirmDialog
            title="全部忽略"
            message={`将忽略全部 ${total ?? 0} 个失败的文件：加入各自所在文件夹的排除规则，并从索引中移除，之后不再跟进它们的变化。\n\n需要时可以在「设置 → 文件夹」的排除规则中删除对应的规则。`}
            confirmText="全部忽略"
            onConfirm={ignoreAll}
            onCancel={() => setConfirmIgnore(false)}
          />
        )}
      </div>
    </div>,
    document.body,
  )
}
