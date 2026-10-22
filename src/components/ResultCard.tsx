import { memo, useState } from 'react'
import type { SearchHit } from '../types'
import type { CardBox } from '../lib/masonry'
import { CAPTION_HEIGHT, hasThumb } from '../lib/masonry'
import { abbreviateDir, kindLabel, locatorText, matchPercent, matchText } from '../lib/format'
import { hitEmbed, hitImageUrl, hitRegion, thumbUrl } from '../lib/media'
import { CloudIcon, KindIcon, SimilarIcon, UnplugIcon } from './icons'
import { HoverTip } from './Tip'

/** 网盘占位文件与设备未连接的标记 */
function Flags({ hit }: { hit: SearchHit }) {
  if (!hit.cloud && !hit.offline) return null
  return (
    <span className="inline-flex items-center gap-1 rounded-full bg-black/55 px-2 py-0.5 text-[11px] font-medium text-white backdrop-blur-sm">
      {hit.offline ? <UnplugIcon className="h-3 w-3" /> : <CloudIcon className="h-3 w-3" />}
      {hit.missing ? '找不到文件夹' : hit.offline ? '设备未连接' : '仅在云端'}
    </span>
  )
}

/** 匹配度，跟在说明文字的右端 */
function Score({ hit }: { hit: SearchHit }) {
  const pct = matchPercent(hit)
  if (!pct) return null
  return <span className="flex-shrink-0 text-xs font-medium tabular-nums text-blue-500/90 dark:text-blue-400/90">{pct}</span>
}

/**
 * 悬停时显示的“找相似的”按钮（触摸屏上手指按在卡片上时显示）；卡片不在 Tab 顺序中，键盘用详情栏中的同名按钮。
 * 点击不选中卡片，双击不打开文件
 */
function SimilarButton({ kind, onClick, className }: { kind: string; onClick: () => void; className: string }) {
  return (
    <button
      type="button"
      tabIndex={-1}
      aria-label="找相似"
      onClick={(e) => {
        e.stopPropagation()
        onClick()
      }}
      onDoubleClick={(e) => e.stopPropagation()}
      className={`absolute rounded-full bg-white/90 p-1.5 text-gray-500 opacity-0 shadow-sm backdrop-blur-sm transition duration-200 hover:text-blue-500 group-hover:opacity-100 dark:bg-gray-900/90 dark:text-gray-400 dark:hover:text-blue-400 ${className}`}
    >
      <SimilarIcon kind={kind} className="h-4 w-4" />
      <HoverTip label="找相似" />
    </button>
  )
}

interface Props {
  hit: SearchHit
  box: CardBox
  index: number
  selected: boolean
  onSelect: (index: number) => void
  onOpen: (index: number) => void
  /** 为空时不显示“找相似的”（只按文件名索引的文件没有内容可比） */
  onSimilar?: (index: number) => void
  /** 后台服务是否连上 */
  connected: boolean
}

// 虚拟化滚动时可视范围内的卡片反复重渲染，props 不变的卡片跳过
export default memo(function ResultCard({ hit, box, index, selected, onSelect, onOpen, onSimilar, connected }: Props) {
  // 0：命中处的图（长图那一段的裁剪图、视频命中时刻的画面、文档中内嵌的图片或视频），没有时直接是缩略图；1：缩略图；2：都失败，显示类型图标
  const [fallback, setFallback] = useState(0)
  const dir = abbreviateDir(hit.path)
  const match = matchText(hit)
  const loc = hit.kind !== 'image' && hit.segments[0] ? locatorText(hit.segments[0].locatorJson) : ''
  const region = hitRegion(hit) ?? hitEmbed(hit)
  const hitSrc = hitImageUrl(hit)
  // 同一文件在新搜索中命中另一段时，重新尝试命中处的图
  const [lastSrc, setLastSrc] = useState(hitSrc)
  if (hitSrc !== lastSrc) {
    setLastSrc(hitSrc)
    setFallback(0)
  }
  // 后台服务断开期间取图失败的，重新连上后再试
  const [wasConnected, setWasConnected] = useState(connected)
  if (connected !== wasConnected) {
    setWasConnected(connected)
    if (connected) setFallback(0)
  }
  const src = hitSrc && fallback === 0 ? hitSrc : thumbUrl(hit)
  // 占位文件和离线设备上的文件不生成缩略图
  const showThumb = !hit.cloud && !hit.offline && fallback < 2

  return (
    <div
      role="option"
      aria-selected={selected}
      onClick={() => onSelect(index)}
      onDoubleClick={() => onOpen(index)}
      style={{ left: box.x, top: box.y, width: box.width, height: box.height }}
      className={`group absolute overflow-hidden rounded-xl border bg-white transition-shadow duration-200 cursor-pointer hover:shadow-lg dark:bg-gray-900 dark:hover:bg-gray-800/80 ${
        selected ? 'border-blue-500 ring-2 ring-blue-500/40' : 'border-gray-200 dark:border-white/[0.08]'
      }`}
    >
      {hasThumb(hit.kind) || region ? (
        <>
          <div className="relative flex items-center justify-center bg-gray-100 dark:bg-black/20" style={{ height: box.height - CAPTION_HEIGHT }}>
            {showThumb ? (
              <img
                src={src}
                alt=""
                loading="lazy"
                decoding="async"
                onError={() => setFallback(hitSrc && fallback === 0 ? 1 : 2)}
                className="h-full w-full object-cover object-top"
              />
            ) : (
              <KindIcon kind={hit.kind} className="h-8 w-8 text-gray-300 dark:text-gray-600" />
            )}
            <span className="absolute left-2 top-2"><Flags hit={hit} /></span>
            {onSimilar && <SimilarButton kind={hit.kind} onClick={() => onSimilar(index)} className="right-2 top-2" />}
          </div>
          <div className="flex flex-col justify-center px-3" style={{ height: CAPTION_HEIGHT }}>
            <p className="truncate text-sm font-medium text-gray-800 dark:text-gray-100">{hit.name}</p>
            <div className="flex items-center gap-2">
              <p className="min-w-0 flex-1 truncate text-xs text-gray-400 dark:text-gray-500">
                {loc && <span className="text-blue-500/80 dark:text-blue-400/80">{loc} · </span>}
                {[dir, match].filter(Boolean).join(' · ')}
              </p>
              <Score hit={hit} />
            </div>
          </div>
        </>
      ) : (
        <div className="flex h-full flex-col gap-1 p-3">
          <div className="flex items-center gap-2 text-xs text-gray-400 dark:text-gray-500">
            <KindIcon kind={hit.kind} className="h-4 w-4 flex-shrink-0" />
            <span className="flex-shrink-0 whitespace-nowrap">{kindLabel(hit.kind)}</span>
            {match && <span className="truncate">· {match}</span>}
            <span className="ml-auto flex flex-shrink-0 items-center gap-2"><Flags hit={hit} /><Score hit={hit} /></span>
          </div>
          <p className="line-clamp-2 break-all text-sm font-medium text-gray-800 dark:text-gray-100">{hit.name}</p>
          <p className="truncate text-xs text-gray-400 dark:text-gray-500">{dir}</p>
          {loc && <p className="mt-auto truncate text-xs text-blue-500/80 dark:text-blue-400/80">{loc}</p>}
          {onSimilar && <SimilarButton kind={hit.kind} onClick={() => onSimilar(index)} className="bottom-2 right-2" />}
        </div>
      )}
    </div>
  )
})
