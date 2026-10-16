import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { RootInfo } from '../../types'
import { errorMessage } from '../../lib/errors'
import { useCoreEvent } from '../../hooks/useCoreEvent'
import ConfirmDialog from '../ConfirmDialog'
import LoadError from '../LoadError'
import { CloseIcon, FilterIcon, FolderIcon, PlusIcon } from '../icons'
import { HoverTip } from '../Tip'
import { CARD, CARD_ROW, CARD_ROW_HOVER, ERROR_TEXT, ICON_TILE, ROW_ICON_BUTTON } from './styles'

interface Props {
  onCount?: (n: number) => void
  /** 设置页中用分区标题，引导中用步骤标题 */
  section?: boolean
  /** 文件夹行内的附加编辑（设置页的排除规则）：`label` 是行尾入口的文字，`panel` 是展开后的编辑区 */
  editor?: {
    label: (root: RootInfo) => string
    panel: (root: RootInfo, close: () => void, reload: () => void) => ReactNode
  }
}

/**
 * 第 1 步：选择要索引的文件夹。文件夹由 Rust 侧的系统对话框选择后直接交给核心（前端不提交路径），
 * 添加后核心立即扫描，扫描结束（`ScanProgress` 的 `done`）时刷新文件数；命令行或 MCP 增删、修改文件夹（`RootsChanged`）时刷新列表。
 * 设置页的“文件夹”也用它
 */
export default function FoldersStep({ onCount, section, editor }: Props) {
  const [roots, setRoots] = useState<RootInfo[] | null>(null)
  // 刚添加、还没扫描完的文件夹
  const [scanning, setScanning] = useState<Set<number>>(new Set())
  // 添加、移除失败的原因
  const [error, setError] = useState('')
  const [loadError, setLoadError] = useState('')
  const [busy, setBusy] = useState(false)
  // 等待确认移除的文件夹
  const [removing, setRemoving] = useState<RootInfo | null>(null)
  // 展开了附加编辑的文件夹
  const [editing, setEditing] = useState<number | null>(null)
  const known = useRef<Set<number> | null>(null)
  // 已扫描完的文件夹：小文件夹的结束事件可能早于添加后的刷新到达
  const finished = useRef(new Set<number>())

  const load = useCallback(() => {
    invoke<RootInfo[]>('list_roots')
      .then((list) => {
        // 第一次读取之后新出现的文件夹，在扫描结束前显示“正在统计”
        const prev = known.current
        if (prev) {
          const added = list.filter((r) => !prev.has(r.id) && !finished.current.has(r.id)).map((r) => r.id)
          if (added.length) setScanning((s) => new Set([...s, ...added]))
        }
        known.current = new Set(list.map((r) => r.id))
        setRoots(list)
        setLoadError('')
        onCount?.(list.length)
      })
      .catch((err) => {
        console.warn('无法读取文件夹列表：', err)
        setLoadError(errorMessage(err, '无法读取文件夹列表。'))
      })
  }, [onCount])

  useEffect(() => {
    load()
  }, [load])

  useCoreEvent((ev) => {
    if (ev.type === 'RootsChanged') {
      load()
      return
    }
    if (ev.type !== 'ScanProgress' || !ev.done) return
    const id = ev.rootId
    finished.current.add(id)
    setScanning((s) => {
      const next = new Set(s)
      next.delete(id)
      return next
    })
    load()
  })

  const add = () => {
    setBusy(true)
    setError('')
    invoke<boolean>('add_root')
      .then((added) => {
        if (added) load()
      })
      .catch((err) => {
        console.warn('无法添加文件夹：', err)
        setError(errorMessage(err, '无法添加这个文件夹。'))
      })
      .finally(() => setBusy(false))
  }

  const remove = (id: number) => {
    setError('')
    invoke('remove_root', { id })
      .then(load)
      .catch((err) => {
        console.warn('无法移除文件夹：', err)
        setError(errorMessage(err, '无法移除这个文件夹。'))
      })
  }

  return (
    <div>
      {section ? (
        <>
          <h4 className="text-sm font-bold text-gray-800 dark:text-gray-100">文件夹</h4>
          <p className="mt-1 text-xs text-gray-500 dark:text-gray-500">Rummage 只读取这些文件夹中的文件，不会修改、移动或删除它们。</p>
        </>
      ) : (
        <>
          <h2 className="text-base font-bold text-gray-800 dark:text-gray-100">选择要搜索的文件夹</h2>
          <p className="mt-1 text-sm text-gray-500 dark:text-gray-400">Rummage 只读取这些文件夹中的文件，不会修改、移动或删除它们。</p>
        </>
      )}

      <div className={`mt-4 ${CARD}`}>
        {roots?.map((r) => (
          <div key={r.id}>
            <div className={CARD_ROW}>
              <span className={ICON_TILE}>
                <FolderIcon className="h-4 w-4" />
              </span>
              <span className="min-w-0 flex-1">
                <span data-selectable-text className="block truncate text-sm text-gray-800 dark:text-gray-100">
                  {r.path}
                  <HoverTip label={r.path} wrap />
                </span>
                <span className={`mt-0.5 block text-xs ${r.offline ? 'text-amber-600 dark:text-amber-400' : 'text-gray-500 dark:text-gray-400'}`}>
                  {r.offline ? '设备未连接' : scanning.has(r.id) ? '正在统计文件' : `${r.files} 个文件`}
                </span>
              </span>
              {editor && (
                <button
                  type="button"
                  onClick={() => setEditing(editing === r.id ? null : r.id)}
                  aria-expanded={editing === r.id}
                  className={`inline-flex shrink-0 items-center gap-1 rounded-lg px-2 py-1.5 text-xs transition hover:bg-black/[0.05] hover:text-gray-600 dark:hover:bg-white/[0.08] dark:hover:text-gray-200 ${editing === r.id ? 'bg-black/[0.05] text-gray-600 dark:bg-white/[0.08] dark:text-gray-200' : 'text-gray-400 dark:text-gray-500'}`}
                >
                  <FilterIcon className="h-3.5 w-3.5" />
                  {editor.label(r)}
                </button>
              )}
              <button
                type="button"
                onClick={() => setRemoving(r)}
                aria-label="移除文件夹"
                className={`shrink-0 ${ROW_ICON_BUTTON}`}
              >
                <CloseIcon className="h-4 w-4" />
                <HoverTip label="移除（不会删除你的文件）" />
              </button>
            </div>
            {editor && editing === r.id && <div className="px-4 pb-4">{editor.panel(r, () => setEditing(null), load)}</div>}
          </div>
        ))}
        <button type="button" disabled={busy} onClick={add} className={`${CARD_ROW} ${CARD_ROW_HOVER}`}>
          <span className={ICON_TILE}>
            <PlusIcon className="h-4 w-4" />
          </span>
          <span className="min-w-0 flex-1">
            <span className="block text-sm text-gray-700 dark:text-gray-200">添加文件夹</span>
            {roots && !roots.length && <span className="mt-0.5 block text-xs text-gray-500 dark:text-gray-400">还没有添加文件夹</span>}
          </span>
        </button>
      </div>
      {loadError && <LoadError message={loadError} onRetry={load} className="mt-2" />}
      {error && <p data-selectable-text className={`mt-2 ${ERROR_TEXT}`}>{error}</p>}
      {removing && (
        <ConfirmDialog
          title="移除文件夹"
          message={`将不再索引「${removing.path}」，并删除它的索引数据。\n\n不会删除你的文件。`}
          confirmText="移除"
          danger
          onConfirm={() => {
            remove(removing.id)
            setRemoving(null)
          }}
          onCancel={() => setRemoving(null)}
        />
      )}
    </div>
  )
}
