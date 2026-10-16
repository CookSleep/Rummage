// @vitest-environment jsdom

import { describe, expect, it, vi } from 'vitest'

import { button, click, findButton, mockCore, render, text } from '../test/tauri'
import CoreStatusNotice from './CoreStatusNotice'
import LoadError from './LoadError'

describe('CoreStatusNotice', () => {
  it('启动中显示加载指示与“正在启动后台服务”，没有“重试”', () => {
    const { container } = render(<CoreStatusNotice status="starting" />)
    expect(text()).toContain('正在启动后台服务，请稍候')
    expect(container.querySelector('.animate-spin')).not.toBeNull()
    expect(findButton('重试')).toBeNull()
  })

  it('已连上、还没取到引导状态时按正在启动显示', () => {
    render(<CoreStatusNotice status="running" />)
    expect(text()).toContain('正在启动后台服务，请稍候')
  })

  it('重新启动中显示加载指示与“正在重新启动”', () => {
    const { container } = render(<CoreStatusNotice status="restarting" />)
    expect(text()).toContain('后台服务正在重新启动，请稍候')
    expect(container.querySelector('.animate-spin')).not.toBeNull()
  })

  it('无法启动时说明并提供“重试”，点击后调用 restart_core', async () => {
    const calls = mockCore({ restart_core: () => null })
    const { container } = render(<CoreStatusNotice status="failed" />)
    expect(text()).toContain('后台服务无法启动')
    expect(container.querySelector('.animate-spin')).toBeNull()

    await click(button('重试'))
    expect(calls.filter((c) => c.cmd === 'restart_core')).toHaveLength(1)
    expect(text()).not.toContain('无法启动后台服务')
  })

  it('重试失败时在按钮下方显示原因，再次重试时先清掉', async () => {
    let fail = true
    mockCore({
      restart_core: () => {
        if (fail) throw { code: 'spawn', message: '找不到后台服务程序，请重新安装 Rummage。' }
        return null
      },
    })
    render(<CoreStatusNotice status="failed" />)

    await click(button('重试'))
    const msg = '找不到后台服务程序，请重新安装 Rummage。'
    expect(text()).toContain(msg)
    // 原因在按钮之后
    const btn = button('重试')
    expect(btn.nextElementSibling?.textContent).toBe(msg)

    fail = false
    await click(btn)
    expect(text()).not.toContain(msg)
  })
})

describe('LoadError', () => {
  it('显示原因，点击“重试”时重新读取', async () => {
    const onRetry = vi.fn()
    render(<LoadError message="无法读取文件夹列表。" onRetry={onRetry} />)
    expect(text()).toContain('无法读取文件夹列表。')

    await click(button('重试'))
    expect(onRetry).toHaveBeenCalledOnce()
  })
})
