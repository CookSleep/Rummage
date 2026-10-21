// 组件测试共用：模拟后台服务（Tauri 命令与事件）、渲染与等待。要在被测模块之前导入：
// `isTauri` 在 src/lib/platform.ts 加载时求值，这里先把它设为真

import { act, type ReactNode } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { emit } from '@tauri-apps/api/event'
import { clearMocks, mockIPC, mockWindows } from '@tauri-apps/api/mocks'
import { afterEach, beforeEach, vi } from 'vitest'
import type { DeviceResult, SelftestStatus, SetupStatus } from '../types'

;(globalThis as { isTauri?: boolean }).isTauri = true
// 让 React 知道当前在测试环境中使用 act
;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

export type Handler = (args: Record<string, unknown>) => unknown

/**
 * 拦下 invoke：按命令名交给 `handlers`（调用时才查，测试中途可以替换），没有列出的命令返回 null；
 * 处理函数抛出的值就是命令返回的错误。事件由 `emitEvent` 发出。返回调用记录
 */
export function mockCore(handlers: Record<string, Handler>) {
  const calls: { cmd: string; args: Record<string, unknown> }[] = []
  mockWindows('main')
  mockIPC((cmd, payload) => {
    const args = (payload ?? {}) as Record<string, unknown>
    calls.push({ cmd, args })
    const h = handlers[cmd]
    return h ? h(args) : null
  }, { shouldMockEvents: true })
  return calls
}

let roots: Root[] = []

beforeEach(() => {
  // 组件在读取失败时记录警告，测试中预期如此
  vi.spyOn(console, 'warn').mockImplementation(() => {})
})

afterEach(async () => {
  act(() => roots.forEach((r) => r.unmount()))
  roots = []
  // 卸载时异步取消事件监听，等它们执行完再清除模拟
  await new Promise((r) => setTimeout(r, 0))
  clearMocks()
  document.body.innerHTML = ''
  localStorage.clear()
  vi.restoreAllMocks()
})

export function render(node: ReactNode) {
  const container = document.createElement('div')
  document.body.appendChild(container)
  const root = createRoot(container)
  roots.push(root)
  act(() => root.render(node))
  return { container, rerender: (next: ReactNode) => act(() => root.render(next)) }
}

/** 等待命令的回复以及随后的渲染与再次读取 */
export async function flush() {
  for (let i = 0; i < 4; i++) {
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0))
    })
  }
}

/** 模拟后台推来的事件（如 `core-status`、`core-event`） */
export async function emitEvent(event: string, payload: unknown) {
  await act(async () => {
    await emit(event, payload)
  })
  await flush()
}

export function text(root: ParentNode = document.body) {
  return root.textContent ?? ''
}

/** 按文字或 aria-label 找按钮，找不到时为空 */
export function findButton(name: string, root: ParentNode = document.body) {
  return [...root.querySelectorAll<HTMLElement>('button, [role="button"]')].find((b) => b.textContent?.trim() === name || b.getAttribute('aria-label') === name) ?? null
}

export function button(name: string, root: ParentNode = document.body) {
  const el = findButton(name, root)
  if (!el) throw new Error(`找不到按钮：${name}`)
  return el
}

export async function click(el: HTMLElement) {
  await act(async () => {
    el.click()
  })
  await flush()
}

/** 标题为 `title` 的分区（设置页各页的 h4 小标题所在的 section） */
export function section(title: string) {
  const h = [...document.querySelectorAll('h4')].find((el) => el.textContent === title)
  const el = h?.closest('section')
  if (!el) throw new Error(`找不到分区：${title}`)
  return el
}

export function dialog() {
  const el = document.querySelector<HTMLElement>('[role="dialog"]:not([aria-label="设置"])') ?? document.querySelector<HTMLElement>('[role="dialog"]')
  if (!el) throw new Error('没有打开的对话框')
  return el
}

export function device(over: Partial<DeviceResult> = {}): DeviceResult {
  return { id: 'cpu', gpu: false, pciId: null, name: 'CPU', status: 'passed', reason: null, textMs: 80, imageMs: 900, audioMs: null, videoMs: null, speed: null, state: null, ...over }
}

export function selftest(over: Partial<SelftestStatus> = {}): SelftestStatus {
  return { running: false, preferred: 'auto', preferredError: null, devices: [], selected: null, error: null, stale: false, auto: false, failed: null, gpusOff: false, blocked: [], ...over }
}

export function setupStatus(over: Partial<SetupStatus> = {}): SetupStatus {
  const file = (name: string) => ({ name, bytes: 2 ** 30, ready: true, partialBytes: 0, verified: true, url: `https://example.com/models/${name}` })
  return {
    onboarded: true,
    step: 0,
    model: file('model.gguf'),
    mmproj: file('mmproj.gguf'),
    npu: null,
    npuChip: null,
    tokenizerUrl: null,
    task: null,
    selftest: selftest(),
    proxy: { mode: 'system', url: '' },
    ...over,
  }
}
