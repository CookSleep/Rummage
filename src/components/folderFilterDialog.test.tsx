// @vitest-environment jsdom

import { act } from 'react'
import { describe, expect, it, vi } from 'vitest'
import type { FolderFilter, PickedFolder, RootInfo } from '../types'

import { button, click, findButton, mockCore, render, text } from '../test/tauri'
import FolderFilterDialog from './FolderFilterDialog'

const root = (id: number, path: string): RootInfo => ({ id, path, offline: false, missing: false, files: 10, excludes: [], skipNomedia: true, scanning: false })

const ROOTS = [root(1, 'D:\\照片'), root(2, 'D:\\文档')]

/** 文字为 `name` 的勾选框 */
function box(name: string) {
  const el = [...document.querySelectorAll('label')].find((l) => l.textContent === name)?.querySelector('input')
  if (!el) throw new Error(`找不到勾选框：${name}`)
  return el
}

/** 已勾选项的文字 */
const checkedNames = () => [...document.querySelectorAll('label')].filter((l) => l.querySelector('input')?.checked).map((l) => l.textContent)

function open(filter: FolderFilter, handlers: Record<string, (args: Record<string, unknown>) => unknown> = {}) {
  const calls = mockCore(handlers)
  const onSaved = vi.fn()
  const onClose = vi.fn()
  render(<FolderFilterDialog roots={ROOTS} filter={filter} onSaved={onSaved} onClose={onClose} />)
  return { calls, onSaved, onClose }
}

const saves = (calls: { cmd: string; args: Record<string, unknown> }[]) => calls.filter((c) => c.cmd === 'set_folder_filter').map((c) => c.args)

/** `pick_subfolder` 依次返回 `results` 中的结果 */
function picker(results: (PickedFolder | null)[]) {
  return { pick_subfolder: () => results.shift() ?? null }
}

const FILTER: FolderFilter = {
  custom: true,
  entries: [
    { rootId: 1, rel: '', checked: true },
    { rootId: 1, rel: '2024', checked: true },
    { rootId: 1, rel: '2023', checked: false },
    { rootId: 2, rel: '', checked: true },
  ],
}

describe('FolderFilterDialog', () => {
  it('打开时用已保存的勾选，有已勾选子文件夹的根目录默认展开，子文件夹按名称排序', () => {
    open(FILTER)
    expect(checkedNames()).toEqual(['照片', '2024', '文档'])
    expect([...document.querySelectorAll('label')].map((l) => l.textContent)).toEqual(['照片', '2023', '2024', '文档'])
    // 只有「照片」被子文件夹缩小了范围
    expect(text().match(/仅子文件夹/g)).toHaveLength(1)
    expect(button('收起子文件夹').getAttribute('aria-expanded')).toBe('true')
    expect(button('展开子文件夹').getAttribute('aria-expanded')).toBe('false')
  })

  it('取消根目录的勾选后子文件夹变淡、不能切换，但保留勾选状态', async () => {
    open(FILTER)
    const sub = box('2024').closest('label')?.querySelector('span:last-child > span') as HTMLElement
    document.elementFromPoint = () => sub
    act(() => {
      sub.dispatchEvent(new MouseEvent('mouseenter'))
    })
    expect(text()).toContain('D:\\照片\\2024')
    act(() => {
      sub.dispatchEvent(new MouseEvent('mouseleave'))
    })

    await click(box('照片'))
    expect(box('2024').disabled).toBe(true)
    expect(box('2024').checked).toBe(true)
    expect(text()).not.toContain('仅子文件夹')
    act(() => {
      sub.dispatchEvent(new MouseEvent('mouseenter'))
    })
    expect(text()).toContain('勾选「照片」后生效')

    await click(box('照片'))
    expect(box('2024').disabled).toBe(false)
    expect(checkedNames()).toEqual(['照片', '2024', '文档'])
  })

  it('清扫按钮清掉该根目录及其子文件夹的勾选，“清除勾选”清掉全部', async () => {
    open(FILTER)
    await click(button('清除「照片」中的勾选'))
    expect(checkedNames()).toEqual(['文档'])
    await click(box('照片'))
    await click(button('清除勾选'))
    expect(checkedNames()).toEqual([])
  })

  it('没有勾选时“保存”不可用', async () => {
    const { calls, onSaved } = open({ custom: false, entries: [] })
    const save = button('保存')
    expect(save.getAttribute('aria-disabled')).toBe('true')
    await click(save)
    expect(saves(calls)).toEqual([])
    expect(onSaved).not.toHaveBeenCalled()
    document.elementFromPoint = () => save
    act(() => {
      save.dispatchEvent(new MouseEvent('mouseenter'))
    })
    expect(text()).toContain('至少勾选一个文件夹')
  })

  it('保存：包含全部根目录与子文件夹', async () => {
    const { calls, onSaved } = open(FILTER)
    await click(button('保存'))
    const entries = [
      { rootId: 1, rel: '', checked: true },
      { rootId: 2, rel: '', checked: true },
      { rootId: 1, rel: '2024', checked: true },
      { rootId: 1, rel: '2023', checked: false },
    ]
    expect(saves(calls)).toEqual([{ custom: true, entries }])
    expect(onSaved).toHaveBeenCalledWith({ custom: true, entries })
  })

  it('只勾选了全部根目录时保存为全部文件夹；没有记录的根目录按未勾选写入', async () => {
    const { calls } = open({ custom: true, entries: [{ rootId: 1, rel: '', checked: true }, { rootId: 1, rel: '2024', checked: true }] })
    await click(box('2024'))
    await click(button('保存'))
    expect(saves(calls)[0].custom).toBe(true)

    await click(box('文档'))
    await click(button('保存'))
    expect(saves(calls)[1]).toEqual({
      custom: false,
      entries: [
        { rootId: 1, rel: '', checked: true },
        { rootId: 2, rel: '', checked: true },
        { rootId: 1, rel: '2024', checked: false },
      ],
    })
  })

  it('取消不写入', async () => {
    const { calls, onClose } = open(FILTER)
    await click(box('文档'))
    await click(button('取消'))
    expect(onClose).toHaveBeenCalled()
    expect(saves(calls)).toEqual([])
  })

  it('× 从列表中移除子文件夹', async () => {
    const { calls } = open(FILTER)
    await click(findButton('从列表中移除') as HTMLElement)
    expect([...document.querySelectorAll('label')].map((l) => l.textContent)).toEqual(['照片', '2024', '文档'])
    await click(button('保存'))
    expect((saves(calls)[0].entries as unknown[]).length).toBe(3)
  })

  it('添加子文件夹：ok 加入并勾选（连同根目录），已在列表中时只勾选；root 勾选根目录；取消时不变', async () => {
    const { calls } = open(
      { custom: true, entries: [{ rootId: 1, rel: '2024', checked: true }, { rootId: 1, rel: '2023', checked: false }] },
      picker([{ status: 'ok', rel: '2022\\春', path: 'D:\\照片\\2022\\春' }, { status: 'ok', rel: '2023', path: 'D:\\照片\\2023' }, { status: 'root', rel: '', path: 'D:\\照片' }, null]),
    )
    await click(button('添加子文件夹'))
    expect(calls.find((c) => c.cmd === 'pick_subfolder')?.args).toEqual({ rootId: 1 })
    expect(checkedNames()).toEqual(['照片', '2022\\春', '2024'])
    await click(button('添加子文件夹'))
    expect(checkedNames()).toEqual(['照片', '2022\\春', '2023', '2024'])
    await click(button('添加子文件夹'))
    expect(checkedNames()).toEqual(['照片', '2022\\春', '2023', '2024'])
    await click(button('添加子文件夹'))
    expect(checkedNames()).toEqual(['照片', '2022\\春', '2023', '2024'])
    expect(document.querySelectorAll('[role="dialog"]')).toHaveLength(1)
  })

  it('选到根目录以外时提示，可重新选择', async () => {
    const { calls } = open(
      { custom: true, entries: [{ rootId: 1, rel: '2024', checked: true }] },
      picker([{ status: 'outside', rel: '', path: 'E:\\下载' }, { status: 'ok', rel: '2021', path: 'D:\\照片\\2021' }]),
    )
    await click(button('添加子文件夹'))
    expect(text()).toContain('请选择「照片」中的文件夹')
    expect(text()).toContain('「E:\\下载」不在「照片」中。')
    await click(button('重新选择'))
    expect(calls.filter((c) => c.cmd === 'pick_subfolder')).toHaveLength(2)
    expect(text()).not.toContain('请选择「照片」中的文件夹')
    expect(checkedNames()).toEqual(['照片', '2021', '2024'])
  })

  it('选到暂无已索引文件的文件夹时提示，可仍然添加或重新选择', async () => {
    const { calls } = open(
      { custom: true, entries: [{ rootId: 1, rel: '2024', checked: true }] },
      picker([{ status: 'empty', rel: '空', path: 'D:\\照片\\空' }, { status: 'empty', rel: '空', path: 'D:\\照片\\空' }]),
    )
    await click(button('添加子文件夹'))
    expect(text()).toContain('此文件夹中暂无已索引的文件')
    expect(findButton('取消', document.querySelectorAll('[role="dialog"]')[1])).toBeNull()
    await click(button('重新选择'))
    expect(calls.filter((c) => c.cmd === 'pick_subfolder')).toHaveLength(2)
    await click(button('仍然添加'))
    expect(text()).not.toContain('此文件夹中暂无已索引的文件')
    expect(checkedNames()).toEqual(['照片', '2024', '空'])
  })
})
