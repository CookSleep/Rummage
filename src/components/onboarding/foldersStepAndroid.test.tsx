// @vitest-environment jsdom

import { act } from 'react'
import { describe, expect, it, vi } from 'vitest'

// `isAndroid` 在 src/lib/platform.ts 加载时求值，要在导入被测模块之前改掉 User-Agent
vi.hoisted(() => {
  Object.defineProperty(navigator, 'userAgent', { value: 'Mozilla/5.0 (Linux; Android 17) AppleWebKit/537.36', configurable: true })
})

import { button, click, flush, mockCore, render, text } from '../../test/tauri'
import type { RootInfo } from '../../types'
import FoldersStep from './FoldersStep'

const CARD_TITLE = '需要“所有文件访问”权限'

function root(id: number, path: string): RootInfo {
  return { id, path, offline: false, missing: false, files: 0, excludes: [], skipNomedia: true, scanning: false }
}

function addButton() {
  const el = [...document.querySelectorAll('button')].find((b) => b.textContent?.startsWith('添加文件夹'))
  if (!el) throw new Error('找不到“添加文件夹”')
  return el
}

describe('FoldersStep（Android）', () => {
  it('未授权时显示授权卡片并禁用添加，授权后显示快速添加', async () => {
    const calls = mockCore({ storage_access: () => false, request_storage_access: () => true, list_roots: () => [] })
    render(<FoldersStep />)
    await flush()
    expect(text()).toContain(CARD_TITLE)
    expect(addButton().disabled).toBe(true)
    expect(text()).not.toContain('快速添加')

    await click(button('前往授权'))
    expect(calls.some((c) => c.cmd === 'request_storage_access')).toBe(true)
    expect(text()).not.toContain(CARD_TITLE)
    expect(addButton().disabled).toBe(false)
    expect(text()).toContain('快速添加')
  })

  it('读取权限失败时按未授权显示卡片', async () => {
    mockCore({
      storage_access: () => {
        throw { code: 'internal', message: '无法读取存储权限，请重试。' }
      },
      list_roots: () => [],
    })
    render(<FoldersStep />)
    await flush()
    expect(text()).toContain(CARD_TITLE)
    expect(addButton().disabled).toBe(true)
  })

  it('回到前台时重新读取权限', async () => {
    let granted = false
    mockCore({ storage_access: () => granted, list_roots: () => [] })
    render(<FoldersStep />)
    await flush()
    expect(text()).toContain(CARD_TITLE)

    granted = true
    await act(async () => {
      document.dispatchEvent(new Event('visibilitychange'))
    })
    await flush()
    expect(text()).not.toContain(CARD_TITLE)
  })

  it('已授权时已添加的常用文件夹禁用，点其他的调用 add_common_root', async () => {
    let roots = [root(1, '/storage/emulated/0/DCIM'), root(2, '/storage/emulated/0/Download/书')]
    const calls = mockCore({
      storage_access: () => true,
      list_roots: () => roots,
      add_common_root: () => {
        roots = [...roots, root(3, '/storage/emulated/0/Pictures')]
        return null
      },
    })
    render(<FoldersStep />)
    await flush()
    expect(text()).not.toContain(CARD_TITLE)
    expect((button('DCIM') as HTMLButtonElement).disabled).toBe(true)
    // 子文件夹不算添加了 Download
    expect((button('Download') as HTMLButtonElement).disabled).toBe(false)

    await click(button('Pictures'))
    expect(calls.filter((c) => c.cmd === 'add_common_root').map((c) => c.args)).toEqual([{ name: 'Pictures' }])
    expect((button('Pictures') as HTMLButtonElement).disabled).toBe(true)
  })
})
