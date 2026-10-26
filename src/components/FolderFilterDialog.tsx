import { useState } from 'react'
import { createPortal } from 'react-dom'
import { invoke } from '@tauri-apps/api/core'
import type { FolderFilter, FolderFilterEntry, PickedFolder, RootInfo } from '../types'
import { errorMessage } from '../lib/errors'
import { folderScope } from '../lib/filter'
import { abbreviatePath, folderName } from '../lib/format'
import { useEscape } from '../hooks/useEscape'
import Checkbox from './Checkbox'
import ConfirmDialog from './ConfirmDialog'
import { BroomIcon, ChevronDownIcon, CloseIcon, PlusIcon } from './icons'
import { BUTTON, PRIMARY_BUTTON, ROW_ICON_BUTTON } from './onboarding/styles'
import { HoverTip } from './Tip'
import { showToast } from './Toast'

interface Props {
  roots: RootInfo[]
  /** 已保存的筛选，打开时作为草稿 */
  filter: FolderFilter
  onSaved: (next: FolderFilter) => void
  onClose: () => void
}

/** 选择子文件夹后等待用户决定的情况 */
type Pending = { root: RootInfo; picked: PickedFolder }

/**
 * “选择要搜索的文件夹”弹窗：每个根目录一行，可展开其中添加的子文件夹。子文件夹用来缩小根目录的范围（`folderScope`）：
 * 根目录未勾选时其下的子文件夹变淡、不能切换，但保留勾选状态。
 * 改动只在草稿中，点“保存”才写入后台服务；取消、Esc 或点遮罩丢弃。样式沿用 FailedFilesDialog 的玻璃面板
 */
export default function FolderFilterDialog({ roots, filter, onSaved, onClose }: Props) {
  const [entries, setEntries] = useState<FolderFilterEntry[]>(() => [
    ...roots.map((r) => ({ rootId: r.id, rel: '', checked: filter.entries.some((e) => e.rootId === r.id && !e.rel && e.checked) })),
    ...filter.entries.filter((e) => e.rel && roots.some((r) => r.id === e.rootId)),
  ])
  // 有已勾选子文件夹的根目录默认展开
  const [expanded, setExpanded] = useState(() => new Set(filter.entries.filter((e) => e.rel && e.checked).map((e) => e.rootId)))
  const [pending, setPending] = useState<Pending | null>(null)
  const [busy, setBusy] = useState(false)

  useEscape(onClose, !pending)

  const isChecked = (rootId: number, rel: string) => entries.some((e) => e.rootId === rootId && e.rel === rel && e.checked)
  const setChecked = (rootId: number, rel: string, checked: boolean) => setEntries((prev) => prev.map((e) => (e.rootId === rootId && e.rel === rel ? { ...e, checked } : e)))
  const anyChecked = entries.some((e) => !e.rel && e.checked)

  // 加入的子文件夹要在勾选的根目录下才生效，一并勾选根目录
  const add = (root: RootInfo, picked: PickedFolder) => {
    const on = (e: FolderFilterEntry) => e.rootId === root.id && (e.rel === '' || e.rel === picked.rel)
    setEntries((prev) => {
      const next = prev.map((e) => (on(e) ? { ...e, checked: true } : e))
      return picked.status === 'root' || prev.some((e) => e.rootId === root.id && e.rel === picked.rel) ? next : [...next, { rootId: root.id, rel: picked.rel, checked: true }]
    })
  }

  const pick = (root: RootInfo) => {
    setPending(null)
    setBusy(true)
    invoke<PickedFolder | null>('pick_subfolder', { rootId: root.id })
      .then((picked) => {
        if (!picked) return
        if (picked.status === 'outside' || picked.status === 'empty') return setPending({ root, picked })
        add(root, picked)
      })
      .catch((err) => {
        console.warn('无法选择子文件夹：', err)
        showToast(errorMessage(err, '无法选择子文件夹，请稍后再试'), 'error')
      })
      .finally(() => setBusy(false))
  }

  const save = () => {
    if (!anyChecked) return
    const scope = folderScope(entries)
    const all = scope.length === roots.length && scope.every((s) => !s.rel)
    const next = { custom: !all, entries }
    setBusy(true)
    invoke('set_folder_filter', { custom: next.custom, entries: next.entries })
      .then(() => onSaved(next))
      .catch((err) => {
        console.warn('无法保存文件夹筛选：', err)
        showToast(errorMessage(err, '无法保存文件夹筛选，请稍后再试'), 'error')
      })
      .finally(() => setBusy(false))
  }

  return createPortal(
    <div className="fixed inset-0 z-[110] flex items-center justify-center modal-pad" onClick={onClose}>
      <div className="absolute inset-0 animate-overlay-in bg-black/20 backdrop-blur-md dark:bg-black/40" />
      <div
        role="dialog"
        aria-modal="true"
        aria-label="选择要搜索的文件夹"
        className="relative z-10 flex max-h-[min(640px,100%)] w-full max-w-md animate-confirm-in flex-col rounded-3xl border border-white/50 bg-white/90 shadow-[0_8px_40px_rgb(0,0,0,0.12)] ring-1 ring-black/5 backdrop-blur-xl dark:border-white/[0.08] dark:bg-gray-900/90 dark:shadow-[0_8px_40px_rgb(0,0,0,0.4)] dark:ring-white/10"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex shrink-0 items-center justify-between gap-4 px-6 pb-3 pt-6">
          <h3 className="text-base font-bold text-gray-800 dark:text-gray-100">选择要搜索的文件夹</h3>
          <button
            type="button"
            onClick={onClose}
            className="shrink-0 rounded-full p-1 text-gray-400 transition hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-white/[0.06] dark:hover:text-gray-200"
            aria-label="关闭"
          >
            <CloseIcon className="h-5 w-5" />
          </button>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-6 [scrollbar-gutter:stable]">
          {roots.map((root) => {
            const name = folderName(root.path)
            const open = expanded.has(root.id)
            const rootOn = isChecked(root.id, '')
            const base = `${root.path.replace(/[\\/]+$/, '')}${root.path.includes('\\') ? '\\' : '/'}`
            const subs = entries.filter((e) => e.rootId === root.id && e.rel).sort((a, b) => a.rel.localeCompare(b.rel, 'zh-CN'))
            const clearLabel = `清除「${name}」中的勾选`
            const toggleLabel = open ? '收起子文件夹' : '展开子文件夹'
            const narrowed = rootOn && subs.some((s) => s.checked)
            return (
              <div key={root.id} className="border-b border-black/[0.05] py-1 last:border-b-0 dark:border-white/[0.06]">
                <div className="flex min-h-10 items-center gap-1">
                  <div className="min-w-0 flex-1">
                    <Checkbox
                      checked={rootOn}
                      onChange={(on) => setChecked(root.id, '', on)}
                      label={
                        <span className="[overflow-wrap:anywhere]">
                          {name}
                          <HoverTip label={abbreviatePath(root.path)} wrap />
                        </span>
                      }
                    />
                  </div>
                  {narrowed && <span className="shrink-0 text-xs text-gray-400 dark:text-gray-500">仅子文件夹</span>}
                  <button
                    type="button"
                    aria-label={clearLabel}
                    onClick={() => setEntries((prev) => prev.map((e) => (e.rootId === root.id ? { ...e, checked: false } : e)))}
                    className={`relative ${ROW_ICON_BUTTON}`}
                  >
                    <BroomIcon className="h-4 w-4" />
                    <HoverTip label={clearLabel} />
                  </button>
                  <button
                    type="button"
                    aria-label={toggleLabel}
                    aria-expanded={open}
                    onClick={() =>
                      setExpanded((prev) => {
                        const next = new Set(prev)
                        if (open) next.delete(root.id)
                        else next.add(root.id)
                        return next
                      })
                    }
                    className={`relative ${ROW_ICON_BUTTON}`}
                  >
                    <ChevronDownIcon className={`h-4 w-4 transition-transform duration-200 ${open ? 'rotate-180' : ''}`} />
                    <HoverTip label={toggleLabel} />
                  </button>
                </div>
                {open && (
                  <div className="pb-1 pl-6">
                    {subs.map((sub) => (
                      <div key={sub.rel} className="flex min-h-10 items-center gap-1">
                        <div className="min-w-0 flex-1">
                          <Checkbox
                            checked={sub.checked}
                            disabled={!rootOn}
                            onChange={(on) => setChecked(root.id, sub.rel, on)}
                            label={
                              <span className="[overflow-wrap:anywhere]">
                                {sub.rel}
                                <HoverTip label={rootOn ? abbreviatePath(base + sub.rel) : `勾选「${name}」后生效`} wrap />
                              </span>
                            }
                          />
                        </div>
                        <button
                          type="button"
                          aria-label="从列表中移除"
                          onClick={() => setEntries((prev) => prev.filter((e) => e !== sub))}
                          className={`relative ${ROW_ICON_BUTTON}`}
                        >
                          <CloseIcon className="h-4 w-4" />
                          <HoverTip label="从列表中移除" />
                        </button>
                      </div>
                    ))}
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => pick(root)}
                      className="-ml-1.5 flex items-center gap-2 rounded-lg px-1.5 py-2 text-[13px] text-gray-500 transition hover:bg-black/[0.04] hover:text-gray-700 disabled:cursor-not-allowed disabled:opacity-50 dark:text-gray-400 dark:hover:bg-white/[0.06] dark:hover:text-gray-200"
                    >
                      <PlusIcon className="h-4 w-4" />
                      添加子文件夹
                    </button>
                  </div>
                )}
              </div>
            )
          })}
        </div>

        <div className="flex shrink-0 gap-2 px-6 pb-6 pt-4">
          <button type="button" className={`whitespace-nowrap max-sm:px-3 ${BUTTON}`} onClick={() => setEntries((prev) => prev.map((e) => ({ ...e, checked: false })))}>
            清除勾选
          </button>
          <button type="button" className={`ml-auto whitespace-nowrap max-sm:px-3 ${BUTTON}`} onClick={onClose}>
            取消
          </button>
          {/* 不用 disabled：禁用的按钮收不到悬停，气泡出不来 */}
          <button
            type="button"
            aria-disabled={!anyChecked || undefined}
            disabled={busy}
            onClick={save}
            className={`relative whitespace-nowrap max-sm:px-3 ${PRIMARY_BUTTON} ${anyChecked ? '' : 'cursor-not-allowed opacity-50'}`}
          >
            保存
            {!anyChecked && <HoverTip label="至少勾选一个文件夹" />}
          </button>
        </div>

        {/* 放在面板内：点遮罩取消时不连带关闭本弹窗 */}
        {pending?.picked.status === 'outside' && (
          <ConfirmDialog
            title={`请选择「${folderName(pending.root.path)}」中的文件夹`}
            message={`「${pending.picked.path}」不在「${folderName(pending.root.path)}」中。`}
            confirmText="重新选择"
            onConfirm={() => pick(pending.root)}
            onCancel={() => setPending(null)}
          />
        )}
        {pending?.picked.status === 'empty' && (
          <ConfirmDialog
            title="此文件夹中暂无已索引的文件"
            message={'可能是空文件夹、其中的文件都被排除规则排除，或者还在扫描。\n\n仍可添加，建立索引后即可搜到其中的文件。'}
            confirmText="仍然添加"
            hideCancel
            extra={{ label: '重新选择', onClick: () => pick(pending.root) }}
            onConfirm={() => {
              add(pending.root, pending.picked)
              setPending(null)
            }}
            onCancel={() => setPending(null)}
          />
        )}
      </div>
    </div>,
    document.body,
  )
}
