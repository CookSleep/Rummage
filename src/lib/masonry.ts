// 搜索结果的瀑布流：卡片等宽，按匹配度顺序依次放进当前最短的一列；
// 位置完全由核心返回的宽高算出，不等缩略图加载。虚拟化与键盘移动都基于这里算出的位置

/** 图片卡片图片区域的最大高宽比，更长的图只显示顶部；对应 Rust `rummage_media::THUMB_MAX_RATIO` */
export const MAX_IMAGE_RATIO = 2
/** 卡片的最小宽度（px），列数由它和容器宽度决定 */
const MIN_COLUMN_WIDTH = 220
/** 卡片间距（px） */
export const GAP = 12
/** 图片卡片下方文件名、路径区域的高度（px） */
export const CAPTION_HEIGHT = 56
/** 文字卡片的高度（px） */
export const TEXT_CARD_HEIGHT = 128

export interface CardBox {
  x: number
  y: number
  width: number
  height: number
  column: number
}

export interface Masonry {
  boxes: CardBox[]
  /** 最长一列的高度 */
  height: number
  columns: number
  /** 横跨各列的分隔带（折叠区的标题）的顶部位置，没有分隔带时为空 */
  bandY: number | null
}

/** 横跨各列的分隔带：放在第 `at` 张卡片之前（可等于卡片数，放在最后），高 `height` */
export interface Band {
  at: number
  height: number
}

/** 显示缩略图的卡片：图片与视频（视频用系统缩略图） */
export function hasThumb(kind: string) {
  return kind === 'image' || kind === 'video'
}

export function columnCount(width: number, minColumn = MIN_COLUMN_WIDTH, gap = GAP) {
  return Math.max(1, Math.floor((width + gap) / (minColumn + gap)))
}

/**
 * 卡片在给定列宽下的高度：图片与视频按 `size` 的宽高比（原图，长图命中某一段时为那一段）（未知时图片按正方形、视频按 16:9），超过最大高宽比时截断；
 * 其他文件为固定高度的文字卡片
 */
export function cardHeight(size: { width: number | null; height: number | null }, columnWidth: number, kind: string) {
  if (!hasThumb(kind)) return TEXT_CARD_HEIGHT
  const ratio = size.width && size.height ? size.height / size.width : kind === 'video' ? 9 / 16 : 1
  return Math.round(columnWidth * Math.min(ratio, MAX_IMAGE_RATIO)) + CAPTION_HEIGHT
}

/**
 * 依次把高度为 `heights[i]` 的卡片放进当前最短的一列，同样短时取左边的列。
 * 有分隔带时，带之前的卡片排完后各列对齐到最长的一列，分隔带之后的卡片从带下方重新开始
 */
export function layoutMasonry(heights: number[], width: number, columns: number, gap = GAP, band?: Band): Masonry {
  const columnWidth = (width - (columns - 1) * gap) / columns
  const bottoms: number[] = new Array(columns).fill(0)
  let bandY: number | null = null
  const placeBand = () => {
    bandY = Math.max(...bottoms)
    bottoms.fill(bandY + band!.height + gap)
  }
  const boxes = heights.map((h, i) => {
    if (band && i === band.at) placeBand()
    const column = bottoms.indexOf(Math.min(...bottoms))
    const box = { x: column * (columnWidth + gap), y: bottoms[column], width: columnWidth, height: h, column }
    bottoms[column] += h + gap
    return box
  })
  if (band && band.at >= heights.length) placeBand()
  return { boxes, height: Math.max(0, Math.max(...bottoms) - gap), columns, bandY }
}

/** 与 [top, bottom] 相交的卡片下标，按原顺序；调用方自行在上下各留出余量 */
export function visibleIndexes(boxes: CardBox[], top: number, bottom: number) {
  const out: number[] = []
  boxes.forEach((b, i) => {
    if (b.y < bottom && b.y + b.height > top) out.push(i)
  })
  return out
}

export type Direction = 'up' | 'down' | 'left' | 'right'

/**
 * 按卡片的实际位置移动选择：上下在同一列内移动；左右跳到相邻列（该方向上最近的非空列）中
 * 垂直中心最接近的卡片，同样近时取上面的。没有可去的卡片时返回原下标
 */
export function moveSelection(boxes: CardBox[], from: number, dir: Direction) {
  const cur = boxes[from]
  if (!cur) return from
  const center = (b: CardBox) => b.y + b.height / 2
  if (dir === 'up' || dir === 'down') {
    let best = from
    boxes.forEach((b, i) => {
      if (b.column !== cur.column) return
      const ok = dir === 'down' ? b.y > cur.y : b.y < cur.y
      if (!ok) return
      if (best === from || (dir === 'down' ? b.y < boxes[best].y : b.y > boxes[best].y)) best = i
    })
    return best
  }
  const step = dir === 'right' ? 1 : -1
  const columns = Math.max(...boxes.map((b) => b.column)) + 1
  for (let col = cur.column + step; col >= 0 && col < columns; col += step) {
    let best = -1
    boxes.forEach((b, i) => {
      if (b.column !== col) return
      const d = Math.abs(center(b) - center(cur))
      const bestD = best < 0 ? Infinity : Math.abs(center(boxes[best]) - center(cur))
      if (d < bestD || (d === bestD && b.y < boxes[best].y)) best = i
    })
    if (best >= 0) return best
  }
  return from
}
