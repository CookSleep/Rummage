import { useCallback, useEffect, useState, type ReactNode } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { AddedRoot, RootInfo } from '../../types'
import { errorMessage } from '../../lib/errors'
import { isAndroid } from '../../lib/platform'
import { useCoreEvent } from '../../hooks/useCoreEvent'
import { useStorageAccess } from '../../hooks/useStorageAccess'
import ConfirmDialog from '../ConfirmDialog'
import LoadError from '../LoadError'
import PathText from '../PathText'
import { CloseIcon, FilterIcon, FolderIcon, PlusIcon, WarningIcon } from '../icons'
import { HoverTip } from '../Tip'
import { showToast } from '../Toast'
import { CARD, CARD_ROW, CARD_ROW_HOVER, ICON_TILE, ROW_ACTIONS, ROW_ICON_BUTTON, ROW_ICON_TILE, ROW_LONG_TEXT, SMALL_BUTTON, SMALL_PRIMARY_BUTTON } from './styles'

/** Android 上可一键添加的手机存储中的文件夹，与 src-tauri/src/setup.rs 的 `COMMON_ROOTS` 一致 */
const COMMON_ROOTS = ['DCIM', 'Pictures', 'Download', 'Documents']

/** 主存储（`/storage/emulated/<用户>`）下的一级文件夹，取出文件夹名 */
const STORAGE_CHILD = /^\/storage\/emulated\/\d+\/([^/]+)\/?$/

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
 * 添加后核心立即统计文件，统计完之前（`scanning`）显示“正在统计文件”，扫描结束（`ScanProgress` 的 `done`）时刷新；命令行或 MCP 增删、修改文件夹（`RootsChanged`）时刷新列表。
 * 设置页的“文件夹”也用它。
 * Android 上读取文件需要“所有文件访问”权限：未授权时在列表上方提示前往授权，并禁用添加；已授权时可一键添加常用文件夹。
 * 本项目不面向全盘索引：添加处一直提示只添加需要的文件夹；添加的是整个磁盘或存储时弹窗建议移除。
 * 不能添加时弹窗说明原因，移除失败用胶囊通知
 */
export default function FoldersStep({ onCount, section, editor }: Props) {
  const storage = useStorageAccess()
  const [roots, setRoots] = useState<RootInfo[] | null>(null)
  // 不能添加的原因
  const [addError, setAddError] = useState('')
  const [loadError, setLoadError] = useState('')
  const [busy, setBusy] = useState(false)
  // 等待确认移除的文件夹
  const [removing, setRemoving] = useState<RootInfo | null>(null)
  // 展开了附加编辑的文件夹
  const [editing, setEditing] = useState<number | null>(null)
  // 刚添加的整个磁盘或存储，等用户决定是否移除
  const [wholeDrive, setWholeDrive] = useState<AddedRoot | null>(null)

  const load = useCallback(() => {
    invoke<RootInfo[]>('list_roots')
      .then((list) => {
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
    if (ev.type === 'RootsChanged' || (ev.type === 'ScanProgress' && ev.done)) load()
  })

  // 添加期间列表不可操作：选择器关闭、页面重新可见时，以及点击页面任意处时，用胶囊通知说明原因。
  // 可见后稍等再提示，取消选择时请求很快结束，不会闪出提示
  useEffect(() => {
    if (!busy) return
    const hint = () => showToast('正在添加文件夹')
    let timer: ReturnType<typeof setTimeout> | undefined
    const onVisible = () => {
      if (document.visibilityState === 'visible') timer = setTimeout(hint, 300)
    }
    document.addEventListener('pointerdown', hint)
    document.addEventListener('visibilitychange', onVisible)
    return () => {
      clearTimeout(timer)
      document.removeEventListener('pointerdown', hint)
      document.removeEventListener('visibilitychange', onVisible)
    }
  }, [busy])

  const add = () => {
    setBusy(true)
    invoke<AddedRoot | null>('add_root')
      .then((added) => {
        if (!added) return
        load()
        if (added.wholeDrive) setWholeDrive(added)
      })
      .catch((err) => {
        console.warn('无法添加文件夹：', err)
        setAddError(errorMessage(err, '无法添加这个文件夹。'))
      })
      .finally(() => setBusy(false))
  }

  const addCommon = (name: string) => {
    setBusy(true)
    invoke('add_common_root', { name })
      .then(load)
      .catch((err) => {
        console.warn('无法添加文件夹：', err)
        setAddError(errorMessage(err, '无法添加这个文件夹。'))
      })
      .finally(() => setBusy(false))
  }

  const addedCommon = new Set(roots?.map((r) => STORAGE_CHILD.exec(r.path)?.[1]))

  const remove = (id: number) => {
    invoke('remove_root', { id })
      .then(load)
      .catch((err) => {
        console.warn('无法移除文件夹：', err)
        showToast(errorMessage(err, '无法移除这个文件夹。'), 'error')
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

      {storage.granted === false && (
        <div className="mt-4 flex items-center gap-3 rounded-2xl bg-amber-500/[0.1] px-4 py-3 max-sm:flex-wrap dark:bg-amber-500/[0.14]">
          <WarningIcon className="h-4 w-4 shrink-0 text-amber-600 dark:text-amber-400 max-sm:self-start max-sm:mt-0.5" />
          <span className="min-w-0 flex-1 max-sm:basis-[calc(100%-1.75rem)]">
            <span className="block text-sm font-medium text-amber-800 dark:text-amber-300">需要“所有文件访问”权限</span>
            <span className="mt-0.5 block text-xs text-amber-700 dark:text-amber-400/90">用于读取要建立索引的文件。Rummage 只读取，不修改、不上传。</span>
          </span>
          <button type="button" onClick={storage.request} className={`shrink-0 max-sm:ml-7 ${SMALL_PRIMARY_BUTTON}`}>
            前往授权
          </button>
        </div>
      )}

      <div className={`mt-4 ${CARD}`}>
        {roots?.map((r) => (
          <div key={r.id}>
            <div className={CARD_ROW}>
              <span className={`${ROW_ICON_TILE} ${ICON_TILE}`}>
                <FolderIcon className="h-4 w-4" />
              </span>
              <span className="min-w-0 flex-1">
                <span data-selectable-text className={`block text-sm text-gray-800 dark:text-gray-100 ${ROW_LONG_TEXT}`}>
                  <PathText path={r.path} />
                  <HoverTip label={r.path} wrap />
                </span>
                <span className={`mt-0.5 block text-xs ${r.offline ? 'text-amber-600 dark:text-amber-400' : 'text-gray-500 dark:text-gray-400'}`}>
                  {r.missing ? '找不到文件夹' : r.offline ? '设备未连接' : r.scanning ? '正在统计文件' : `${r.files} 个文件`}
                </span>
              </span>
              {/* 只有移除按钮时留在行尾，手机宽度下也不另起一行 */}
              <span className={`gap-3 ${editor ? ROW_ACTIONS : 'flex shrink-0 items-center'}`}>
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
              </span>
            </div>
            {editor && editing === r.id && <div className="px-4 pb-4">{editor.panel(r, () => setEditing(null), load)}</div>}
          </div>
        ))}
        <button type="button" disabled={busy || !storage.granted} onClick={add} className={`${CARD_ROW} ${CARD_ROW_HOVER}`}>
          <span className={ICON_TILE}>
            <PlusIcon className="h-4 w-4" />
          </span>
          <span className="min-w-0 flex-1">
            <span className="block text-sm text-gray-700 dark:text-gray-200">添加文件夹</span>
            <span className="mt-0.5 block text-xs text-gray-500 dark:text-gray-400">{roots && !roots.length ? '还没有添加文件夹，' : ''}只添加需要搜索的文件夹，不建议添加{isAndroid ? '整个存储' : '整个磁盘'}</span>
          </span>
        </button>
        {isAndroid && storage.granted && (
          <div className={`${CARD_ROW} flex-wrap`}>
            <span className="text-xs text-gray-500 dark:text-gray-400">快速添加</span>
            {COMMON_ROOTS.map((name) => (
              <button key={name} type="button" disabled={busy || addedCommon.has(name)} onClick={() => addCommon(name)} className={SMALL_BUTTON}>
                {name}
              </button>
            ))}
            <span className="basis-full text-xs text-gray-400 dark:text-gray-500">系统的文件夹选择器可能不允许选择 Download 等文件夹，可以在这里直接添加</span>
          </div>
        )}
      </div>
      {loadError && <LoadError message={loadError} onRetry={load} className="mt-2" />}
      {addError && (
        <ConfirmDialog
          title="无法添加文件夹"
          message={addError}
          confirmText="知道了"
          hideCancel
          onConfirm={() => setAddError('')}
          onCancel={() => setAddError('')}
        />
      )}
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
      {wholeDrive && (
        <ConfirmDialog
          title="建议只添加需要的文件夹"
          message={`「${wholeDrive.path}」是${isAndroid ? '整个存储' : '整个磁盘'}，其中的系统与程序文件大多只能按文件名搜索。Rummage 不是为全盘索引设计的，文件过多会拖慢建立索引和搜索。\n\n请尽量只添加需要搜索的文件夹。`}
          confirmText="移除"
          danger
          hideCancel
          extra={{ label: '保留', onClick: () => setWholeDrive(null) }}
          onConfirm={() => {
            remove(wholeDrive.id)
            setWholeDrive(null)
          }}
          onCancel={() => setWholeDrive(null)}
        />
      )}
    </div>
  )
}
