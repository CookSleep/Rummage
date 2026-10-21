// @vitest-environment jsdom

import { describe, expect, it, vi } from 'vitest'
import type { ParamDef } from '../../types'

// `isAndroid` 在 src/lib/platform.ts 加载时求值，要在导入被测模块之前改掉 User-Agent
vi.hoisted(() => {
  Object.defineProperty(navigator, 'userAgent', { value: 'Mozilla/5.0 (Linux; Android 17) AppleWebKit/537.36', configurable: true })
})

import { click, device, flush, mockCore, render, selftest, setupStatus, text } from '../../test/tauri'
import SettingsModal from './SettingsModal'

const APPEARANCE = { uiFont: 'builtin', uiFontName: '', monoFont: 'builtin', monoFontName: '' } as const

function def(key: string, label: string, platform = ''): ParamDef {
  return { key, group: 'Resource', section: '自动暂停', kind: 'Bool', default: true, affectsIndex: false, indexKinds: [], label, help: '', basis: '', platform }
}

const DEFS = [def('pause_on_battery', '未接电源时暂停'), def('pause_when_screen_on', '亮屏时暂停', 'android'), def('pause_on_fullscreen', '全屏时暂停', 'windows')]

async function open() {
  mockCore({
    param_defs: () => DEFS,
    get_params: () => ({}),
    get_notices: () => [],
    list_roots: () => [],
    battery_unrestricted: () => true,
    storage_access: () => true,
    storage_info: () => ({ dataDir: '/data/user/0/com.cooksleep.rummage/files', db: 1, thumbs: 1, models: 1, logs: 1, other: 1 }),
  })
  const gpu = device({ id: 'gpu', gpu: true, name: 'Adreno', status: 'passed' })
  render(
    <SettingsModal
      onClose={vi.fn()}
      coreStatus="running"
      setup={setupStatus({ selftest: selftest({ devices: [device(), gpu], selected: 0, blocked: [{ id: 'gpu-x', name: '旧显卡', pciId: null, reason: null }] }) })}
      setupError=""
      onSetup={vi.fn()}
      onSetupRetry={vi.fn()}
      appearance={APPEARANCE}
      onAppearance={vi.fn()}
    />,
  )
  await flush()
}

const tab = (label: string) => click([...document.querySelectorAll<HTMLElement>('nav button')].find((b) => b.textContent === label)!)

describe('Android 上的设置', () => {
  it('参数页只显示 Android 与各平台通用的参数，资源页顶部是“后台运行”', async () => {
    await open()
    await tab('资源')
    expect(text()).toContain('未接电源时暂停')
    expect(text()).toContain('亮屏时暂停')
    expect(text()).not.toContain('全屏时暂停')
    expect(text()).toContain('后台运行')
    expect(text()).toContain('控制建立索引时占用的手机资源')
  })

  it('外观页没有“关闭窗口时”，数据页没有“打开”和“更改位置”', async () => {
    await open()
    await tab('外观')
    expect(text()).toContain('界面字体')
    expect(text()).not.toContain('关闭窗口时')
    await tab('数据')
    expect(text()).toContain('/data/user/0/com.cooksleep.rummage/files')
    expect(text()).not.toContain('更改位置')
    expect([...document.querySelectorAll('button')].some((b) => b.textContent === '打开')).toBe(false)
  })

  it('设备页可以选择设备，说明按 GPU 与 NPU 写', async () => {
    await open()
    await tab('设备')
    expect(text()).toContain('使用的设备')
    expect(text()).toContain('检测结果')
    expect(text()).toContain('重新检测')
    expect(text()).toContain('只能选择通过检测的 GPU 或 NPU')
    expect(text()).toContain('它比 GPU 与 NPU 快')
    expect(text()).not.toContain('显卡驱动')
  })

  it('高级页的诊断中没有“日志目录”', async () => {
    await open()
    await tab('高级')
    expect(text()).toContain('导出诊断包')
    expect(text()).not.toContain('日志目录')
  })
})
