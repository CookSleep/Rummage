// @vitest-environment jsdom

import { describe, expect, it, vi } from 'vitest'
import type { ModelFileStatus, SetupStatus } from '../../types'

// `isAndroid` 在 src/lib/platform.ts 加载时求值，要在导入被测模块之前改掉 User-Agent
vi.hoisted(() => {
  Object.defineProperty(navigator, 'userAgent', { value: 'Mozilla/5.0 (Linux; Android 17) AppleWebKit/537.36', configurable: true })
})

import { button, click, device, flush, mockCore, render, selftest, setupStatus, text } from '../../test/tauri'
import ModelsTab from '../settings/ModelsTab'
import { NPU_DEVICE, requiredModel } from './ModelDownload'
import ModelStep from './ModelStep'
import SelftestStep from './SelftestStep'
import { selftestText } from '../../lib/device'

const file = (name: string, ready: boolean): ModelFileStatus => ({ name, bytes: 500 * 2 ** 20, ready, partialBytes: 0, verified: ready ? true : null, url: `https://example.com/${name}` })
const GENERIC = 'embeddinggemma-2-740m.litertlm'
const NPU = 'embeddinggemma-2-740m_Qualcomm_SM8550.litertlm'

/** 芯片有 NPU 版的手机；`generic`、`npu` 为两个模型是否已装好 */
function phone(generic: boolean, npu: boolean, over: Partial<SetupStatus> = {}): SetupStatus {
  const g = file(GENERIC, generic)
  return setupStatus({ model: g, mmproj: g, npu: file(NPU, npu), npuChip: '高通 SM8550', ...over })
}

describe('Android 的模型', () => {
  it('芯片有 NPU 版时先要 NPU 模型；NPU 检测未通过或已装好通用模型时改要通用模型', () => {
    expect(requiredModel(phone(false, false)).npu).toBe(true)
    expect(requiredModel(phone(false, true)).npu).toBe(true)
    expect(requiredModel(phone(true, false)).npu).toBe(false)
    const failed = selftest({ devices: [device({ id: NPU_DEVICE, gpu: true, name: 'NPU', status: 'failed' })] })
    expect(requiredModel(phone(false, true, { selftest: failed })).npu).toBe(false)
    const blocked = selftest({ blocked: [{ id: NPU_DEVICE, name: 'NPU', pciId: null, reason: null }] })
    expect(requiredModel(phone(false, true, { selftest: blocked })).npu).toBe(false)
    // 芯片没有 NPU 版
    expect(requiredModel(setupStatus({ npu: null })).npu).toBe(false)
  })

  it('引导只列出要下载的 NPU 模型，下载时带上 npu', async () => {
    const calls = mockCore({ download_models: () => null })
    render(<ModelStep status={phone(false, false)} mmproj={true} onProxy={vi.fn()} />)
    await flush()
    expect(text()).toContain('NPU 模型')
    expect(text()).toContain('高通 SM8550')
    expect(text()).not.toContain('通用模型')
    expect(text()).not.toContain('图片与音视频模型')
    await click(button('开始下载'))
    expect(calls.find((c) => c.cmd === 'download_models')?.args).toMatchObject({ npu: true })
  })

  it('暂停的下载保留进度条，点继续图标续传，不另有下载按钮；设置页同样从进度条继续，行中只留删除', async () => {
    const calls = mockCore({ download_models: () => null })
    const half = { ...file(NPU, false), partialBytes: 250 * 2 ** 20 }
    render(<ModelStep status={phone(false, false, { npu: half })} mmproj={true} onProxy={vi.fn()} />)
    await flush()
    expect(text()).toContain('已暂停：NPU 模型')
    expect(text()).toContain('250 MB / 500 MB')
    expect([...document.querySelectorAll('button')].some((b) => b.textContent?.includes('开始下载'))).toBe(false)
    await click(document.querySelector<HTMLButtonElement>('button[aria-label="继续下载"]')!)
    expect(calls.find((c) => c.cmd === 'download_models')?.args).toMatchObject({ npu: true })

    const { container } = render(<ModelsTab setup={phone(true, false, { npu: half })} acted={false} onAct={vi.fn()} />)
    await flush()
    expect(container.textContent).toContain('已暂停：NPU 模型')
    expect([...container.querySelectorAll('button')].some((b) => b.textContent === '继续下载')).toBe(false)
    expect(container.querySelector('button[aria-label="继续下载"]')).not.toBeNull()
  })

  it('只有 NPU 模型时说明只测 NPU，不能只用 CPU；装好通用模型后才列出 CPU 与 GPU', () => {
    expect(selftestText(phone(false, true))).toContain('用 NPU 计算几条样本')
    expect(selftestText(phone(false, true))).toContain('CPU 和 GPU 要用通用模型')
    expect(selftestText(phone(true, true))).toContain('用 CPU、GPU 和 NPU 计算')
    expect(selftestText(phone(true, false, { npu: null }))).toContain('用 CPU 和 GPU 计算')
    mockCore({})
    const { container } = render(<SelftestStep selftest={selftest({})} model cpu={false} text="" />)
    expect([...container.querySelectorAll('button')].some((b) => b.textContent === '只用 CPU')).toBe(false)
    expect(container.textContent).not.toContain('只用 CPU')
  })

  it('NPU 检测未通过后，引导改为下载通用模型，已装好的 NPU 模型照常列出', async () => {
    const calls = mockCore({ download_models: () => null })
    const failed = selftest({ devices: [device({ id: NPU_DEVICE, gpu: true, name: 'NPU', status: 'failed' })] })
    render(<ModelStep status={phone(false, true, { selftest: failed })} mmproj={true} onProxy={vi.fn()} />)
    await flush()
    expect(text()).toContain('通用模型')
    expect(text()).toContain('NPU 模型')
    await click(button('开始下载'))
    expect(calls.find((c) => c.cmd === 'download_models')?.args).toMatchObject({ npu: false })
  })

  it('设置的模型页列出 NPU 模型与通用模型，各自校验与删除；删除其中一个时说明改用另一个并重建', async () => {
    const calls = mockCore({ verify_model: () => null, delete_model: () => null })
    render(<ModelsTab setup={phone(true, true)} acted={false} onAct={vi.fn()} />)
    await flush()
    expect(text()).toContain('NPU 模型')
    expect(text()).toContain('通用模型')
    expect(text()).not.toContain('文字模型')
    const verify = [...document.querySelectorAll('button')].filter((b) => b.textContent === '校验')
    await click(verify[0])
    expect(calls.find((c) => c.cmd === 'verify_model')?.args).toMatchObject({ npu: true })
    const remove = [...document.querySelectorAll('button')].filter((b) => b.textContent === '删除')
    await click(remove[0])
    expect(text()).toContain('之后改用通用模型')
  })
})
