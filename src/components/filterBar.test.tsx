// @vitest-environment jsdom

import { act, useState } from 'react'
import { describe, expect, it, vi } from 'vitest'
import type { FolderFilter, RootInfo } from '../types'

import { button, click, emitEvent, findButton, flush, mockCore, render } from '../test/tauri'
import { DEFAULT_FILTERS, type Filters } from '../lib/filter'
import FilterBar from './FilterBar'

/** 像搜索页一样保存筛选条件，变化时告诉 `spy` */
function Harness({ spy }: { spy: (next: Filters) => void }) {
  const [filters, setFilters] = useState(DEFAULT_FILTERS)
  return (
    <FilterBar
      filters={filters}
      onChange={(next) => {
        spy(next)
        setFilters(next)
      }}
      refresh={false}
      connected
    />
  )
}

const root = (id: number, path: string): RootInfo => ({ id, path, offline: false, missing: false, files: 10, excludes: [], skipNomedia: true, scanning: false })

// jsdom 没有 scrollIntoView，下拉菜单展开时用到
Element.prototype.scrollIntoView = () => {}

const ROOTS = [root(1, 'D:\\照片'), root(2, 'E:\\资料\\合同\\')]

/** 展开“文件夹”下拉框并点选一项 */
async function choose(label: string) {
  await click(button('文件夹'))
  const el = [...document.querySelectorAll<HTMLElement>('[role="option"]')].find((o) => o.querySelector('span')?.textContent === label)
  if (!el) throw new Error(`找不到选项：${label}`)
  await click(el)
}

/** 触发器上显示的文字（只取选中的一项） */
const shown = () => button('文件夹').querySelector('[aria-hidden="false"]')?.textContent

const count = (calls: { cmd: string }[], cmd: string) => calls.filter((c) => c.cmd === cmd).length

describe('FilterBar', () => {
  it('连上时读取文件夹列表与文件夹筛选，自定义时显示生效的文件夹名并把生效范围传出', async () => {
    const filter: FolderFilter = {
      custom: true,
      entries: [
        { rootId: 1, rel: '', checked: true },
        { rootId: 1, rel: '2023', checked: false },
        { rootId: 2, rel: '', checked: true },
        { rootId: 2, rel: '2024\\旅行', checked: true },
      ],
    }
    mockCore({ list_roots: () => ROOTS, get_folder_filter: () => filter })
    const onChange = vi.fn()
    render(<FilterBar filters={DEFAULT_FILTERS} onChange={onChange} refresh={false} connected />)
    await flush()
    expect(shown()).toBe('照片、旅行')
    expect(onChange).toHaveBeenLastCalledWith({ ...DEFAULT_FILTERS, folders: [{ rootId: 1, rel: '' }, { rootId: 2, rel: '2024\\旅行' }] })
  })

  it('勾选三项以上时显示数量；保存为全部文件夹时不限范围', async () => {
    let filter: FolderFilter = {
      custom: true,
      entries: [
        { rootId: 1, rel: '', checked: true },
        { rootId: 2, rel: '', checked: true },
        { rootId: 2, rel: '2024', checked: true },
        { rootId: 2, rel: '2025', checked: true },
      ],
    }
    mockCore({ list_roots: () => ROOTS, get_folder_filter: () => filter })
    const onChange = vi.fn()
    render(<FilterBar filters={DEFAULT_FILTERS} onChange={onChange} refresh={false} connected />)
    await flush()
    expect(shown()).toBe('3 个文件夹')

    filter = { ...filter, custom: false }
    onChange.mockClear()
    await emitEvent('core-event', { type: 'RootsChanged' })
    expect(shown()).toBe('全部文件夹')
    // 传入的筛选条件仍是默认的不限范围，不必再改
    expect(onChange).not.toHaveBeenCalled()
  })

  it('断开期间不读取，重新连上后重新读取', async () => {
    const calls = mockCore({ list_roots: () => ROOTS, get_folder_filter: () => ({ custom: false, entries: [] }) })
    const onChange = vi.fn()
    const { rerender } = render(<FilterBar filters={DEFAULT_FILTERS} onChange={onChange} refresh={false} connected />)
    await flush()

    rerender(<FilterBar filters={DEFAULT_FILTERS} onChange={onChange} refresh={false} connected={false} />)
    await flush()
    expect(count(calls, 'list_roots')).toBe(1)
    expect(count(calls, 'get_folder_filter')).toBe(1)

    rerender(<FilterBar filters={DEFAULT_FILTERS} onChange={onChange} refresh={false} connected />)
    await flush()
    expect(count(calls, 'list_roots')).toBe(2)
    expect(count(calls, 'get_folder_filter')).toBe(2)
  })

  it('读取失败后重新连上时恢复', async () => {
    let fail = true
    mockCore({
      list_roots: () => {
        if (fail) throw { code: 'disconnected', message: '后台服务未连接。' }
        return [root(1, 'D:\\照片')]
      },
      get_folder_filter: () => {
        if (fail) throw { code: 'disconnected', message: '后台服务未连接。' }
        return { custom: true, entries: [{ rootId: 1, rel: '', checked: true }] }
      },
    })
    const { rerender } = render(<FilterBar filters={DEFAULT_FILTERS} onChange={vi.fn()} refresh={false} connected />)
    await flush()
    expect(shown()).toBe('全部文件夹')

    fail = false
    rerender(<FilterBar filters={DEFAULT_FILTERS} onChange={vi.fn()} refresh={false} connected={false} />)
    rerender(<FilterBar filters={DEFAULT_FILTERS} onChange={vi.fn()} refresh={false} connected />)
    await flush()
    expect(shown()).toBe('照片')
  })

  it('命令行或 MCP 增删文件夹（RootsChanged）时重新读取文件夹列表与筛选', async () => {
    const calls = mockCore({ list_roots: () => ROOTS, get_folder_filter: () => ({ custom: false, entries: [] }) })
    render(<FilterBar filters={DEFAULT_FILTERS} onChange={vi.fn()} refresh={false} connected />)
    await flush()
    await emitEvent('core-event', { type: 'RootsChanged' })
    expect(count(calls, 'list_roots')).toBe(2)
    expect(count(calls, 'get_folder_filter')).toBe(2)
  })

  it('还没有添加文件夹时文件夹筛选不可展开，悬停说明原因；添加后恢复', async () => {
    let roots: RootInfo[] = []
    mockCore({ list_roots: () => roots, get_folder_filter: () => ({ custom: false, entries: [] }) })
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
  })

  it('选“全部文件夹”时保存为非自定义，保留列表与勾选', async () => {
    const entries = [
      { rootId: 1, rel: '', checked: true },
      { rootId: 1, rel: '2024', checked: true },
    ]
    const calls = mockCore({ list_roots: () => ROOTS, get_folder_filter: () => ({ custom: true, entries }) })
    const onChange = vi.fn()
    render(<Harness spy={onChange} />)
    await flush()
    expect(onChange).toHaveBeenLastCalledWith({ ...DEFAULT_FILTERS, folders: [{ rootId: 1, rel: '2024' }] })

    await choose('全部文件夹')
    expect(calls.filter((c) => c.cmd === 'set_folder_filter').map((c) => c.args)).toEqual([{ custom: false, entries }])
    expect(shown()).toBe('全部文件夹')
    expect(onChange).toHaveBeenLastCalledWith({ ...DEFAULT_FILTERS, folders: [] })
  })

  it('选“自定义”：有已勾选的项时直接切换，没有时打开弹窗', async () => {
    let entries = [{ rootId: 2, rel: '', checked: true }]
    const calls = mockCore({ list_roots: () => ROOTS, get_folder_filter: () => ({ custom: false, entries }) })
    const onChange = vi.fn()
    render(<Harness spy={onChange} />)
    await flush()

    await choose('自定义')
    expect(calls.filter((c) => c.cmd === 'set_folder_filter').map((c) => c.args)).toEqual([{ custom: true, entries }])
    expect(findButton('保存')).toBeNull()
    expect(shown()).toBe('合同')
    expect(onChange).toHaveBeenLastCalledWith({ ...DEFAULT_FILTERS, folders: [{ rootId: 2, rel: '' }] })

    // 已是自定义时再选一次打开弹窗
    await choose('自定义')
    expect(findButton('保存')).not.toBeNull()
    await click(button('取消'))

    entries = [{ rootId: 2, rel: '', checked: false }]
    await emitEvent('core-event', { type: 'RootsChanged' })
    await choose('自定义')
    expect(findButton('保存')).not.toBeNull()
    expect(count(calls, 'set_folder_filter')).toBe(1)
  })

  it('弹窗保存后更新生效范围', async () => {
    mockCore({ list_roots: () => [root(1, 'D:\\照片'), root(2, 'D:\\文档')], get_folder_filter: () => ({ custom: false, entries: [] }) })
    const onChange = vi.fn()
    render(<FilterBar filters={DEFAULT_FILTERS} onChange={onChange} refresh={false} connected />)
    await flush()
    await click(button('文件夹'))
    await click(button('选择文件夹'))
    const box = [...document.querySelectorAll('label')].find((l) => l.textContent === '文档')?.querySelector('input')
    await click(box as HTMLElement)
    await click(button('保存'))
    expect(findButton('保存')).toBeNull()
    expect(shown()).toBe('文档')
    expect(onChange).toHaveBeenLastCalledWith({ ...DEFAULT_FILTERS, folders: [{ rootId: 2, rel: '' }] })
  })
})
