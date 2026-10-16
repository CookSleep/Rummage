import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { SearchHit, SegmentPreview } from '../types'
import { formatClock, highlight, locatorText } from '../lib/format'
import { canPlay, cropUrl, embedOf, frameUrl, hitFrameMs, hitRegion, imageUrl, playUrl, thumbUrl, timeRange } from '../lib/media'
import { isTauri } from '../lib/platform'
import { HoverTip } from './Tip'

/** 文档预览显示的命中片段数 */
const MAX_SEGMENTS = 3
/** 选中停留这么久（ms）才加载原图或音视频，方向键快速移动时不为每个结果都去读文件 */
const SETTLE_MS = 300

/** 选中在同一个结果上停留 `SETTLE_MS` 后变为真；组件按文件 ID 重建，切换结果时计时随之取消 */
function useSettled() {
  const [settled, setSettled] = useState(false)
  useEffect(() => {
    const timer = setTimeout(() => setSettled(true), SETTLE_MS)
    return () => clearTimeout(timer)
  }, [])
  return settled
}

function Note({ children }: { children: ReactNode }) {
  return <p className="rounded-xl bg-black/[0.03] px-3 py-6 text-center text-xs text-gray-400 dark:bg-white/[0.04]">{children}</p>
}

function Marked({ text, query }: { text: string; query: string }) {
  return (
    <>
      {highlight(text, query).map((p, i) =>
        p.hit ? <mark key={i} className="rounded-sm bg-amber-200/80 text-inherit dark:bg-amber-400/30">{p.text}</mark> : p.text,
      )}
    </>
  )
}

/** 文档中内嵌的图片或视频：位置与它的画面；音频只显示位置 */
function EmbedSegment({ hit, segmentId, locatorJson }: { hit: SearchHit; segmentId: number; locatorJson: string }) {
  const [failed, setFailed] = useState(false)
  const embed = embedOf(locatorJson)
  return (
    <div className="flex flex-col gap-1">
      <p className="text-xs text-blue-500/80 dark:text-blue-400/80">{locatorText(locatorJson)}</p>
      {embed?.kind !== 'audio' && (failed ? (
        <Note>无法显示这个画面，可以用系统程序打开文档</Note>
      ) : (
        <img src={cropUrl(hit, segmentId)} alt={hit.name} onError={() => setFailed(true)} className="mx-auto max-h-[45vh] rounded-xl object-contain" />
      ))}
    </div>
  )
}

/** 一个命中片段：位置（页码、章节），前后各一段上下文（颜色淡一些），命中部分高亮查询中的词，打开时滚到命中部分 */
function Segment({ segmentId, query }: { segmentId: number; query: string }) {
  const [seg, setSeg] = useState<SegmentPreview | null>(null)
  const [state, setState] = useState<'loading' | 'loaded' | 'missing' | 'failed'>('loading')
  const boxRef = useRef<HTMLPreElement>(null)
  const hitRef = useRef<HTMLSpanElement>(null)

  useEffect(() => {
    if (!isTauri) return
    let active = true
    invoke<SegmentPreview | null>('segment_text', { segmentId })
      .then((s) => {
        if (!active) return
        setSeg(s)
        setState(s ? 'loaded' : 'missing')
      })
      .catch((err) => {
        console.warn('无法读取片段正文：', err)
        if (active) setState('failed')
      })
    return () => {
      active = false
    }
  }, [segmentId])

  useLayoutEffect(() => {
    if (boxRef.current && hitRef.current && seg?.before) boxRef.current.scrollTop = Math.max(0, hitRef.current.offsetTop - 24)
  }, [seg])

  const loc = seg ? locatorText(seg.locatorJson) : ''
  const context = 'text-gray-400 dark:text-gray-500'
  return (
    <div className="flex flex-col gap-1">
      {loc && <p className="text-xs text-blue-500/80 dark:text-blue-400/80">{loc}</p>}
      <pre
        ref={boxRef}
        data-selectable-text
        className="relative max-h-56 overflow-auto whitespace-pre-wrap break-words rounded-xl bg-black/[0.03] p-3 font-mono text-xs leading-relaxed text-gray-700 [scrollbar-gutter:stable] dark:bg-white/[0.04] dark:text-gray-200"
      >
        {state === 'failed' ? '内容读取失败' : state === 'missing' ? '这段内容已不在索引中' : !seg ? '加载中' : (
          <>
            {seg.before && <span className={context}><Marked text={seg.before} query={query} />{'\n\n'}</span>}
            <span ref={hitRef}><Marked text={seg.text ?? ''} query={query} /></span>
            {seg.after && <span className={context}>{'\n\n'}<Marked text={seg.after} query={query} /></span>}
          </>
        )}
      </pre>
    </div>
  )
}

/** 要预览的片段：正文关键词命中的第一段放最前（查询中的词就在里面），再是内容最匹配的几段，去重 */
function previewSegments(hit: SearchHit) {
  const ids = [...hit.textSegments.slice(0, 1), ...hit.segments.map((s) => s.segmentId), ...hit.textSegments.slice(1)]
  return [...new Set(ids)].slice(0, MAX_SEGMENTS)
}

/**
 * 图片大图：先显示已缓存的缩略图；停留后再加载原图（WebView 不支持的格式由协议退回缩略图），
 * 原图加载完才替换缩略图，失败时保留缩略图
 */
function ImagePreview({ hit }: { hit: SearchHit }) {
  const wantOriginal = useSettled()
  const [original, setOriginal] = useState<'loading' | 'loaded' | 'failed'>('loading')
  const [thumbFailed, setThumbFailed] = useState(false)

  if (thumbFailed && original === 'failed') return <Note>无法显示这张图片，可以用系统程序打开</Note>
  const cls = 'mx-auto max-h-[45vh] rounded-xl object-contain'
  return (
    <>
      {original !== 'loaded' && !thumbFailed && <img src={thumbUrl(hit)} alt={hit.name} onError={() => setThumbFailed(true)} className={cls} />}
      {wantOriginal && original !== 'failed' && (
        <img
          src={imageUrl(hit.fileId)}
          alt={hit.name}
          onLoad={() => setOriginal('loaded')}
          onError={() => setOriginal('failed')}
          className={original === 'loaded' ? cls : 'hidden'}
        />
      )}
    </>
  )
}

/**
 * 长图命中某一段：直接加载原图，显示成可以滚动的整图，滚到那一段并框出来。
 * 原图读不了或不是原图（WebView 不支持的格式退回的是截过的缩略图，宽高比对不上）时改为显示那一段的裁剪图。
 * 高度固定为详情栏给出的 `--long-image-max`：详情栏内容是可滚动的纵向弹性布局，不固定会被压扁
 */
function LongImagePreview({ hit, region }: { hit: SearchHit; region: NonNullable<ReturnType<typeof hitRegion>> }) {
  const [size, setSize] = useState<{ w: number; h: number } | null>(null)
  const [originalFailed, setOriginalFailed] = useState(false)
  const [cropFailed, setCropFailed] = useState(false)
  const boxRef = useRef<HTMLDivElement>(null)
  // 横向的长图横着滚动；切片可能是正方形，按整图的宽高判断
  const wide = hit.width && hit.height ? hit.width > hit.height : region.width > region.height

  useLayoutEffect(() => {
    const box = boxRef.current
    if (!box || !size) return
    const img = box.querySelector('img')
    if (!img) return
    const scale = img.clientWidth / size.w
    box.scrollTop = region.y * scale - (box.clientHeight - region.height * scale) / 2
    box.scrollLeft = region.x * scale - (box.clientWidth - region.width * scale) / 2
    // region 每次重渲染都是新对象，按片段判断；否则建立索引期间的重渲染会把用户的滚动拉回去
  }, [size, region.segmentId])

  const onLoad = (img: HTMLImageElement) => {
    const w = img.naturalWidth
    const h = img.naturalHeight
    const known = hit.width && hit.height ? hit.height / hit.width : h / w
    if (Math.abs(h / w - known) > known * 0.02 || region.x + region.width > w || region.y + region.height > h) {
      setOriginalFailed(true)
      return
    }
    setSize({ w, h })
  }

  if (originalFailed) {
    if (cropFailed) return <Note>无法显示这张图片，可以用系统程序打开</Note>
    return (
      <div className="flex h-[var(--long-image-max)] flex-shrink-0 items-center justify-center">
        <img src={cropUrl(hit, region.segmentId)} alt={hit.name} onError={() => setCropFailed(true)} className="max-h-full max-w-full rounded-xl object-contain" />
      </div>
    )
  }
  const pct = (v: number, total: number) => `${(v / total) * 100}%`
  return (
    <div ref={boxRef} className="h-[var(--long-image-max)] flex-shrink-0 overflow-auto rounded-xl bg-black/[0.03] [scrollbar-gutter:stable] dark:bg-white/[0.04]">
      <div className={`relative ${wide ? 'w-max' : ''} ${size ? '' : 'invisible'}`}>
        <img
          src={imageUrl(hit.fileId)}
          alt={hit.name}
          onLoad={(e) => onLoad(e.currentTarget)}
          onError={() => setOriginalFailed(true)}
          className={wide ? 'block h-[calc(var(--long-image-max)-8px)] w-auto max-w-none' : 'block w-full'}
        />
        {size && (
          <div
            className="pointer-events-none absolute rounded-md bg-blue-500/10 ring-2 ring-blue-500"
            style={{ left: pct(region.x, size.w), top: pct(region.y, size.h), width: pct(region.width, size.w), height: pct(region.height, size.h) }}
          />
        )}
      </div>
    </div>
  )
}

/**
 * 音频与视频：WebView 能播放的格式在应用内播放，打开时停在最匹配那一段的起点；视频的封面与不能播放时显示的
 * 都是命中时刻的画面（没有时用缩略图）。下面列出命中的时间段，能播放时点一下就从那里播放
 */
function MediaPreview({ hit }: { hit: SearchHit }) {
  const settled = useSettled()
  const [playFailed, setPlayFailed] = useState(false)
  const [frameFailed, setFrameFailed] = useState(0)
  const mediaRef = useRef<HTMLVideoElement & HTMLAudioElement>(null)
  const ranges = hit.segments.slice(0, MAX_SEGMENTS).flatMap((s) => {
    const r = timeRange(s.locatorJson)
    return r ? [{ id: s.segmentId, ...r }] : []
  })
  const start = ranges[0]?.start ?? 0
  const video = hit.kind === 'video'
  const frameMs = hitFrameMs(hit)
  const still = frameMs !== null && frameFailed === 0 ? frameUrl(hit, frameMs) : frameFailed < 2 ? thumbUrl(hit) : null
  const playing = settled && canPlay(hit.name) && !playFailed

  const seek = (ms: number) => {
    const el = mediaRef.current
    if (!el) return
    el.currentTime = ms / 1000
    void el.play().catch(() => {})
  }

  const media = {
    ref: mediaRef,
    src: playUrl(hit.fileId),
    controls: true,
    preload: 'metadata' as const,
    onLoadedMetadata: () => {
      if (mediaRef.current) mediaRef.current.currentTime = start / 1000
    },
    onError: () => setPlayFailed(true),
  }
  return (
    <div className="flex flex-col gap-3">
      {video && (playing ? (
        <video {...media} poster={still ?? undefined} className="max-h-[45vh] w-full rounded-xl bg-black" />
      ) : still ? (
        <img src={still} alt={hit.name} onError={() => setFrameFailed((n) => n + (frameMs !== null ? 1 : 2))} className="mx-auto max-h-[45vh] rounded-xl object-contain" />
      ) : null)}
      {!video && playing && <audio {...media} className="w-full" />}
      {ranges.length > 0 && (
        <div className="flex flex-col gap-1.5">
          <p className="text-xs text-gray-500 dark:text-gray-400">命中的时间段</p>
          <ul className="flex flex-wrap gap-2">
            {ranges.map((r) => {
              const text = `${formatClock(r.start)}–${formatClock(r.end)}`
              const cls = 'rounded-lg bg-black/[0.04] px-2.5 py-1 text-xs tabular-nums text-blue-600 dark:bg-white/[0.06] dark:text-blue-400'
              return (
                <li key={r.id}>
                  {playing ? (
                    <button type="button" onClick={() => seek(r.start)} className={`${cls} transition duration-200 hover:bg-black/[0.08] dark:hover:bg-white/[0.1]`}>
                      {text}
                      <HoverTip label="从这里播放" />
                    </button>
                  ) : (
                    <span className={`inline-block ${cls}`}>{text}</span>
                  )}
                </li>
              )
            })}
          </ul>
        </div>
      )}
    </div>
  )
}

/** 详情栏顶部的预览；`query` 为文字查询，文档预览中高亮其中的词 */
export default function Preview({ hit, query }: { hit: SearchHit; query: string }) {
  // 占位文件与离线设备上的文件不预览
  if (hit.offline) return <Note>文件所在的设备未连接，无法预览</Note>
  if (hit.cloud) return <Note>文件仅在云端，打开时由系统下载，这里不预览</Note>
  if (hit.kind === 'image') {
    const region = hitRegion(hit)
    return region ? <LongImagePreview hit={hit} region={region} /> : <ImagePreview hit={hit} />
  }
  if (hit.kind === 'audio' || hit.kind === 'video') return <MediaPreview hit={hit} />
  const ids = previewSegments(hit)
  if (!ids.length) return <Note>这个结果来自文件名匹配，没有可预览的内容</Note>
  // 内嵌媒体的片段没有正文，直接显示它的画面
  const embedded = new Map(hit.segments.filter((s) => embedOf(s.locatorJson)).map((s) => [s.segmentId, s.locatorJson]))
  return (
    <div className="flex flex-col gap-3">
      {ids.map((id) => {
        const loc = embedded.get(id)
        return loc ? <EmbedSegment key={id} hit={hit} segmentId={id} locatorJson={loc} /> : <Segment key={id} segmentId={id} query={query} />
      })}
    </div>
  )
}
