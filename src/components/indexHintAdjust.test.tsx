// @vitest-environment jsdom

import { describe, expect, it, vi } from 'vitest'
import type { IndexStatus } from '../types'
import { click, findButton, render } from '../test/tauri'
import IndexHint from './IndexHint'

const base: IndexStatus = {
  state: 'running', paused: null, slow: false, total: 70, done: 60, failed: 0, pending: 10, byKind: {}, backend: 'CPU', error: null, etaS: null, rebuild: null, round: { done: 60, total: 70 },
}

function show(status: IndexStatus) {
  const onAdjust = vi.fn()
  const { container } = render(<IndexHint status={status} onFailed={vi.fn()} onAdjust={onAdjust} />)
  return { onAdjust, adjust: findButton('调整', container) }
}

describe('IndexHint 的“调整”', () => {
  it('自动暂停时点击打开对应的参数', async () => {
    const cases = [
      ['disk_low', 'min_free_disk_gb'],
      ['hot', 'pause_when_hot'],
      ['battery', 'pause_on_battery'],
      ['screen_on', 'pause_when_screen_on'],
      ['fullscreen', 'pause_on_fullscreen'],
    ] as const
    for (const [type, key] of cases) {
      const paused = type === 'disk_low' ? { type, freeGb: 1 } : { type }
      const v = show({ ...base, paused })
      await click(v.adjust!)
      expect(v.onAdjust).toHaveBeenCalledWith(key)
    }
  })

  it('发热放慢时打开“发热时放慢”', async () => {
    const v = show({ ...base, slow: true })
    await click(v.adjust!)
    expect(v.onAdjust).toHaveBeenCalledWith('slow_when_hot')
  })

  it('手动暂停、睡眠、额度用完、没有暂停时不显示', () => {
    for (const paused of [{ type: 'manual', until: null }, { type: 'sleeping' }, { type: 'quota' }, null] as const) {
      expect(show({ ...base, paused }).adjust).toBeNull()
    }
  })

  it('缺模型等警告时、没有提示文字时不显示', () => {
    expect(show({ ...base, state: 'no_model', paused: { type: 'screen_on' } }).adjust).toBeNull()
    expect(show({ ...base, pending: 0, round: null, slow: true }).adjust).toBeNull()
  })
})
