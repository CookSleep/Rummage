import { describe, expect, it } from 'vitest'
import type { ParamDef } from '../types'
import { describeParam, parseParamInput, rebuildLabel } from './params'

function def(kind: ParamDef['kind'], extra: Partial<ParamDef> = {}): ParamDef {
  return { key: 'k', group: 'Index', section: '', kind, default: 1, affectsIndex: false, indexKinds: [], label: '', help: '', basis: '', ...extra }
}

describe('describeParam', () => {
  it('数字在说明后的括号中写范围，开关与选项只有说明', () => {
    expect(describeParam(def('Bool', { default: true, help: '记下最近的搜索。' }))).toBe('记下最近的搜索。')
    expect(describeParam(def({ Int: { min: 10, max: 500 } }, { default: 100, help: '一次搜索最多显示多少个结果。' }))).toBe('一次搜索最多显示多少个结果（10–500）。')
    expect(describeParam(def({ Float: { min: 0.9, max: 1 } }, { help: '越高越严格。' }))).toBe('越高越严格（0.9–1）。')
    const named = def({ Named: { options: [[1, '找物体'], [0, '自定义']], unit: '' } }, { help: '找物体最快。' })
    expect(describeParam(named)).toBe('找物体最快。')
  })
})

describe('parseParamInput', () => {
  const int = { Int: { min: 1, max: 64 } }
  const float = { Float: { min: 0, max: 2 } }

  it('接受范围内的值', () => {
    expect(parseParamInput(int, ' 8 ')).toEqual({ value: 8 })
    expect(parseParamInput(float, '1.5')).toEqual({ value: 1.5 })
    expect(parseParamInput(float, '2')).toEqual({ value: 2 })
  })

  it('越界或不是数字时给出原因', () => {
    expect(parseParamInput(int, '0')).toEqual({ error: '范围 1–64' })
    expect(parseParamInput(int, '65')).toEqual({ error: '范围 1–64' })
    expect(parseParamInput(int, '1.5')).toEqual({ error: '请输入整数' })
    expect(parseParamInput(int, '')).toEqual({ error: '请输入整数' })
    expect(parseParamInput(float, 'abc')).toEqual({ error: '请输入数字' })
    expect(parseParamInput(float, '2.01')).toEqual({ error: '范围 0–2' })
  })
})

describe('rebuildLabel', () => {
  it('合并受影响的种类', () => {
    const img = def({ Named: { options: [[70, '低']], unit: 'Token' } }, { affectsIndex: true, indexKinds: ['image'] })
    const text = def({ Int: { min: 1, max: 2 } }, { affectsIndex: true, indexKinds: ['document'] })
    expect(rebuildLabel([[img, 70], [text, 1]])).toBe('图片、文档')
    const audio = def({ Int: { min: 1, max: 30 } }, { affectsIndex: true, indexKinds: ['audio'] })
    expect(rebuildLabel([[audio, 5]])).toBe('音频')
    const video = def({ Int: { min: 1, max: 10 } }, { affectsIndex: true, indexKinds: ['video'] })
    expect(rebuildLabel([[video, 4]])).toBe('视频')
    expect(rebuildLabel([[def('Bool'), true]])).toBe('')
  })
})
