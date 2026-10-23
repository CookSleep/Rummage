// @vitest-environment jsdom

import type { ComponentProps } from 'react'
import { describe, expect, it, vi } from 'vitest'
import type { ParamDef, RootInfo } from '../../types'

// `isAndroid` 在 src/lib/platform.ts 加载时求值，要在导入被测模块之前改掉 User-Agent
vi.hoisted(() => {
  Object.defineProperty(navigator, 'userAgent', { value: 'Mozilla/5.0 (Linux; Android 17) AppleWebKit/537.36', configurable: true })
})

import { button, click, device, flush, mockCore, render, selftest, setupStatus, text } from '../../test/tauri'
import SettingsModal from './SettingsModal'

const APPEARANCE = { uiFont: 'builtin', uiFontName: '', monoFont: 'builtin', monoFontName: '' } as const

function def(key: string, label: string, platform = ''): ParamDef {
  return { key, group: 'Resource', section: '自动暂停', kind: 'Bool', default: true, affectsIndex: false, indexKinds: [], label, help: '', basis: '', platform }
}

const DEFS = [
  def('pause_on_battery', '未接电源时暂停'),
  def('pause_on_low_battery', '低电量时暂停'),
  { ...def('low_battery_percent', '低电量（%）'), kind: { Int: { min: 10, max: 80 } }, default: 40 } as ParamDef,
  def('pause_when_screen_on', '亮屏时暂停', 'android'),
  def('pause_on_fullscreen', '全屏时暂停', 'windows'),
  def('slow_when_hot', '发热时放慢', 'android'),
  { ...def('thermal_slow_headroom', '开始放慢的发热程度', 'android'), kind: { Named: { options: [[70, '稍热'], [80, '较热'], [90, '很热']], unit: '' } }, default: 80 } as ParamDef,
]

async function open(params: Record<string, unknown> = {}, roots: RootInfo[] = [], at: Pick<ComponentProps<typeof SettingsModal>, 'initialTab' | 'initialParam'> = {}) {
  const calls = mockCore({
    param_defs: () => DEFS,
    get_params: () => params,
    get_notices: () => [],
    list_roots: () => roots,
    set_root_excludes: () => null,
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
      {...at}
    />,
  )
  await flush()
  return calls
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

  it('开启“发热时放慢”时显示放慢的发热程度', async () => {
    await open({ slow_when_hot: true, thermal_slow_headroom: 80 })
    await tab('资源')
    expect(text()).toContain('开始放慢的发热程度')
  })

  it('关闭“发热时放慢”时不显示放慢的发热程度', async () => {
    await open({ slow_when_hot: false, thermal_slow_headroom: 80 })
    await tab('资源')
    expect(text()).toContain('发热时放慢')
    expect(text()).not.toContain('开始放慢的发热程度')
  })

  it('开启“低电量时暂停”时显示低电量的门槛', async () => {
    await open({ pause_on_low_battery: true, low_battery_percent: 40 })
    await tab('资源')
    expect(text()).toContain('低电量（%）')
  })

  it('关闭“低电量时暂停”时不显示低电量的门槛', async () => {
    await open({ pause_on_low_battery: false, low_battery_percent: 40 })
    await tab('资源')
    expect(text()).toContain('低电量时暂停')
    expect(text()).not.toContain('低电量（%）')
  })

  it('排除规则下方可以取消勾选“跳过含 .nomedia 的文件夹”，与规则一起保存', async () => {
    const root = { id: 1, path: '/storage/emulated/0/Pictures', offline: false, missing: false, files: 3, excludes: ['.*'], skipNomedia: true, scanning: false }
    const calls = await open({}, [root])
    await click(button('排除 1 条'))
    const label = [...document.querySelectorAll<HTMLElement>('label')].find((el) => el.textContent === '跳过含 .nomedia 的文件夹')!
    const box = label.querySelector('input')!
    expect(box.checked).toBe(true)
    // 点文字也能切换
    await click(label.querySelector('span:last-child')!)
    expect(box.checked).toBe(false)
    await click(button('保存'))
    expect(calls.filter((c) => c.cmd === 'set_root_excludes').map((c) => c.args)).toEqual([{ id: 1, excludes: ['.*'], skipNomedia: false }])
  })

  it('“系统的后台管控”打开本应用的系统应用信息页', async () => {
    const calls = await open()
    await tab('资源')
    expect(text()).toContain('系统的后台管控')
    await click([...document.querySelectorAll<HTMLElement>('button')].find((b) => b.textContent === '打开应用信息')!)
    expect(calls.some((c) => c.cmd === 'open_app_settings')).toBe(true)
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

  it('打开到“资源”页时滚动到指定的参数，切换页后再回来不再滚动', async () => {
    const scrolled: string[] = []
    Element.prototype.scrollIntoView = function (this: Element) {
      scrolled.push(this.textContent ?? '')
    }
    await open({}, [], { initialTab: 'resource', initialParam: 'pause_when_screen_on' })
    expect(text()).toContain('后台运行')
    expect(scrolled).toHaveLength(1)
    expect(scrolled[0]).toContain('亮屏时暂停')
    await tab('外观')
    await tab('资源')
    expect(scrolled).toHaveLength(1)
    delete (Element.prototype as Partial<Element>).scrollIntoView
  })

  it('窄窗口下打开时把当前页的分页按钮滚到中间', async () => {
    // jsdom 没有布局：分页栏宽 320，“资源”按钮在 300 到 380 之间
    vi.spyOn(HTMLElement.prototype, 'offsetLeft', 'get').mockImplementation(function (this: HTMLElement) {
      return this.textContent === '资源' ? 300 : 0
    })
    vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(80)
    vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(320)
    await open({}, [], { initialTab: 'resource' })
    expect(document.querySelector('nav')!.scrollLeft).toBe(180)
    expect(document.querySelector('nav [aria-current="page"]')!.textContent).toBe('资源')
  })

  it('高级页的诊断中没有“日志目录”', async () => {
    await open()
    await tab('高级')
    expect(text()).toContain('导出诊断包')
    expect(text()).not.toContain('日志目录')
  })
})
