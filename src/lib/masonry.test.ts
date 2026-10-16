import { describe, expect, it } from 'vitest'
import { CAPTION_HEIGHT, MAX_IMAGE_RATIO, TEXT_CARD_HEIGHT, cardHeight, columnCount, layoutMasonry, moveSelection, visibleIndexes } from './masonry'

describe('columnCount', () => {
  it('按最小列宽和间距决定列数，至少一列', () => {
    expect(columnCount(100, 220, 12)).toBe(1)
    expect(columnCount(452, 220, 12)).toBe(2)
    expect(columnCount(451, 220, 12)).toBe(1)
    expect(columnCount(1000, 220, 12)).toBe(4)
    // 手机宽度下排成两列
    expect(columnCount(328)).toBe(2)
    expect(columnCount(444)).toBe(1)
  })
})

describe('cardHeight', () => {
  it('图片按原始宽高比，过长的截断，未知时按正方形', () => {
    expect(cardHeight({ width: 400, height: 300 }, 200, 'image')).toBe(150 + CAPTION_HEIGHT)
    expect(cardHeight({ width: 100, height: 1000 }, 200, 'image')).toBe(200 * MAX_IMAGE_RATIO + CAPTION_HEIGHT)
    expect(cardHeight({ width: null, height: null }, 200, 'image')).toBe(200 + CAPTION_HEIGHT)
    expect(cardHeight({ width: null, height: null }, 200, 'document')).toBe(TEXT_CARD_HEIGHT)
  })
})

describe('layoutMasonry', () => {
  it('按顺序放进当前最短的一列，同样短时取左边', () => {
    const m = layoutMasonry([100, 50, 80, 30, 40], 310, 3, 10)
    expect(m.boxes.map((b) => b.column)).toEqual([0, 1, 2, 1, 2])
    expect(m.boxes[0]).toEqual({ x: 0, y: 0, width: 96.66666666666667, height: 100, column: 0 })
    expect(m.boxes[3].y).toBe(60)
    expect(m.boxes[4].y).toBe(90)
    expect(m.boxes[2].x).toBeCloseTo(213.33, 2)
    // 最长一列：80 + 10 + 40
    expect(m.height).toBe(130)
  })

  it('空列表高度为 0', () => {
    expect(layoutMasonry([], 300, 2).height).toBe(0)
    expect(layoutMasonry([], 300, 2).bandY).toBeNull()
  })

  it('分隔带之后的卡片从带下方重新开始', () => {
    // 带之前：列 0 为 100，列 1 为 50；带从最长的一列下方（110）开始，高 40
    const m = layoutMasonry([100, 50, 30, 20], 210, 2, 10, { at: 2, height: 40 })
    expect(m.bandY).toBe(110)
    expect(m.boxes.map((b) => [b.column, b.y])).toEqual([[0, 0], [1, 0], [0, 160], [1, 160]])
    expect(m.height).toBe(190)
  })

  it('分隔带可以在最前或最后', () => {
    const first = layoutMasonry([30], 210, 2, 10, { at: 0, height: 40 })
    expect(first.bandY).toBe(0)
    expect(first.boxes[0].y).toBe(50)
    const last = layoutMasonry([30, 60], 210, 2, 10, { at: 2, height: 40 })
    expect(last.bandY).toBe(70)
    expect(last.height).toBe(110)
  })
})

describe('visibleIndexes', () => {
  it('只取与可视区域相交的卡片', () => {
    const m = layoutMasonry([100, 100, 100, 100, 100, 100], 210, 2, 10)
    expect(visibleIndexes(m.boxes, 0, 50)).toEqual([0, 1])
    expect(visibleIndexes(m.boxes, 105, 215)).toEqual([2, 3])
    expect(visibleIndexes(m.boxes, 400, 500)).toEqual([])
  })
})

describe('moveSelection', () => {
  // 列 0：0（0–100）、3（110–150）、4（160–200）、5（210–410）；列 1：1（0–40）、2（50–250）
  const m = layoutMasonry([100, 40, 200, 40, 40, 200], 210, 2, 10)

  it('布局符合预期', () => {
    expect(m.boxes.map((b) => b.column)).toEqual([0, 1, 1, 0, 0, 0])
  })

  it('上下在同一列内移动，到头不动', () => {
    expect(moveSelection(m.boxes, 0, 'down')).toBe(3)
    expect(moveSelection(m.boxes, 3, 'down')).toBe(4)
    expect(moveSelection(m.boxes, 5, 'down')).toBe(5)
    expect(moveSelection(m.boxes, 5, 'up')).toBe(4)
    expect(moveSelection(m.boxes, 1, 'up')).toBe(1)
    expect(moveSelection(m.boxes, 2, 'up')).toBe(1)
  })

  it('左右跳到相邻列中垂直中心最接近的卡片', () => {
    // 3 的中心 130，列 1 中 2 的中心 150 比 1 的 20 近
    expect(moveSelection(m.boxes, 3, 'right')).toBe(2)
    expect(moveSelection(m.boxes, 5, 'right')).toBe(2)
    // 1 的中心 20，列 0 中 0 的中心 50 最近
    expect(moveSelection(m.boxes, 1, 'left')).toBe(0)
    // 2 的中心 150，列 0 中 3 的中心 130 比 4 的 180 近
    expect(moveSelection(m.boxes, 2, 'left')).toBe(3)
    expect(moveSelection(m.boxes, 0, 'left')).toBe(0)
    expect(moveSelection(m.boxes, 2, 'right')).toBe(2)
  })

  it('同样近时取上面的，相邻列为空时跳过', () => {
    const boxes = [
      { x: 0, y: 40, width: 10, height: 20, column: 0 },
      { x: 40, y: 0, width: 10, height: 40, column: 2 },
      { x: 40, y: 60, width: 10, height: 40, column: 2 },
    ]
    expect(moveSelection(boxes, 0, 'right')).toBe(1)
    expect(moveSelection(boxes, 2, 'left')).toBe(0)
  })

  it('下标无效时原样返回', () => {
    expect(moveSelection([], 0, 'down')).toBe(0)
  })
})

describe('详情栏打开后网格变窄', () => {
  it('列数减少，卡片按新列宽重新排版且不超出容器', () => {
    const cards = [{ kind: 'image', width: 400, height: 300 }, { kind: 'document', width: null, height: null }, { kind: 'image', width: 300, height: 600 }, { kind: 'image', width: null, height: null }]
    const layout = (width: number) => {
      const columns = columnCount(width)
      const columnWidth = (width - (columns - 1) * 12) / columns
      return layoutMasonry(cards.map((c) => cardHeight(c, columnWidth, c.kind)), width, columns, 12)
    }
    const full = layout(1200)
    // 1200 宽减去 400 宽的详情栏
    const narrow = layout(800)
    expect(full.columns).toBe(5)
    expect(narrow.columns).toBe(3)
    for (const b of narrow.boxes) expect(b.x + b.width).toBeLessThanOrEqual(800 + 1e-6)
    expect(narrow.boxes[0].width).toBeGreaterThan(full.boxes[0].width)
    expect(narrow.boxes[3].column).toBe(1)
  })
})
