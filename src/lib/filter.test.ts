import { describe, expect, it } from 'vitest'
import { customLabel, DEFAULT_FILTERS, isReversed, toSearchFilter } from './filter'

describe('toSearchFilter', () => {
  const now = new Date(2026, 9, 14, 15, 30)

  it('默认全部勾选，此时不限种类', () => {
    const f = toSearchFilter(DEFAULT_FILTERS, now)
    expect(f.kinds).toEqual([])
    expect(f.rootIds).toEqual([])
    expect(f.since).toBeNull()
    const some = toSearchFilter({ ...DEFAULT_FILTERS, kinds: ['image', 'video'] }, now)
    expect(some.kinds).toEqual(['image', 'video'])
  })

  it('文件夹与修改时间', () => {
    expect(toSearchFilter({ ...DEFAULT_FILTERS, rootIds: [3, 5] }, now).rootIds).toEqual([3, 5])
    expect(toSearchFilter({ ...DEFAULT_FILTERS, time: 'today' }, now).since).toBe(new Date(2026, 9, 14).getTime())
    expect(toSearchFilter({ ...DEFAULT_FILTERS, time: 'week' }, now).since).toBe(now.getTime() - 7 * 86_400_000)
    expect(toSearchFilter({ ...DEFAULT_FILTERS, time: 'year' }, now).since).toBe(now.getTime() - 365 * 86_400_000)
    expect(toSearchFilter({ ...DEFAULT_FILTERS, time: 'year' }, now).until).toBeNull()
  })

  it('自定义范围按本地时区含当天，与命令行的 --since、--until 相同', () => {
    const both = toSearchFilter({ ...DEFAULT_FILTERS, time: 'custom', from: '2025-03-01', to: '2025-06-30' }, now)
    expect(both.since).toBe(new Date(2025, 2, 1).getTime())
    expect(both.until).toBe(new Date(2025, 6, 1).getTime() - 1)
    const from = toSearchFilter({ ...DEFAULT_FILTERS, time: 'custom', from: '2025-03-01', to: '' }, now)
    expect(from.until).toBeNull()
    const to = toSearchFilter({ ...DEFAULT_FILTERS, time: 'custom', from: '', to: '2025-12-31' }, now)
    expect(to.since).toBeNull()
    expect(to.until).toBe(new Date(2026, 0, 1).getTime() - 1)
    // 没选自定义时不使用记住的范围
    expect(toSearchFilter({ ...DEFAULT_FILTERS, time: 'any', from: '2025-03-01', to: '2025-06-30' }, now).until).toBeNull()
  })

  it('范围的显示与颠倒检查', () => {
    expect(customLabel('2025-03-01', '2025-06-30')).toBe('2025-03-01 – 2025-06-30')
    expect(customLabel('2025-03-01', '')).toBe('2025-03-01 以后')
    expect(customLabel('', '2025-06-30')).toBe('2025-06-30 以前')
    expect(customLabel('', '')).toBe('')
    expect(isReversed('2025-06-30', '2025-03-01')).toBe(true)
    expect(isReversed('2025-03-01', '2025-03-01')).toBe(false)
    expect(isReversed('2025-06-30', '')).toBe(false)
  })
})
