import { useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { IndexStatus, PauseFor, PauseReason } from '../types'
import { useClickOutside } from '../hooks/useClickOutside'
import { errorMessage } from '../lib/errors'
import { formatDuration, pad2 } from '../lib/format'
import { renderMessage } from './ConfirmDialog'
import { PauseIcon, WarningIcon } from './icons'
import { NOTICE_BOX } from './settings/styles'
import { HoverTip } from './Tip'

const PAUSE_TEXT: Record<PauseReason['type'], string> = {
  manual: '已手动暂停',
  sleeping: '系统即将睡眠或刚唤醒，稍后继续',
  disk_low: '磁盘空间不足，已暂停',
  quota: '已达到系统每天 6 小时的后台运行上限',
  hot: '手机温度过高，已暂停',
  battery: '未接电源，已暂停',
  screen_on: '屏幕亮着，已暂停',
  fullscreen: '有全屏程序在运行，已暂停',
}

/** 可在“设置 → 资源”中调整的自动暂停原因与对应的参数 */
const ADJUST_PARAM: Partial<Record<PauseReason['type'], string>> = {
  disk_low: 'min_free_disk_gb',
  hot: 'pause_when_hot',
  battery: 'pause_on_battery',
  screen_on: 'pause_when_screen_on',
  fullscreen: 'pause_on_fullscreen',
}

/** 与托盘菜单（src-tauri/src/tray.rs）的选项一致 */
const PAUSE_OPTIONS: { label: string; value: PauseFor }[] = [
  { label: '暂停 1 小时', value: 'hour' },
  { label: '暂停到明天', value: 'tomorrow' },
  { label: '直到手动恢复', value: 'manual' },
]

/** 手动暂停何时恢复，时刻写法与托盘菜单（src-tauri/src/tray.rs 的 manual_text）一致；直到手动恢复时为空 */
function resumeText(until: number | null, now: Date) {
  if (until == null) return ''
  // 已到期、核心还没清除（每 5 秒判定一次）
  if (until <= now.getTime()) return '，即将恢复'
  const t = new Date(until)
  const clock = `${t.getHours()}:${pad2(t.getMinutes())}`
  if (t.toDateString() === now.toDateString()) return `，到 ${clock} 恢复`
  return `，到 ${t.getMonth() + 1}月${t.getDate()}日 ${clock} 恢复`
}

/** 已处理的文件数（含失败的） */
function processed(s: IndexStatus) {
  return s.done + s.failed
}

/**
 * 本轮的基数（本轮开始时已处理的文件数），与托盘（src-tauri/src/tray.rs 的 next_base）一致：有待处理的文件时才有，
 * 没有时从当前的已处理数算起。失败数减少（重试了失败的文件）时，基数减去减少的量：已处理数不倒退，重试的文件计入本轮的总数
 */
export function nextBase(base: number | null, lastFailed: number | null, s: IndexStatus) {
  if (s.pending === 0) return null
  if (base === null) return processed(s)
  return Math.max(0, base - Math.max(0, (lastFailed ?? s.failed) - s.failed))
}

/** 本轮的进度：`base` 为本轮开始时已处理的文件数，只数这一轮实际要建的文件，不数之前已建好的 */
function round(s: IndexStatus, base: number) {
  return { done: Math.max(processed(s) - base, 0), total: Math.max(s.total - base, 0) }
}

/** 进度条的比例；没有进度条时为空：建立索引中，或按新设置重建中 */
function progress(s: IndexStatus, base: number) {
  const r = round(s, base)
  if (s.pending > 0 && r.total > 0) return r.done / r.total
  if (s.rebuild) return s.rebuild.done / s.rebuild.total
  return null
}

/**
 * 索引未完成或语义检索不可用时的提示；没有需要说明的情况时为空。
 * `device`：原本要用的设备出了问题而改用的设备，或用户没选只用 CPU 却用 CPU 时的 CPU，及原因（`indexDevice`），
 * 写成“正在用「{设备}」建立索引（原因）”。自动暂停时并没有在算，只说暂停原因、不说设备，免得两个括号叠在一起
 */
export function indexHint(s: IndexStatus, now = new Date(), base = 0, device: { name: string; why: string } | null = null) {
  const r = round(s, base)
  if (s.state === 'no_model') return s.error ?? '尚未下载文字模型，无法识别文件内容。可在「设置 → 模型」中下载或导入，之后会继续建立索引。'
  if (s.state === 'error') return s.error ?? '内容识别无法启动，目前只能按文件名搜索。请重新启动 Rummage，或在「设置 → 设备」中选择使用 CPU。'
  if (s.state === 'no_mmproj') return '尚未下载图片与音视频模型，图片、音频和视频目前只能按文件名搜索。可在「设置 → 模型」中下载或导入。'
  if (s.paused?.type === 'manual') {
    const resume = resumeText(s.paused.until, now)
    if (s.pending === 0) return `索引已暂停${resume}`
    return `索引已暂停：已处理 ${r.done} / ${r.total}${resume}，完成前，尚未处理的文件只能按文件名搜索`
  }
  const paused = s.paused ? `（${PAUSE_TEXT[s.paused.type]}）` : s.slow ? '（手机发热，已放慢）' : ''
  const eta = s.etaS != null ? `，预计还需${formatDuration(s.etaS)}` : ''
  const used = s.paused ? null : device
  const by = used ? `用「${used.name}」` : ''
  const why = used ? `（${used.why}）` : ''
  if (s.pending > 0) return `正在${by}建立索引${why}：已处理 ${r.done} / ${r.total}${paused}${eta}，完成前，尚未处理的文件只能按文件名搜索`
  // 新文件都处理完后才重建；重建期间旧索引照常可搜
  const rb = s.rebuild
  if (!rb) return null
  return `正在${by}按新设置重建索引${why}：已完成 ${rb.done} / ${rb.total}${paused}${eta}`
}

interface Props {
  status: IndexStatus | null
  /** 本轮的基数（`nextBase`），由搜索页记下：提示在页头与窗口底部之间移动时不重新计数 */
  base: number
  /** 点击“N 个文件失败”；失败文件弹窗也由搜索页管理，提示换位置时不关掉 */
  onFailed: () => void
  /** 点击“调整”：自动暂停或发热放慢时打开“设置 → 资源”，并滚动到参数 `key` */
  onAdjust: (key: string) => void
  /** 放在窗口底部时，暂停菜单向上展开 */
  up?: boolean
  /** 见 `indexHint` */
  device?: { name: string; why: string } | null
}

export default function IndexHint({ status, base, onFailed, onAdjust, up = false, device = null }: Props) {
  const [open, setOpen] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const menuRef = useRef<HTMLDivElement>(null)

  useClickOutside(menuRef, open, () => setOpen(false))

  const text = status && indexHint(status, new Date(), base, device)
  const failed = status?.failed ?? 0
  if (!status || (!text && !failed)) return null
  const ratio = progress(status, base)
  const running = ratio != null
  const manual = status.paused?.type === 'manual'
  // 缺模型、内容识别无法启动：需要用户处理，用提示框（与设置页的提示相同）
  const warn = status.state === 'no_model' || status.state === 'no_mmproj' || status.state === 'error'
  // 提示中写了暂停或放慢的原因时才有“调整”
  const adjust = !text || warn ? undefined : status.paused ? ADJUST_PARAM[status.paused.type] : status.slow ? 'slow_when_hot' : undefined

  // 暂停状态以核心为准，成功后由 core-event 中的 IndexProgress 更新
  const run = (cmd: string, args?: Record<string, unknown>) => {
    setOpen(false)
    setBusy(true)
    setError('')
    invoke(cmd, args)
      .catch((err) => {
        console.warn('暂停或恢复索引失败：', err)
        setError(errorMessage(err, '操作失败，请稍后再试。'))
      })
      .finally(() => setBusy(false))
  }

  // 手动暂停时的“恢复”：有进度条时在进度条右端，没有时跟在文字这一行的右端
  const resume = (
    <button
      type="button"
      disabled={busy}
      onClick={() => run('resume_indexing')}
      className="flex-shrink-0 rounded-md px-1.5 py-0.5 text-xs text-blue-500 transition hover:bg-black/[0.05] disabled:opacity-50 dark:text-blue-400 dark:hover:bg-white/[0.08]"
    >
      恢复
    </button>
  )

  return (
    <div className="px-1">
      {/* 有进度条时按钮在进度条右端，没有时跟在文字这一行的右端；没有文字（只剩失败文件）时居中。
          手机宽度下失败文件数单独一行，居中放在最上方 */}
      <div className={`flex items-center gap-2 max-sm:flex-wrap max-sm:gap-y-1 ${text ? '' : 'justify-center'}`}>
        {text && warn && (
          <div className={`min-w-0 flex-1 ${NOTICE_BOX}`}>
            <WarningIcon className="mt-px h-4 w-4 flex-shrink-0" />
            <span data-selectable-text>{renderMessage(text, 'font-semibold')}</span>
          </div>
        )}
        {text && !warn && <p className="min-w-0 flex-1 text-xs text-gray-400 dark:text-gray-500">{text}</p>}
        {adjust && (
          <button
            type="button"
            onClick={() => onAdjust(adjust)}
            className="flex-shrink-0 text-xs text-blue-500 underline underline-offset-2 transition hover:text-blue-600 dark:text-blue-400 dark:hover:text-blue-300"
          >
            调整
          </button>
        )}
        {failed > 0 && (
          <button
            type="button"
            onClick={onFailed}
            className="flex-shrink-0 text-xs text-blue-500 transition max-sm:order-first max-sm:basis-full max-sm:text-center hover:text-blue-600 dark:text-blue-400 dark:hover:text-blue-300"
          >
            {failed} 个文件失败
          </button>
        )}
        {!running && manual && resume}
      </div>
      {error && <p className="mt-1 text-xs text-red-500">{error}</p>}
      {running && (
        <div className="mt-1.5 flex items-center gap-2">
          <div className="h-1 flex-1 overflow-hidden rounded-full bg-black/[0.06] dark:bg-white/[0.08]">
            <div className="h-full rounded-full bg-blue-500/70 transition-all duration-200" style={{ width: `${ratio * 100}%` }} />
          </div>
          {manual ? (
            resume
          ) : (
            <div ref={menuRef} className="relative flex-shrink-0">
              <button
                type="button"
                aria-label="暂停索引"
                aria-haspopup="menu"
                aria-expanded={open}
                disabled={busy}
                onClick={() => setOpen(!open)}
                onKeyDown={(e) => {
                  if (e.key === 'Escape' && open) {
                    e.stopPropagation()
                    setOpen(false)
                  }
                }}
                className="flex rounded-md p-0.5 text-gray-400 transition hover:bg-black/[0.05] hover:text-gray-600 disabled:opacity-50 dark:text-gray-500 dark:hover:bg-white/[0.08] dark:hover:text-gray-300"
              >
                <PauseIcon className="h-4 w-4" />
                <HoverTip label="暂停索引" hide={open} />
              </button>
              {open && (
                <div
                  role="menu"
                  className={`absolute right-0 z-50 w-32 overflow-hidden rounded-xl border border-transparent bg-white/95 shadow-[0_8px_30px_rgb(0,0,0,0.12)] ring-1 ring-black/5 backdrop-blur-xl dark:bg-gray-800/95 dark:shadow-[0_8px_30px_rgb(0,0,0,0.4)] dark:ring-white/[0.06] ${up ? 'bottom-full mb-1.5 animate-dropdown-up' : 'top-full mt-1.5 animate-dropdown-down'}`}
                >
                  {PAUSE_OPTIONS.map((o) => (
                    <div
                      key={o.value}
                      role="menuitem"
                      onClick={() => run('pause_indexing', { until: o.value })}
                      className="flex min-h-9 cursor-pointer items-center px-3 py-2 text-xs text-gray-700 transition-colors hover:bg-gray-50 dark:text-gray-300 dark:hover:bg-white/[0.06]"
                    >
                      {o.label}
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  )
}
