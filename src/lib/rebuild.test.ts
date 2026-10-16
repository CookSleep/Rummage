import { describe, expect, it } from 'vitest'
import { rebuildCounts, rebuildNotice } from './rebuild'

describe('rebuild', () => {
  it('按种类列出要重建的文件，数字带千位分隔', () => {
    expect(rebuildCounts({ image: 1234 })).toBe('1,234 张图片')
    expect(rebuildCounts({ video: 2, image: 3, audio: 0, document: 1 })).toBe('3 张图片、1 个文档、2 个视频')
  })

  it('设置页只说还要重建多少', () => {
    expect(rebuildNotice({ done: 10, total: 1234, byKind: { image: 1224 } })).toBe('按新设置还需重建 1,224 张图片，期间旧索引仍可搜索。')
  })
})
