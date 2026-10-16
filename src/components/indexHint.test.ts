import { describe, expect, it } from 'vitest'
import type { IndexStatus } from '../types'
import { indexHint, nextBase } from './IndexHint'

describe('indexHint', () => {
  const base: IndexStatus = {
    state: 'running', paused: null, total: 70, done: 60, failed: 6, pending: 4, byKind: {}, backend: 'CPU', error: null, etaS: null, rebuild: null,
  }

  it('有剩余时间时附在进度后面，没有时不显示', () => {
    expect(indexHint(base)).toBe('正在建立索引：已处理 66 / 70，完成前，尚未处理的文件只能按文件名搜索')
    expect(indexHint({ ...base, etaS: 300 })).toBe('正在建立索引：已处理 66 / 70，预计还需约 5 分钟，完成前，尚未处理的文件只能按文件名搜索')
  })

  it('只数本轮要建的文件，不数之前已建好的', () => {
    expect(indexHint(base, new Date(), 60)).toBe('正在建立索引：已处理 6 / 10，完成前，尚未处理的文件只能按文件名搜索')
    // 本轮中删掉了文件时不出现负数
    expect(indexHint(base, new Date(), 80)).toBe('正在建立索引：已处理 0 / 0，完成前，尚未处理的文件只能按文件名搜索')
  })

  it('手动暂停时说明何时恢复，不说正在建立索引', () => {
    const now = new Date(2026, 9, 13, 15, 0)
    const manual = (until: number | null) => ({ ...base, paused: { type: 'manual' as const, until } })
    expect(indexHint(manual(null), now)).toBe('索引已暂停：已处理 66 / 70，完成前，尚未处理的文件只能按文件名搜索')
    expect(indexHint(manual(new Date(2026, 9, 13, 16, 5).getTime()), now)).toBe('索引已暂停：已处理 66 / 70，到 16:05 恢复，完成前，尚未处理的文件只能按文件名搜索')
    expect(indexHint(manual(new Date(2026, 9, 14, 0, 0).getTime()), now)).toBe('索引已暂停：已处理 66 / 70，到 10月14日 0:00 恢复，完成前，尚未处理的文件只能按文件名搜索')
    expect(indexHint(manual(now.getTime() - 1000), now)).toBe('索引已暂停：已处理 66 / 70，即将恢复，完成前，尚未处理的文件只能按文件名搜索')
    expect(indexHint({ ...manual(null), pending: 0 }, now)).toBe('索引已暂停')
    expect(indexHint({ ...manual(new Date(2026, 9, 13, 16, 5).getTime()), pending: 0 }, now)).toBe('索引已暂停，到 16:05 恢复')
  })

  it('新文件处理完后显示按新设置重建的进度', () => {
    const rebuild = { ...base, pending: 0, rebuild: { done: 3, total: 10, byKind: { image: 7 } } }
    expect(indexHint(rebuild)).toBe('正在按新设置重建索引：已完成 3 / 10')
    expect(indexHint({ ...rebuild, etaS: 60 })).toBe('正在按新设置重建索引：已完成 3 / 10，预计还需约 1 分钟')
    // 还有新文件时先说建立索引
    expect(indexHint({ ...rebuild, pending: 4 })).toBe('正在建立索引：已处理 66 / 70，完成前，尚未处理的文件只能按文件名搜索')
    expect(indexHint({ ...base, pending: 0, rebuild: null })).toBeNull()
  })

  it('没选只用 CPU 却用 CPU、或原本要用的设备出了问题时写明用的设备与原因；自动暂停时只说暂停原因', () => {
    expect(indexHint(base, new Date(), 0, { name: 'CPU', why: '显卡检测未通过' })).toBe('正在用「CPU」建立索引（显卡检测未通过）：已处理 66 / 70，完成前，尚未处理的文件只能按文件名搜索')
    const rebuild = { ...base, pending: 0, rebuild: { done: 3, total: 10, byKind: { image: 7 } } }
    expect(indexHint(rebuild, new Date(), 0, { name: 'CPU', why: '正在检测显卡' })).toBe('正在用「CPU」按新设置重建索引（正在检测显卡）：已完成 3 / 10')
    const paused = { ...base, paused: { type: 'battery' as const } }
    // 原本要用的设备出了问题而改用另一个设备
    expect(indexHint(base, new Date(), 0, { name: 'RTX 3070', why: '「RTX 4080 SUPER」出现问题' })).toBe('正在用「RTX 3070」建立索引（「RTX 4080 SUPER」出现问题）：已处理 66 / 70，完成前，尚未处理的文件只能按文件名搜索')
    expect(indexHint(paused, new Date(), 0, { name: 'CPU', why: '显卡检测未通过' })).toBe('正在建立索引：已处理 66 / 70（未接电源，已暂停），完成前，尚未处理的文件只能按文件名搜索')
  })
})

describe('nextBase', () => {
  const s: IndexStatus = {
    state: 'running', paused: null, total: 100, done: 50, failed: 10, pending: 40, byKind: {}, backend: 'CPU', error: null, etaS: null, rebuild: null,
  }

  it('本轮开始时从已处理数算起，处理完清掉', () => {
    expect(nextBase(null, null, s)).toBe(60)
    expect(nextBase(60, 10, { ...s, pending: 0 })).toBeNull()
  })

  it('本轮中途基数不变，已处理数从本轮开始时算起', () => {
    // 基数由搜索页逐次记下，提示在页头与窗口底部之间移动不影响
    const base = nextBase(nextBase(null, null, s), 10, { ...s, done: 80, pending: 10 })
    expect(base).toBe(60)
    expect(indexHint({ ...s, done: 80, pending: 10 }, new Date(), base!)).toBe('正在建立索引：已处理 30 / 40，完成前，尚未处理的文件只能按文件名搜索')
  })

  it('重试失败的文件后已处理数不倒退，重试的文件计入本轮', () => {
    // 本轮已处理 5 个，然后重试了 4 个失败的文件
    const retried = { ...s, done: 55, failed: 6, pending: 39 }
    const base = nextBase(60, 10, retried)
    expect(base).toBe(56)
    expect(indexHint(retried, new Date(), base!)).toBe('正在建立索引：已处理 5 / 44，完成前，尚未处理的文件只能按文件名搜索')
  })
})
