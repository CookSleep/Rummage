// @vitest-environment jsdom

import { act } from 'react'
import { describe, expect, it } from 'vitest'
import type { IndexStatus } from '../types'

import { emitEvent, flush, mockCore, render, text } from '../test/tauri'
import { useIndexStatus } from './useIndexStatus'

const status = (done: number): IndexStatus => ({
  state: 'running', paused: null, slow: false, total: 10, done, failed: 0, pending: 10 - done, byKind: {}, backend: 'CPU', error: null, etaS: null, rebuild: null, round: { done, total: 10 },
})

function Done() {
  const s = useIndexStatus(true)
  return <span>{s ? `已处理 ${s.done}` : '无'}</span>
}

describe('useIndexStatus', () => {
  it('index_status 的回复晚于进度事件到达时，不覆盖较新的进度', async () => {
    let reply = (_: IndexStatus) => {}
    mockCore({ index_status: () => new Promise((resolve) => { reply = resolve }) })
    render(<Done />)
    await flush()
    await emitEvent('core-event', { type: 'IndexProgress', ...status(5) })
    expect(text()).toBe('已处理 5')
    await act(async () => reply(status(1)))
    await flush()
    expect(text()).toBe('已处理 5')
  })
})
