// @vitest-environment jsdom

import { act } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { SearchHit } from '../types'

// `isAndroid` 在 src/lib/platform.ts 加载时求值，要在导入被测模块之前改掉 User-Agent
vi.hoisted(() => {
  Object.defineProperty(navigator, 'userAgent', { value: 'Mozilla/5.0 (Linux; Android 17) AppleWebKit/537.36', configurable: true })
})

import { button, click, flush, mockCore, render, setupStatus } from '../test/tauri'
import { handleBack } from '../lib/back'
import Onboarding from './onboarding/Onboarding'
import SearchPage from './SearchPage'

const APPEARANCE = { uiFont: 'builtin', uiFontName: '', monoFont: 'builtin', monoFontName: '' } as const

const HIT: SearchHit = {
  fileId: 1,
  path: '/storage/emulated/0/Documents/报告.txt',
  name: '报告.txt',
  kind: 'document',
  size: 10,
  mtime: 1_700_000_000_000,
  cloud: false,
  offline: false,
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
}

beforeEach(() => {
  vi.stubGlobal('ResizeObserver', class {
    observe() {}
    unobserve() {}
    disconnect() {}
  })
  vi.spyOn(window, 'scrollBy').mockImplementation(() => {})
  // 手机宽度：详情栏为覆盖整个窗口的详情页
  vi.spyOn(window, 'innerWidth', 'get').mockReturnValue(375)
})

function press(el: Element, key: string) {
  act(() => {
    el.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }))
  })
}

/** 按一次返回键 */
function back() {
  let handled = false
  act(() => {
    handled = handleBack()
  })
  return handled
}

const input = () => document.querySelector<HTMLInputElement>('input[role="combobox"]')
const detail = () => document.querySelector('aside[aria-label="详情"]')
const dialog = (label?: string) => document.querySelector(label ? `[role="dialog"][aria-label="${label}"]` : '[role="dialog"]:not([aria-label])')

async function searchPage() {
  mockCore({
    search: () => ({ hits: [HIT], tookMs: 1, partial: false, semanticUnavailable: false, unavailable: null }),
    get_history: () => [],
    list_roots: () => [],
    get_params: () => ({}),
    param_defs: () => [],
    get_notices: () => [],
    storage_info: () => ({ dataDir: '/data/user/0/com.cooksleep.rummage/files', db: 1, thumbs: 1, models: 1, logs: 1, other: 1 }),
  })
  render(<SearchPage coreStatus="running" setup={setupStatus()} setupError="" onSetup={vi.fn()} onSetupRetry={vi.fn()} appearance={APPEARANCE} onAppearance={vi.fn()} />)
  await flush()
  const el = input()!
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(el, '报告')
    el.dispatchEvent(new Event('input', { bubbles: true }))
  })
  await act(async () => {
    await new Promise((r) => setTimeout(r, 300))
  })
  await flush()
}

describe('Android 返回键', () => {
  it('依次关闭详情页、清空搜索，最后交给系统（退到后台）', async () => {
    await searchPage()
    // jsdom 中网格宽度为 0，不渲染卡片：用键盘选中第一个结果，空格打开详情页
    press(input()!, 'ArrowDown')
    press(document.activeElement!, ' ')
    expect(detail()).not.toBeNull()

    expect(back()).toBe(true)
    expect(detail()).toBeNull()
    expect(input()!.value).toBe('报告')

    expect(back()).toBe(true)
    expect(input()!.value).toBe('')

    expect(back()).toBe(false)
  })

  it('先关闭设置里的确认框，再关闭设置，最后清空搜索', async () => {
    await searchPage()
    await click(button('设置'))
    await click([...document.querySelectorAll<HTMLElement>('nav button')].find((b) => b.textContent === '数据')!)
    await click(button('清空'))
    expect(dialog()).not.toBeNull()

    expect(back()).toBe(true)
    expect(dialog()).toBeNull()
    expect(dialog('设置')).not.toBeNull()

    expect(back()).toBe(true)
    expect(dialog('设置')).toBeNull()
    expect(input()!.value).toBe('报告')

    expect(back()).toBe(true)
    expect(input()!.value).toBe('')
  })

  it('引导中回到上一步，第一步时交给系统', async () => {
    const calls = mockCore({
      get_params: () => ({ index_documents: true, index_images: false, index_audio: false, index_video: false }),
      list_roots: () => [{ id: 1, path: '/storage/emulated/0/DCIM', offline: false, files: 5, excludes: [], scanning: false }],
    })
    render(<Onboarding status={setupStatus({ onboarded: false, step: 1 })} coreStatus="running" onStatus={vi.fn()} onDone={vi.fn()} />)
    await flush()
    expect(document.body.textContent).toContain('希望按内容查找哪些文件')

    expect(back()).toBe(true)
    await flush()
    expect(calls.filter((c) => c.cmd === 'set_onboarding_step').map((c) => c.args)).toEqual([{ step: 0 }])
    expect(document.body.textContent).toContain('选择要搜索的文件夹')

    expect(back()).toBe(false)
  })
})
