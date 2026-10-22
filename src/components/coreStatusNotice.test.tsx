// @vitest-environment jsdom

import { describe, expect, it, vi } from 'vitest'

import { button, click, findButton, mockCore, render, text, toast } from '../test/tauri'
import CoreStatusNotice from './CoreStatusNotice'
import LoadError from './LoadError'
import Toaster from './Toast'

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

  it('重试失败时用胶囊通知说明原因', async () => {
    mockCore({
      restart_core: () => {
        throw { code: 'spawn', message: '找不到后台服务程序，请重新安装 Rummage。' }
      },
    })
    render(
      <>
        <Toaster />
        <CoreStatusNotice status="failed" />
      </>,
    )

    await click(button('重试'))
    expect(toast()).toBe('找不到后台服务程序，请重新安装 Rummage')
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
