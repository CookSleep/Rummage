// @vitest-environment jsdom

import { describe, expect, it } from 'vitest'
import type { Appearance, CoreStatus, SetupStatus } from './types'

import { type Handler, button, click, device, emitEvent, findButton, flush, mockCore, render, section, selftest, setupStatus, text } from './test/tauri'
import App from './App'

const APPEARANCE: Appearance = { uiFont: 'custom', uiFontName: 'Foo Sans', monoFont: 'builtin', monoFontName: '' }

/** 后台服务：没有列出的命令返回 null；`over` 中的处理函数测试中途可以替换 */
function core(status: CoreStatus, over: Record<string, Handler> = {}) {
  const handlers: Record<string, Handler> = {
    core_status: () => status,
    setup_status: () => setupStatus(),
    get_appearance: () => APPEARANCE,
    get_params: () => ({}),
    param_defs: () => [],
    get_notices: () => [],
    list_roots: () => [],
    data_dir_notices: () => [],
    db_status: () => ({ error: null, rebuildable: false }),
    fetch_releases: () => '[]',
    storage_info: () => ({ dataDir: 'D:\\RummageData', db: 1024, thumbs: 0, models: 0, logs: 0, other: 0 }),
    ...over,
  }
  const calls = mockCore(handlers)
  return { handlers, calls, count: (cmd: string) => calls.filter((c) => c.cmd === cmd).length }
}

const searchShown = () => findButton('设置') !== null

/** 打开设置并切到某一页 */
async function openSettings(tab: string) {
  await click(button('设置'))
  await click(button(tab, document.querySelector('[aria-label="设置"][role="dialog"]')!))
}

describe('App 启动', () => {
  it('拿到引导状态前只显示后台服务的状态，不闪现搜索页', async () => {
    let resolve: (s: SetupStatus) => void = () => {}
    core('running', { setup_status: () => new Promise<SetupStatus>((r) => (resolve = r)) })
    render(<App />)
    await flush()
    expect(text()).toContain('正在启动后台服务，请稍候')
    expect(searchShown()).toBe(false)

    resolve(setupStatus())
    await flush()
    expect(searchShown()).toBe(true)
    expect(text()).not.toContain('正在启动后台服务')
  })

  it('后台服务启动中、重新启动中显示对应的整页提示，连上后显示搜索页', async () => {
    core('starting')
    render(<App />)
    await flush()
    expect(text()).toContain('正在启动后台服务，请稍候')
    expect(searchShown()).toBe(false)

    await emitEvent('core-status', 'restarting')
    expect(text()).toContain('后台服务正在重新启动，请稍候')
    expect(searchShown()).toBe(false)

    await emitEvent('core-status', 'running')
    expect(searchShown()).toBe(true)
  })

  it('还没完成引导时连上后显示引导', async () => {
    core('running', { setup_status: () => setupStatus({ onboarded: false }) })
    render(<App />)
    await flush()
    expect(text()).toContain('欢迎使用 Rummage')
    expect(searchShown()).toBe(false)
  })

  it('后台服务无法启动时显示搜索页与“后台服务无法启动”，点击“重试”调用 restart_core', async () => {
    const { count } = core('failed')
    render(<App />)
    await flush()
    expect(searchShown()).toBe(true)
    expect(text()).toContain('后台服务无法启动')

    await click(button('重试'))
    expect(count('restart_core')).toBe(1)
  })

  it('读取引导状态失败时显示搜索页，不停在启动提示', async () => {
    core('running', {
      setup_status: () => {
        throw { code: 'db', message: '索引数据库不可用。' }
      },
    })
    render(<App />)
    await flush()
    expect(searchShown()).toBe(true)
    expect(text()).not.toContain('正在启动后台服务')
  })

  it('显示过搜索页后，后台服务断开或无法启动都不再退回整页提示', async () => {
    core('running')
    render(<App />)
    await flush()
    expect(searchShown()).toBe(true)

    await emitEvent('core-status', 'restarting')
    expect(searchShown()).toBe(true)
    expect(text()).toContain('后台服务正在重新启动，请稍候')

    await emitEvent('core-status', 'failed')
    expect(searchShown()).toBe(true)
    expect(text()).toContain('后台服务无法启动')
  })
})

describe('设置：后台服务重新连上后重新读取', () => {
  it('设备页：断开时显示状态，重新连上后按新的引导状态显示', async () => {
    const { handlers, count } = core('running')
    render(<App />)
    await flush()
    await openSettings('设备')
    expect(text()).toContain('检测结果')

    await emitEvent('core-status', 'restarting')
    expect(text()).toContain('后台服务正在重新启动，请稍候')
    expect(text()).not.toContain('检测结果')

    const gpu = device({ id: 'gpu-3070', gpu: true, name: 'RTX 3070 Laptop', imageMs: 200, state: 'usable' })
    handlers.setup_status = () => setupStatus({ selftest: selftest({ devices: [device(), gpu], selected: 1 }) })
    await emitEvent('core-status', 'running')
    expect(count('setup_status')).toBe(2)
    expect(text(section('检测结果'))).toContain('RTX 3070 Laptop')
  })

  it('设备页：读取引导状态失败时显示原因与“重试”，重试成功后显示设备', async () => {
    let fail = true
    core('running', {
      setup_status: () => {
        if (fail) throw { code: 'db', message: '无法读取设备与模型的状态：索引数据库不可用。' }
        return setupStatus()
      },
    })
    render(<App />)
    await flush()
    await openSettings('设备')
    expect(text()).toContain('无法读取设备与模型的状态：索引数据库不可用。')

    fail = false
    await click(button('重试'))
    expect(text()).not.toContain('无法读取设备与模型的状态')
    expect(text()).toContain('检测结果')
  })

  it('模型页：重新连上后按新的模型状态显示', async () => {
    const { handlers } = core('running')
    render(<App />)
    await flush()
    await openSettings('模型')
    expect(findButton('下载')).toBeNull()

    await emitEvent('core-status', 'restarting')
    expect(text()).toContain('后台服务正在重新启动，请稍候')

    const s = setupStatus()
    handlers.setup_status = () => ({ ...s, mmproj: { ...s.mmproj, ready: false } })
    await emitEvent('core-status', 'running')
    expect(findButton('下载')).not.toBeNull()
  })

  it('网络页：断开时代理分区显示状态，重新连上后重新读取并显示代理设置', async () => {
    const { count } = core('running', { setup_status: () => setupStatus({ proxy: { mode: 'custom', url: 'http://127.0.0.1:7890' } }) })
    render(<App />)
    await flush()
    await openSettings('网络')
    const proxyUrl = () => document.querySelector<HTMLInputElement>('[aria-label="代理地址"]')
    expect(proxyUrl()?.value).toBe('http://127.0.0.1:7890')

    await emitEvent('core-status', 'restarting')
    expect(text(section('网络'))).toContain('后台服务正在重新启动，请稍候')
    expect(proxyUrl()).toBeNull()
    // 存在本地的“自动检查更新”不受影响
    expect(text(section('更新'))).toContain('自动检查更新')

    await emitEvent('core-status', 'running')
    expect(count('setup_status')).toBe(2)
    expect(proxyUrl()?.value).toBe('http://127.0.0.1:7890')
  })

  it('外观页：断开时字体分区显示状态，重新连上后显示后台服务中的字体', async () => {
    const { handlers } = core('running')
    render(<App />)
    await flush()
    await openSettings('外观')
    const uiFont = () => document.querySelector<HTMLInputElement>('[aria-label="界面字体名称"]')
    expect(uiFont()?.value).toBe('Foo Sans')

    await emitEvent('core-status', 'restarting')
    expect(text(section('字体'))).toContain('后台服务正在重新启动，请稍候')
    expect(uiFont()).toBeNull()

    handlers.get_appearance = () => ({ ...APPEARANCE, uiFontName: 'Bar Sans' })
    await emitEvent('core-status', 'running')
    expect(uiFont()?.value).toBe('Bar Sans')
  })

  it('数据页：断开时索引分区显示状态，重新连上后可以清空索引', async () => {
    core('running')
    render(<App />)
    await flush()
    await openSettings('数据')
    expect(findButton('清空')).not.toBeNull()

    await emitEvent('core-status', 'failed')
    expect(text(section('索引'))).toContain('后台服务无法启动')
    expect(findButton('清空')).toBeNull()
    // 数据目录不依赖后台服务，照常显示
    expect(text(section('数据目录'))).toContain('D:\\RummageData')

    await emitEvent('core-status', 'running')
    expect(findButton('清空')).not.toBeNull()
  })

  it('参数页：断开时显示状态，重新连上后重新读取参数', async () => {
    const { count } = core('running')
    render(<App />)
    await flush()
    await click(button('设置'))
    expect(count('param_defs')).toBe(1)
    expect(text()).toContain('文件类型')

    await emitEvent('core-status', 'restarting')
    expect(text()).not.toContain('文件类型')

    await emitEvent('core-status', 'running')
    expect(count('param_defs')).toBe(2)
    expect(text()).toContain('文件类型')
  })
})

describe('搜索页：后台服务重新连上后重新读取', () => {
  it('筛选条的文件夹列表在重新连上后重新读取', async () => {
    const { handlers, count } = core('running')
    render(<App />)
    await flush()
    expect(count('list_roots')).toBe(1)

    await emitEvent('core-status', 'restarting')
    handlers.list_roots = () => [{ id: 1, path: 'D:\\照片', offline: false, missing: false, files: 3, excludes: [], scanning: false }]
    await emitEvent('core-status', 'running')
    expect(count('list_roots')).toBe(2)

    await click(button('文件夹'))
    expect([...document.querySelectorAll('[role="option"]')].map((el) => el.textContent)).toEqual(['照片'])
  })
})
