// @vitest-environment jsdom

import { act } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { SearchHit, SearchResults } from '../types'

import { flush, mockCore, render, setupStatus, text, toast, type Handler } from '../test/tauri'
import SearchPage from './SearchPage'
import Toaster from './Toast'

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
  // 选中结果时滚到可见处、找相似时回到顶部，jsdom 没有实现
  Element.prototype.scrollBy = () => {}
  Element.prototype.scrollTo = () => {}
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

/** 在 `el` 中粘贴：`text` 是剪贴板中的文字，`files` 是剪贴板中的文件（截图或复制的文件） */
function paste(el: Element, text: string, files: File[]) {
  const e = new Event('paste', { bubbles: true, cancelable: true })
  const items = files.map((f) => ({ kind: 'file', type: f.type, getAsFile: () => f }))
  Object.defineProperty(e, 'clipboardData', { value: { getData: (t: string) => (t === 'text/plain' ? text : ''), items } })
  act(() => {
    el.dispatchEvent(e)
  })
  return e
}

/** 渲染空的搜索页，`pasteImage` 处理 `paste_image` 命令 */
async function renderPage(pasteImage: Handler) {
  const calls = mockCore({ search: () => results(), get_history: () => [], list_roots: () => [], get_params: () => ({}), paste_image: pasteImage })
  render(<SearchPage coreStatus="running" setup={setupStatus()} setupError="" onSetup={vi.fn()} onSetupRetry={vi.fn()} appearance={APPEARANCE} onAppearance={vi.fn()} />)
  await flush()
  return calls
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

  it('粘贴图片时以图搜图，Base64 编码的图片内容与文件名交给 Rust 侧', async () => {
    const calls = await renderPage(() => ({ token: 7, name: '截图.png' }))
    const input = document.querySelector<HTMLInputElement>('input[role="combobox"]')!
    const file = new File([new Uint8Array([1, 2, 3])], '截图.png', { type: 'image/png' })
    expect(paste(input, '', [file]).defaultPrevented).toBe(true)
    await flush()
    expect(calls.find((c) => c.cmd === 'paste_image')!.args).toEqual({ data: 'AQID', name: '截图.png', mime: 'image/png' })
    expect(text()).toContain('以图搜图：截图.png')
    // 已在以图搜图时，描述框中粘贴新图片换掉原来的
    const note = document.querySelector<HTMLInputElement>('input[aria-label="加一句描述"]')!
    expect(paste(note, '', [file]).defaultPrevented).toBe(true)
    // 焦点不在输入框中（如在结果中、筛选条的复选框上）时同样可以粘贴；剪贴板中只有空白的文字不算文字
    expect(paste(document.body, '', [file]).defaultPrevented).toBe(true)
    const box = document.createElement('input')
    box.type = 'checkbox'
    document.body.appendChild(box)
    expect(paste(box, ' \n', [file]).defaultPrevented).toBe(true)
    box.remove()
    await flush()
    expect(calls.filter((c) => c.cmd === 'paste_image')).toHaveLength(4)
  })

  it('剪贴板中有文字、没有图片，或焦点在其他输入框中时照常粘贴', async () => {
    const calls = await renderPage(() => ({ token: 7, name: 'a.png' }))
    const input = document.querySelector<HTMLInputElement>('input[role="combobox"]')!
    const image = new File([], 'image.png', { type: 'image/png' })
    // 从表格中复制的文字附带一张渲染图
    expect(paste(input, '季度报表', [image]).defaultPrevented).toBe(false)
    expect(paste(input, '', []).defaultPrevented).toBe(false)
    // 如“自定义日期”的输入框
    const other = document.createElement('input')
    document.body.appendChild(other)
    expect(paste(other, '', [image]).defaultPrevented).toBe(false)
    other.remove()
    await flush()
    expect(calls.some((c) => c.cmd === 'paste_image')).toBe(false)
  })

  it('粘贴的文件不是可用的图片时不读取内容，类型为空的按扩展名判断', async () => {
    const calls = await renderPage(() => ({ token: 7, name: 'a.heic' }))
    const input = document.querySelector<HTMLInputElement>('input[role="combobox"]')!
    for (const f of [new File([], 'a.mp4', { type: 'video/mp4' }), new File([], 'disk.img'), new File([], 'a.psd', { type: 'image/vnd.adobe.photoshop' })]) {
      expect(paste(input, '', [f]).defaultPrevented).toBe(true)
    }
    await flush()
    expect(calls.some((c) => c.cmd === 'paste_image')).toBe(false)
    expect(paste(input, '', [new File([], 'a.HEIC')]).defaultPrevented).toBe(true)
    await flush()
    expect(calls.some((c) => c.cmd === 'paste_image')).toBe(true)
  })

  it('读不出粘贴的文件时提示中文', async () => {
    await renderPage(() => ({ token: 7, name: 'a.png' }))
    render(<Toaster />)
    const read = vi.spyOn(FileReader.prototype, 'readAsDataURL').mockImplementation(function (this: FileReader) {
      Object.defineProperty(this, 'error', { value: new DOMException('The requested file could not be read', 'NotReadableError') })
      this.onerror?.(new ProgressEvent('error') as ProgressEvent<FileReader>)
    })
    paste(document.body, '', [new File([], 'a.png', { type: 'image/png' })])
    await flush()
    expect(toast()).toContain('无法读取粘贴的图片')
    expect(toast()).not.toContain('requested')
    read.mockRestore()
  })
})
