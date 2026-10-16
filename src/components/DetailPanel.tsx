import { useEffect, useRef, useState, type CSSProperties, type ReactNode } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { FileInfo, SearchHit } from '../types'
import { copyText } from '../lib/clipboard'
import { errorMessage } from '../lib/errors'
import { canFindSimilar, formatSize, formatTime, indexStateText, kindLabel, matchPercent, matchText, openAtText } from '../lib/format'
import { hitPosition } from '../lib/media'
import { isTauri } from '../lib/platform'
import { useCoreEvent } from '../hooks/useCoreEvent'
import { renderMessage } from './ConfirmDialog'
import { CloseIcon, FolderIcon, KindIcon, OpenIcon, PanelCollapseIcon, ShareIcon, SimilarIcon } from './icons'
import Preview from './Preview'
import { HoverTip } from './Tip'
import PathText from './PathText'
import { showToast } from './Toast'

/** 详情栏宽度（px）；窗口不窄于 `PANEL_OVERLAY_BELOW` 时详情栏常驻右侧、网格让出这么宽，否则选中结果后覆盖整个窗口 */
export const PANEL_WIDTH = 400
export const PANEL_OVERLAY_BELOW = 900
/** 建索引期间刷新详情的最短间隔（ms） */
const REFRESH_MS = 2000

/** 点击文件名或路径时复制；在其中拖选了文字时不复制，留给用户复制选中的部分 */
async function copyField(el: HTMLElement, text: string, what: string) {
  const sel = window.getSelection()
  if (sel && !sel.isCollapsed && sel.containsNode(el, true)) return
  try {
    await copyText(text)
    showToast(`${what}已复制`, 'success')
  } catch (err) {
    console.warn(err)
    showToast(`复制${what}失败`, 'error')
  }
}

const COPY_TEXT = 'cursor-pointer text-left transition duration-200 hover:text-blue-500 dark:hover:text-blue-400'

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex gap-3 py-1 text-xs">
      <dt className="w-14 flex-shrink-0 text-gray-400 dark:text-gray-500">{label}</dt>
      <dd className="min-w-0 flex-1 text-gray-700 [overflow-wrap:anywhere] dark:text-gray-200">{children}</dd>
    </div>
  )
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section>
      <h3 className="mb-1 text-xs font-medium text-gray-500 dark:text-gray-400">{title}</h3>
      <dl>{children}</dl>
    </section>
  )
}

interface DetailsProps {
  hit: SearchHit
  query: string
  onOpen: () => void
  /** 为空时不显示“在资源管理器中显示”（Android） */
  onReveal?: () => void
  onSimilar: () => void
  /** 为空时不显示“分享”（Windows） */
  onShare?: () => void
}

/** 选中结果的预览、文件信息与索引情况 */
function Details({ hit, query, onOpen, onReveal, onSimilar, onShare }: DetailsProps) {
  const [info, setInfo] = useState<FileInfo | null>(null)
  // 默认程序能跳到命中位置时按钮写“打开到 03:12”，打开时由 SearchPage 带上同一个位置
  const at = hitPosition(hit)
  const [canJump, setCanJump] = useState(false)
  const [error, setError] = useState<string | null>(null)
  // 建索引期间按进度事件递增，触发重新读取详情
  const [tick, setTick] = useState(0)
  const lastFetch = useRef(0)

  useEffect(() => {
    if (!isTauri) return
    let active = true
    lastFetch.current = Date.now()
    invoke<FileInfo>('file_details', { fileId: hit.fileId })
      .then((i) => {
        if (!active) return
        setInfo(i)
        setError(null)
      })
      .catch((err) => {
        console.warn('无法读取文件详情：', err)
        if (active) setError(errorMessage(err, '无法读取文件详情。'))
      })
    return () => {
      active = false
    }
    // 换文件时整个组件按 key 重新挂载
  }, [tick])

  useEffect(() => {
    if (!isTauri || !at) return
    let active = true
    invoke<boolean>('can_open_at', { fileId: hit.fileId })
      .then((ok) => {
        if (active) setCanJump(ok)
      })
      .catch((err) => console.warn('无法查询默认程序：', err))
    return () => {
      active = false
    }
    // 只看有没有位置；同一个文件的默认程序不随命中位置变化
  }, [hit.fileId, Boolean(at)])

  // 索引进度变化时刷新当前文件的状态，最多每 REFRESH_MS 一次，间隔内到达的事件合并到间隔末尾
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)
  useCoreEvent((ev) => {
    if (ev.type !== 'IndexProgress' || timer.current) return
    const wait = Math.max(0, lastFetch.current + REFRESH_MS - Date.now())
    timer.current = setTimeout(() => {
      timer.current = null
      setTick((t) => t + 1)
    }, wait)
  })
  useEffect(() => () => {
    if (timer.current) clearTimeout(timer.current)
  }, [])

  const match = matchText(hit)
  const width = info?.width ?? hit.width
  const height = info?.height ?? hit.height

  return (
    <>
      <div className="flex min-h-0 flex-1 flex-col gap-5 overflow-y-auto overscroll-contain px-5 pb-5 [scrollbar-gutter:stable_both-edges]">
        <Preview hit={hit} query={query} />

        <Section title="文件">
          <Row label="路径">
            <button type="button" data-selectable-text onClick={(e) => void copyField(e.currentTarget, hit.path, '路径')} className={COPY_TEXT}>
              <PathText path={hit.path} />
              <HoverTip label="复制路径" />
            </button>
          </Row>
          <Row label="类型">{kindLabel(hit.kind)}{info?.ext ? `（.${info.ext}）` : ''}</Row>
          <Row label="大小">{formatSize(info?.size ?? hit.size)}</Row>
          <Row label="修改时间">{formatTime(info?.mtime ?? hit.mtime)}</Row>
          {(hit.kind === 'image' || hit.kind === 'video') && width && height ? <Row label="尺寸">{width} × {height}</Row> : null}
          {hit.offline && <Row label="位置">设备未连接</Row>}
          {hit.cloud && <Row label="位置">仅在云端</Row>}
        </Section>

        <Section title="索引">
          {match && <Row label="匹配">{match}</Row>}
          {matchPercent(hit) && <Row label="匹配度">{matchPercent(hit)}</Row>}
          {error ? (
            <Row label="状态">{renderMessage(error, 'font-semibold')}</Row>
          ) : !info ? (
            <Row label="状态">读取中</Row>
          ) : (
            <>
              <Row label="状态">{renderMessage(indexStateText(info), 'font-semibold')}</Row>
              {info.indexedAt !== null && <Row label="索引时间">{formatTime(info.indexedAt)}</Row>}
              {info.segments > 0 && <Row label="分段">{info.segments} 段</Row>}
            </>
          )}
        </Section>
      </div>

      <div className="flex items-center gap-1 border-t border-black/5 px-5 pb-[calc(0.75rem+var(--safe-bottom))] pt-3 dark:border-white/[0.06]">
        {canFindSimilar(hit) && (
          <button type="button" onClick={onSimilar} aria-label="找相似" className={ICON_BUTTON}>
            <SimilarIcon kind={hit.kind} className="h-4 w-4" />
            <HoverTip label="找相似" />
          </button>
        )}
        {onShare && (
          <button type="button" onClick={onShare} aria-label="分享" className={ICON_BUTTON}>
            <ShareIcon className="h-4 w-4" />
            <HoverTip label="分享" />
          </button>
        )}
        {onReveal && (
          <button type="button" onClick={onReveal} aria-label="在资源管理器中显示" className={ICON_BUTTON}>
            <FolderIcon className="h-4 w-4" />
            <HoverTip label="在资源管理器中显示" />
          </button>
        )}
        <button type="button" onClick={onOpen} className="ml-auto inline-flex flex-shrink-0 items-center gap-1.5 whitespace-nowrap rounded-xl bg-blue-500 px-4 py-2 text-sm font-medium text-white transition duration-200 hover:bg-blue-600">
          <OpenIcon className="h-4 w-4" />
          {at && canJump ? openAtText(at) : '打开'}
        </button>
      </div>
    </>
  )
}

/** 底部只有图标的次要按钮，说明在气泡里 */
const ICON_BUTTON = 'rounded-xl p-2.5 text-gray-600 transition duration-200 hover:bg-black/5 dark:text-gray-300 dark:hover:bg-white/10'

interface Props {
  /** 选中的结果；为空时显示占位文字 */
  hit: SearchHit | undefined
  /** 文字查询，文档预览中高亮其中的词；以图搜图与“找相似的”为空 */
  query: string
  /** 窄窗口时为覆盖整个窗口的详情页，按钮为关闭；否则为右侧常驻、按钮为收起 */
  overlay: boolean
  /** 窗口高度，长图预览据此定高 */
  viewportHeight: number
  /** 后台服务是否已连接；变化时详情与预览（片段正文、图片、音视频）重新挂载、重新读取 */
  connected: boolean
  onHide: () => void
  onOpen: () => void
  /** 为空时不显示“在资源管理器中显示”（Android） */
  onReveal?: () => void
  onSimilar: () => void
  /** 为空时不显示“分享”（Windows） */
  onShare?: () => void
}

/**
 * 右侧详情栏：不是弹窗，不抢焦点。宽窗口时有结果就常驻（可收起），单击只更新内容、不改变布局，
 * 双击就是指针下那张卡片的原生双击；窄窗口时选中结果后覆盖整个窗口，内部滚动
 */
export default function DetailPanel({ hit, query, overlay, viewportHeight, connected, onHide, onOpen, onReveal, onSimilar, onShare }: Props) {
  // 长图预览的高度（px，Preview 的 `--long-image-max`）：除去详情栏的标题行与底部按钮（约 140），再留出下面“文件”一节的标题和第一行，
  // 不把文件信息完全挤出可见范围。详情栏可以滚动，窗口很矮时仍保留 320
  const longImageMax = Math.max(320, viewportHeight - 220)
  return (
    <aside
      aria-label="详情"
      style={{ ...(overlay ? {} : { width: PANEL_WIDTH }), '--long-image-max': `${longImageMax}px` } as CSSProperties & { '--long-image-max': string }}
      className={`fixed bottom-0 right-0 top-0 z-30 flex max-w-full flex-col ${
        overlay ? 'left-0 bg-gray-50 dark:bg-gray-950' : 'border-l border-white/50 bg-white/90 backdrop-blur-xl ring-1 ring-black/5 dark:border-white/[0.08] dark:bg-gray-900/90 dark:ring-white/10'
      }`}
    >
      {/* 收起按钮与收起后的“展开详情栏”按钮同位置、同大小 */}
      <div className="flex items-start gap-3 pb-3 pl-5 pr-3 pt-[calc(1.25rem+var(--safe-top))]">
        {hit && (
          <>
            <KindIcon kind={hit.kind} className="mt-2 h-5 w-5 flex-shrink-0 text-gray-400" />
            <button type="button" onClick={(e) => void copyField(e.currentTarget, hit.name, '文件名')} className={`mt-2 min-w-0 flex-1 break-all text-sm font-semibold text-gray-800 dark:text-gray-100 ${COPY_TEXT}`}>
              {hit.name}
              <HoverTip label="复制文件名" />
            </button>
          </>
        )}
        <button
          type="button"
          onClick={onHide}
          aria-label={overlay ? '关闭详情' : '收起详情栏'}
          className="ml-auto flex-shrink-0 rounded-full border border-transparent p-2 text-gray-400 transition duration-200 hover:bg-black/5 hover:text-gray-600 dark:hover:bg-white/10 dark:hover:text-gray-200"
        >
          {overlay ? <CloseIcon className="h-4 w-4" /> : <PanelCollapseIcon className="h-4 w-4" />}
          <HoverTip label={overlay ? '关闭（Esc）' : '收起（空格）'} />
        </button>
      </div>
      {hit ? (
        <Details key={`${hit.fileId}-${connected}`} hit={hit} query={query} onOpen={onOpen} onReveal={onReveal} onSimilar={onSimilar} onShare={onShare} />
      ) : (
        <p className="flex flex-1 items-center justify-center px-5 pb-16 text-sm text-gray-400 dark:text-gray-500">选择一个结果查看详情</p>
      )}
    </aside>
  )
}
