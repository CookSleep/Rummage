import { useEffect, useLayoutEffect, useMemo, useState, type ReactNode, type RefObject } from 'react'
import type { SearchHit } from '../types'
import { cardHeight, columnCount, GAP, layoutMasonry, visibleIndexes, type Masonry } from '../lib/masonry'
import { canFindSimilar } from '../lib/format'
import { hitEmbed, hitRegion } from '../lib/media'
import ResultCard from './ResultCard'

interface Props {
  hits: SearchHit[]
  selected: number | null
  /** 需保持引用稳定，卡片按下标回调 */
  onSelect: (idx: number) => void
  /** 双击打开，同样需保持引用稳定 */
  onOpen: (idx: number) => void
  /** 找相似的，同样需保持引用稳定 */
  onSimilar: (idx: number) => void
  /** 网格容器，父组件据此把选中的卡片滚动到可视区域 */
  gridRef: RefObject<HTMLDivElement | null>
  /** 布局变化时通知父组件，键盘移动选择用 */
  onLayout: (layout: Masonry) => void
  /** 后台服务是否连上，卡片据此重试取图 */
  connected: boolean
  /** 横跨各列的分隔带（折叠区的标题），放在第 `at` 个结果之前 */
  band?: { at: number; node: ReactNode }
}

/** 分隔带的高度（px） */
const BAND_HEIGHT = 40

/**
 * 虚拟化的瀑布流：随窗口滚动，只渲染视口及上下各一屏内的卡片，容器高度由布局算出。
 * 列数随容器宽度变化，卡片位置不依赖缩略图加载
 */
export default function ResultGrid({ hits, selected, onSelect, onOpen, onSimilar, gridRef, onLayout, connected, band }: Props) {
  const [width, setWidth] = useState(0)
  const [view, setView] = useState({ top: 0, bottom: window.innerHeight })

  useLayoutEffect(() => {
    const grid = gridRef.current
    if (!grid) return
    // 视口位置按半屏取整，只在跨过取整边界时更新状态，滚动时不必每帧重渲染；上下各多渲染约一屏
    const update = () => {
      const vh = window.innerHeight
      const step = Math.max(1, Math.round(vh / 2))
      const base = Math.floor(-grid.getBoundingClientRect().top / step) * step
      setView((prev) => (prev.top === base - vh && prev.bottom === base + 2 * vh ? prev : { top: base - vh, bottom: base + 2 * vh }))
    }
    const measure = () => {
      setWidth(grid.clientWidth)
      update()
    }
    measure()
    const ro = new ResizeObserver(measure)
    ro.observe(grid)
    window.addEventListener('scroll', update, { passive: true })
    window.addEventListener('resize', update)
    return () => {
      ro.disconnect()
      window.removeEventListener('scroll', update)
      window.removeEventListener('resize', update)
    }
  }, [gridRef])

  const bandAt = band?.at
  const layout = useMemo(() => {
    const columns = columnCount(width)
    const columnWidth = (width - (columns - 1) * GAP) / columns
    const b = bandAt === undefined ? undefined : { at: bandAt, height: BAND_HEIGHT }
    // 长图命中某一段时卡片显示那一段的裁剪图，按那一段的宽高比排；文档中内嵌的图片或视频命中时按图片卡片排
    const heights = hits.map((h) => {
      const embed = hitEmbed(h)
      return embed ? cardHeight(embed, columnWidth, 'image') : cardHeight(hitRegion(h) ?? h, columnWidth, h.kind)
    })
    return layoutMasonry(heights, width, columns, GAP, b)
  }, [hits, width, bandAt])

  useEffect(() => onLayout(layout), [layout, onLayout])

  const visible = visibleIndexes(layout.boxes, view.top, view.bottom)

  return (
    <div ref={gridRef} role="listbox" aria-label="搜索结果" tabIndex={-1} className="relative w-full outline-none" style={{ height: layout.height }}>
      {band && layout.bandY !== null && (
        <div className="absolute inset-x-0 flex items-center" style={{ top: layout.bandY, height: BAND_HEIGHT }}>{band.node}</div>
      )}
      {width > 0 && visible.map((i) => (
        <ResultCard key={hits[i].fileId} hit={hits[i]} box={layout.boxes[i]} index={i} selected={i === selected} onSelect={onSelect} onOpen={onOpen} onSimilar={canFindSimilar(hits[i]) ? onSimilar : undefined} connected={connected} />
      ))}
    </div>
  )
}
