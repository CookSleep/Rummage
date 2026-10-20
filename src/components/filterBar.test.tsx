// @vitest-environment jsdom

import { act } from 'react'
import { describe, expect, it, vi } from 'vitest'
import type { RootInfo } from '../types'

import { button, click, emitEvent, flush, mockCore, render } from '../test/tauri'
import { DEFAULT_FILTERS } from '../lib/filter'
import FilterBar from './FilterBar'

const root = (id: number, path: string): RootInfo => ({ id, path, offline: false, missing: false, files: 10, excludes: [], scanning: false })

/** 展开“文件夹”菜单，返回其中的选项文字 */
async function folderOptions() {
  await click(button('文件夹'))
  const list = [...document.querySelectorAll('[role="option"]')].map((el) => el.textContent)
  await click(button('文件夹'))
  return list
}

describe('FilterBar', () => {
  it('连上时读取文件夹列表，菜单中只显示文件夹名', async () => {
    mockCore({ list_roots: () => [root(1, 'D:\\照片'), root(2, 'E:\\资料\\合同\\')] })
    render(<FilterBar filters={DEFAULT_FILTERS} onChange={vi.fn()} refresh={false} connected />)
    await flush()
    expect(await folderOptions()).toEqual(['照片', '合同'])
  })

  it('断开期间不读取，重新连上后重新读取文件夹列表', async () => {
    let roots = [root(1, 'D:\\照片')]
    const calls = mockCore({ list_roots: () => roots })
    const onChange = vi.fn()
    const { rerender } = render(<FilterBar filters={DEFAULT_FILTERS} onChange={onChange} refresh={false} connected />)
    await flush()

    rerender(<FilterBar filters={DEFAULT_FILTERS} onChange={onChange} refresh={false} connected={false} />)
    roots = [root(1, 'D:\\照片'), root(2, 'D:\\文档')]
    await flush()
    expect(calls.filter((c) => c.cmd === 'list_roots')).toHaveLength(1)
    expect(await folderOptions()).toEqual(['照片'])

    rerender(<FilterBar filters={DEFAULT_FILTERS} onChange={onChange} refresh={false} connected />)
    await flush()
    expect(await folderOptions()).toEqual(['照片', '文档'])
  })

  it('读取失败后重新连上时恢复', async () => {
    let fail = true
    mockCore({
      list_roots: () => {
        if (fail) throw { code: 'disconnected', message: '后台服务未连接。' }
        return [root(1, 'D:\\照片')]
      },
    })
    const { rerender } = render(<FilterBar filters={DEFAULT_FILTERS} onChange={vi.fn()} refresh={false} connected />)
    await flush()
    expect(await folderOptions()).toEqual([])

    fail = false
    rerender(<FilterBar filters={DEFAULT_FILTERS} onChange={vi.fn()} refresh={false} connected={false} />)
    rerender(<FilterBar filters={DEFAULT_FILTERS} onChange={vi.fn()} refresh={false} connected />)
    await flush()
    expect(await folderOptions()).toEqual(['照片'])
  })

  it('命令行或 MCP 增删文件夹（RootsChanged）时重新读取', async () => {
    let roots = [root(1, 'D:\\照片')]
    mockCore({ list_roots: () => roots })
    render(<FilterBar filters={DEFAULT_FILTERS} onChange={vi.fn()} refresh={false} connected />)
    await flush()

    roots = [root(2, 'D:\\文档')]
    await emitEvent('core-event', { type: 'RootsChanged' })
    expect(await folderOptions()).toEqual(['文档'])
  })

  it('还没有添加文件夹时文件夹筛选不可展开，悬停说明原因；添加后恢复', async () => {
    let roots: RootInfo[] = []
    mockCore({ list_roots: () => roots })
    render(<FilterBar filters={DEFAULT_FILTERS} onChange={vi.fn()} refresh={false} connected />)
    await flush()
    const trigger = button('文件夹')
    expect(trigger.getAttribute('aria-disabled')).toBe('true')
    await click(trigger)
    expect(trigger.getAttribute('aria-expanded')).toBe('false')
    // jsdom 没有 elementFromPoint，气泡定位时用到
    document.elementFromPoint = () => trigger
    act(() => {
      trigger.dispatchEvent(new MouseEvent('mouseenter'))
    })
    expect(document.body.textContent).toContain('还没有添加文件夹')

    roots = [root(1, 'D:\\照片')]
    await emitEvent('core-event', { type: 'RootsChanged' })
    expect(button('文件夹').getAttribute('aria-disabled')).toBeNull()
    expect(await folderOptions()).toEqual(['照片'])
  })
})
