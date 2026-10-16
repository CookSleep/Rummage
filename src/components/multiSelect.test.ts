import { describe, expect, it } from 'vitest'
import { summary } from './MultiSelect'

describe('summary', () => {
  const kinds = ['文档', '图片', '音频', '视频', '其他']

  it('全部、一两项、只少一项与其余', () => {
    expect(summary(kinds, kinds, '全部类型', '种类型')).toBe('全部类型')
    expect(summary(kinds, ['图片'], '全部类型', '种类型')).toBe('图片')
    expect(summary(kinds, ['图片', '视频'], '全部类型', '种类型')).toBe('图片、视频')
    expect(summary(kinds, ['文档', '图片', '音频', '视频'], '全部类型', '种类型')).toBe('不含其他')
    expect(summary(kinds, ['文档', '图片', '音频'], '全部类型', '种类型')).toBe('3 种类型')
  })

  it('选项少时列出名称', () => {
    expect(summary(['甲', '乙', '丙'], ['甲', '丙'], '全部文件夹', '个文件夹')).toBe('甲、丙')
    expect(summary([], [], '全部文件夹', '个文件夹')).toBe('全部文件夹')
  })
})
