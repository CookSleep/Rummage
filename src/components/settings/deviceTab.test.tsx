// @vitest-environment jsdom

import { describe, expect, it } from 'vitest'
import type { BlockedGpu } from '../../types'

import { button, click, device, dialog, findButton, mockCore, render, section, selftest, setupStatus, text } from '../../test/tauri'
import DeviceTab from './DeviceTab'

const BLOCKED: BlockedGpu = { id: 'gpu-4080', name: 'RTX 4080 SUPER', pciId: '0000:02:00.0', reason: '显卡驱动多次超时，已屏蔽这块显卡。' }
const CPU = device()
const GPU = device({ id: 'gpu-3070', gpu: true, pciId: '0000:01:00.0', name: 'RTX 3070 Laptop', imageMs: 200, state: 'usable' })

/** 检测结果中有 CPU 与一块显卡，另一块显卡已屏蔽 */
const withGpu = (over = {}) => setupStatus({ selftest: selftest({ devices: [CPU, GPU], selected: 1, blocked: [BLOCKED], ...over }) })

describe('DeviceTab', () => {
  it('已屏蔽的显卡列在“已屏蔽的显卡”中，附屏蔽原因，不在检测结果中', () => {
    mockCore({})
    render(<DeviceTab setup={withGpu()} values={null} />)
    const blocked = text(section('已屏蔽的显卡'))
    expect(blocked).toContain('RTX 4080 SUPER')
    expect(blocked).toContain('显卡驱动多次超时，已屏蔽这块显卡。')
    const results = text(section('检测结果'))
    expect(results).toContain('RTX 3070 Laptop')
    expect(results).not.toContain('RTX 4080 SUPER')
  })

  it('没有已屏蔽的显卡时不显示这一分区', () => {
    mockCore({})
    render(<DeviceTab setup={withGpu({ blocked: [] })} values={null} />)
    expect(() => section('已屏蔽的显卡')).toThrow()
    expect(findButton('解除屏蔽')).toBeNull()
  })

  it('解除屏蔽先确认，确认后以显卡标识调用 unblock_gpu 并关闭确认框', async () => {
    const calls = mockCore({ unblock_gpu: () => null })
    render(<DeviceTab setup={withGpu()} values={null} />)

    await click(button('解除屏蔽', section('已屏蔽的显卡')))
    expect(calls.some((c) => c.cmd === 'unblock_gpu')).toBe(false)
    expect(text(dialog())).toContain('解除后会立即重新检测「RTX 4080 SUPER」')

    await click(button('解除屏蔽', dialog()))
    expect(calls.filter((c) => c.cmd === 'unblock_gpu').map((c) => c.args)).toEqual([{ device: 'gpu-4080' }])
    expect(document.querySelector('[role="dialog"]')).toBeNull()
  })

  it('取消确认时不解除屏蔽', async () => {
    const calls = mockCore({ unblock_gpu: () => null })
    render(<DeviceTab setup={withGpu()} values={null} />)
    await click(button('解除屏蔽', section('已屏蔽的显卡')))
    await click(button('取消', dialog()))
    expect(calls.some((c) => c.cmd === 'unblock_gpu')).toBe(false)
    expect(document.querySelector('[role="dialog"]')).toBeNull()
  })

  it('解除屏蔽失败时在分区下方显示原因', async () => {
    mockCore({
      unblock_gpu: () => {
        throw { code: 'db', message: '无法解除屏蔽：索引数据库不可用。' }
      },
    })
    render(<DeviceTab setup={withGpu()} values={null} />)
    await click(button('解除屏蔽', section('已屏蔽的显卡')))
    await click(button('解除屏蔽', dialog()))
    expect(text(section('已屏蔽的显卡'))).toContain('无法解除屏蔽：索引数据库不可用。')
  })

  it('解除后没能当场重新检测时，在检测结果下方说明', async () => {
    mockCore({ unblock_gpu: () => '已解除屏蔽。正在建立索引，完成当前文件后开始检测。' })
    render(<DeviceTab setup={withGpu()} values={null} />)
    await click(button('解除屏蔽', section('已屏蔽的显卡')))
    await click(button('解除屏蔽', dialog()))
    expect(text(section('检测结果'))).toContain('已解除屏蔽。正在建立索引，完成当前文件后开始检测。')
  })

  it('检测进行中不能解除屏蔽', () => {
    mockCore({})
    render(<DeviceTab setup={withGpu({ running: true })} values={null} />)
    expect((button('解除屏蔽', section('已屏蔽的显卡')) as HTMLButtonElement).disabled).toBe(true)
  })

  it('只剩已屏蔽的显卡时黄条写“显卡已屏蔽”，当前设备为 CPU', () => {
    mockCore({})
    render(<DeviceTab setup={setupStatus({ selftest: selftest({ devices: [CPU], selected: 0, blocked: [BLOCKED] }) })} values={null} />)
    const used = text(section('使用的设备'))
    expect(used).toContain('显卡已屏蔽，已改用 CPU 建立索引，速度较慢。')
    expect(used).toContain('显卡已屏蔽，可以在下方解除屏蔽')
    expect(used).not.toContain('显卡检测未通过')
  })

  it('有显卡检测未通过时黄条写“显卡检测未通过”', () => {
    mockCore({})
    const failed = { ...GPU, status: 'failed' as const, reason: '结果与 CPU 不一致。' }
    render(<DeviceTab setup={setupStatus({ selftest: selftest({ devices: [CPU, failed], selected: 0, blocked: [BLOCKED] }) })} values={null} />)
    expect(text(section('使用的设备'))).toContain('显卡检测未通过，已改用 CPU 建立索引，速度较慢。')
  })

  it('点击“重新检测”调用 run_selftest', async () => {
    const calls = mockCore({ run_selftest: () => null })
    render(<DeviceTab setup={withGpu()} values={null} />)
    expect(findButton('开始检测')).toBeNull()
    await click(button('重新检测'))
    expect(calls.filter((c) => c.cmd === 'run_selftest').map((c) => c.args)).toEqual([{ onboarding: false }])
  })
})
