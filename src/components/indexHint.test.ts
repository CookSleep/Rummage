import { describe, expect, it } from 'vitest'
import type { IndexStatus } from '../types'
import { indexHint } from './IndexHint'

describe('indexHint', () => {
  const base: IndexStatus = {
    state: 'running', paused: null, slow: false, total: 70, done: 60, failed: 6, pending: 4, byKind: {}, backend: 'CPU', error: null, etaS: null, rebuild: null, round: { done: 66, total: 70 },
  }

  it('有剩余时间时附在进度后面，没有时不显示', () => {
    expect(indexHint(base)).toBe('正在建立索引：已处理 66 / 70，完成前，尚未处理的文件只能按文件名搜索')
    expect(indexHint({ ...base, etaS: 300 })).toBe('正在建立索引：已处理 66 / 70，预计还需约 5 分钟，完成前，尚未处理的文件只能按文件名搜索')
  })

  it('缺少模型时提示下载；只有 NPU 模型、NPU 却用不了时用核心给的说明', () => {
    expect(indexHint({ ...base, state: 'no_model' })).toBe('尚未下载文字模型，无法识别文件内容。可在「设置 → 模型」中下载或导入，之后会继续建立索引。')
    const npu = 'NPU 出错，无法识别文件内容。可在「设置 → 模型」中下载通用模型，改用 GPU 或 CPU。'
    expect(indexHint({ ...base, state: 'no_model', error: npu })).toBe(npu)
  })

  it('显示核心给出的本轮进度，不数之前已建好的', () => {
    expect(indexHint({ ...base, round: { done: 6, total: 10 } })).toBe('正在建立索引：已处理 6 / 10，完成前，尚未处理的文件只能按文件名搜索')
  })

  it('手动暂停时说明何时恢复，不说正在建立索引', () => {
    const now = new Date(2026, 9, 13, 15, 0)
    const manual = (until: number | null) => ({ ...base, paused: { type: 'manual' as const, until } })
    expect(indexHint(manual(null), now)).toBe('索引已暂停：已处理 66 / 70，完成前，尚未处理的文件只能按文件名搜索')
    expect(indexHint(manual(new Date(2026, 9, 13, 16, 5).getTime()), now)).toBe('索引已暂停：已处理 66 / 70，到 16:05 恢复，完成前，尚未处理的文件只能按文件名搜索')
    expect(indexHint(manual(new Date(2026, 9, 14, 0, 0).getTime()), now)).toBe('索引已暂停：已处理 66 / 70，到 10月14日 0:00 恢复，完成前，尚未处理的文件只能按文件名搜索')
    expect(indexHint(manual(now.getTime() - 1000), now)).toBe('索引已暂停：已处理 66 / 70，即将恢复，完成前，尚未处理的文件只能按文件名搜索')
    expect(indexHint({ ...manual(null), pending: 0, round: null }, now)).toBe('索引已暂停')
    expect(indexHint({ ...manual(new Date(2026, 9, 13, 16, 5).getTime()), pending: 0, round: null }, now)).toBe('索引已暂停，到 16:05 恢复')
  })

  it('新文件处理完后显示按新设置重建的进度', () => {
    const rebuild = { ...base, pending: 0, round: null, rebuild: { done: 3, total: 10, byKind: { image: 7 } } }
    expect(indexHint(rebuild)).toBe('正在按新设置重建索引：已完成 3 / 10')
    expect(indexHint({ ...rebuild, etaS: 60 })).toBe('正在按新设置重建索引：已完成 3 / 10，预计还需约 1 分钟')
    // 还有新文件时先说建立索引
    expect(indexHint({ ...rebuild, pending: 4, round: base.round })).toBe('正在建立索引：已处理 66 / 70，完成前，尚未处理的文件只能按文件名搜索')
    expect(indexHint({ ...base, pending: 0, round: null, rebuild: null })).toBeNull()
  })

  it('没选只用 CPU 却用 CPU、或原本要用的设备出了问题时写明用的设备与原因；自动暂停时只说暂停原因', () => {
    expect(indexHint(base, new Date(), { name: 'CPU', why: '显卡检测未通过' })).toBe('正在用「CPU」建立索引（显卡检测未通过）：已处理 66 / 70，完成前，尚未处理的文件只能按文件名搜索')
    const rebuild = { ...base, pending: 0, round: null, rebuild: { done: 3, total: 10, byKind: { image: 7 } } }
    expect(indexHint(rebuild, new Date(), { name: 'CPU', why: '正在检测显卡' })).toBe('正在用「CPU」按新设置重建索引（正在检测显卡）：已完成 3 / 10')
    const paused = { ...base, paused: { type: 'battery' as const } }
    // 原本要用的设备出了问题而改用另一个设备
    expect(indexHint(base, new Date(), { name: 'RTX 3070', why: '「RTX 4080 SUPER」出现问题' })).toBe('正在用「RTX 3070」建立索引（「RTX 4080 SUPER」出现问题）：已处理 66 / 70，完成前，尚未处理的文件只能按文件名搜索')
    expect(indexHint(paused, new Date(), { name: 'CPU', why: '显卡检测未通过' })).toBe('正在建立索引：已处理 66 / 70（未接电源，已暂停），完成前，尚未处理的文件只能按文件名搜索')
  })

  it('发热放慢时写明原因', () => {
    expect(indexHint({ ...base, slow: true })).toBe('正在建立索引：已处理 66 / 70（手机发热，已放慢），完成前，尚未处理的文件只能按文件名搜索')
  })
})
