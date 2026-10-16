import { describe, expect, it } from 'vitest'
import { rowRange } from './rowRange'

describe('rowRange', () => {
  it('只渲染可视区域及上下几行', () => {
    expect(rowRange(0, 300, 60, 100, 2)).toEqual({ start: 0, end: 7 })
    expect(rowRange(600, 300, 60, 100, 2)).toEqual({ start: 8, end: 17 })
  })

  it('不超出列表两端', () => {
    expect(rowRange(5700, 300, 60, 100, 2)).toEqual({ start: 93, end: 100 })
    expect(rowRange(0, 300, 60, 0, 2)).toEqual({ start: 0, end: 0 })
    // 列表变短、滚动位置还停在原处时
    expect(rowRange(9000, 300, 60, 10, 2)).toEqual({ start: 148, end: 148 })
  })
})
