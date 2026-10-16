// @vitest-environment jsdom

import { beforeEach, describe, expect, it, vi } from 'vitest'

const tauri = vi.hoisted(() => ({ handler: null as (() => void) | null, invoke: vi.fn(() => Promise.resolve()) }))

vi.mock('@tauri-apps/api/app', () => ({
  onBackButtonPress: async (fn: () => void) => {
    tauri.handler = fn
    return { unregister: async () => {} }
  },
}))
vi.mock('@tauri-apps/api/core', () => ({ invoke: tauri.invoke }))

import { handleBack, installBackButton, pushBackLayer } from './back'

beforeEach(() => {
  tauri.invoke.mockClear()
})

describe('返回键', () => {
  it('后登记的层先关闭，撤销登记的层不再关闭', () => {
    const closed: string[] = []
    const offA = pushBackLayer(() => closed.push('a'))
    const offB = pushBackLayer(() => closed.push('b'))
    const offC = pushBackLayer(() => closed.push('c'))
    offC()
    expect(handleBack()).toBe(true)
    expect(closed).toEqual(['b'])
    offB()
    expect(handleBack()).toBe(true)
    expect(closed).toEqual(['b', 'a'])
    offA()
    expect(handleBack()).toBe(false)
  })

  it('没有可关闭的层时把应用退到后台', async () => {
    await installBackButton()
    const off = pushBackLayer(() => {})
    tauri.handler!()
    expect(tauri.invoke).not.toHaveBeenCalled()
    off()
    tauri.handler!()
    expect(tauri.invoke).toHaveBeenCalledWith('move_task_to_back')
  })
})
