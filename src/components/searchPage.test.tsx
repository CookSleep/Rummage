// @vitest-environment jsdom

import { act } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { SearchHit, SearchResults } from '../types'

import { flush, mockCore, render, setupStatus, text } from '../test/tauri'
import SearchPage from './SearchPage'

const APPEARANCE = { uiFont: 'builtin', uiFontName: '', monoFont: 'builtin', monoFontName: '' } as const

const hit = (fileId: number): SearchHit => ({
  fileId,
  path: `D:\\资料\\${fileId}.txt`,
  name: `${fileId}.txt`,
  kind: 'document',
  size: 10,
  mtime: 1_700_000_000_000,
  cloud: false,
  offline: false,
  missing: false,
  score: 1,
  cosine: null,
  z: null,
  matchPct: null,
  lowRelevance: false,
  segments: [],
  nameHit: true,
  textSegments: [],
  width: null,
  height: null,
  snippet: null,
})

const results = (over: Partial<SearchResults> = {}): SearchResults => ({ hits: [], tookMs: 1, partial: false, semanticUnavailable: false, unavailable: null, ...over })

beforeEach(() => {
  // jsdom 没有 ResizeObserver，结果网格只用它重新测量
  vi.stubGlobal('ResizeObserver', class {
    observe() {}
    unobserve() {}
    disconnect() {}
  })
  // 选中结果时滚到可见处，jsdom 没有实现
  vi.spyOn(window, 'scrollBy').mockImplementation(() => {})
})

/** 渲染搜索页并在搜索框中输入 `value`，等防抖后的搜索返回 */
async function search(value: string, reply: SearchResults) {
  mockCore({ search: () => reply, get_history: () => [], list_roots: () => [], get_params: () => ({}) })
  render(<SearchPage coreStatus="running" setup={setupStatus()} setupError="" onSetup={vi.fn()} onSetupRetry={vi.fn()} appearance={APPEARANCE} onAppearance={vi.fn()} />)
  await flush()
  const input = document.querySelector<HTMLInputElement>('input[role="combobox"]')!
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, value)
    input.dispatchEvent(new Event('input', { bubbles: true }))
  })
  await act(async () => {
    await new Promise((r) => setTimeout(r, 300))
  })
  await flush()
  return input
}

function press(el: Element, key: string) {
  const e = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true })
  act(() => {
    el.dispatchEvent(e)
  })
  return e
}

describe('SearchPage', () => {
  it('焦点在搜索框以外的输入框中时，方向键与 Esc 交给输入框', async () => {
    const input = await search('报告', results({ hits: [hit(1), hit(2)] }))
    // 搜索框中按向下键移到结果
    input.focus()
    expect(press(input, 'ArrowDown').defaultPrevented).toBe(true)
    // 如“自定义日期”的输入框
    const other = document.createElement('input')
    other.type = 'text'
    document.body.appendChild(other)
    other.focus()
    expect(press(other, 'ArrowDown').defaultPrevented).toBe(false)
    expect(press(other, 'ArrowUp').defaultPrevented).toBe(false)
    expect(press(other, 'Escape').defaultPrevented).toBe(false)
    expect(input.value).toBe('报告')
    other.remove()
  })

  it('描述为空时在其中按退格清除找相似的标记，回到原来的文字搜索', async () => {
    const input = await search('报告', results({ hits: [{ ...hit(1), kind: 'image', name: '1.jpg' }, hit(2)] }))
    // 选中第一个结果，用详情栏中的按钮找相似
    input.focus()
    press(input, 'ArrowDown')
    act(() => {
      document.querySelector<HTMLButtonElement>('button[aria-label="找相似"]')!.click()
    })
    await flush()
    expect(document.querySelector('input[role="combobox"]')).toBeNull()
    expect(text()).toContain('相似：1.jpg')
    // 焦点不在描述框中时不清除
    const note = document.querySelector<HTMLInputElement>('input[aria-label="加一句描述"]')!
    note.blur()
    expect(press(document.body, 'Backspace').defaultPrevented).toBe(false)
    note.focus()
    expect(press(note, 'Backspace').defaultPrevented).toBe(true)
    await flush()
    expect(document.querySelector<HTMLInputElement>('input[role="combobox"]')!.value).toBe('报告')
  })

  it('不能加描述的找相似标记，焦点不在输入框中时按退格清除', async () => {
    const input = await search('报告', results({ hits: [hit(1), hit(2)] }))
    input.focus()
    press(input, 'ArrowDown')
    act(() => {
      document.querySelector<HTMLButtonElement>('button[aria-label="找相似"]')!.click()
    })
    await flush()
    expect(text()).toContain('相似：1.txt')
    expect(press(document.body, 'Backspace').defaultPrevented).toBe(true)
    await flush()
    expect(document.querySelector<HTMLInputElement>('input[role="combobox"]')!.value).toBe('报告')
  })

  it('没有结果时，一句话的不可用原因去掉句末句号', async () => {
    await search('报告', results({ semanticUnavailable: true, unavailable: 'engine' }))
    expect(text()).toContain('内容识别无法使用，只显示文件名或正文中含有搜索词的结果')
    expect(text()).not.toContain('搜索词的结果。')
  })
})
