// @vitest-environment jsdom

import { beforeEach, describe, expect, it } from 'vitest'
import type { DeviceSpeed, Estimate, ParamDef } from '../../types'

import { button, click, device, dialog, flush, mockCore, render, selftest, text } from '../../test/tauri'
import { SPEED_STALE_TEXT } from '../../lib/device'
import EstimateStep from './EstimateStep'
import SelftestStep from './SelftestStep'

const SPEED: DeviceSpeed = { textMs: 400, imageMs: 15_000, audioMs: null, videoMs: null, params: { image_tokens: 280, chunk_tokens: 384 } }
const TESTED = selftest({ devices: [device({ speed: SPEED })], selected: 0 })

function def(key: string, label: string, over: Partial<ParamDef> = {}): ParamDef {
  return { key, group: 'Index', section: '图片', kind: { Int: { min: 1, max: 64 } }, default: 1, affectsIndex: false, indexKinds: [], label, help: '说明。', basis: '', platform: '', ...over }
}

const TIERS: [number, string][] = [[140, '较低'], [280, '标准'], [560, '较高']]
const DEFS = [
  def('image_tokens', '图片识别精度', { kind: { Named: { options: TIERS, unit: 'Token' } }, default: 280, affectsIndex: true, indexKinds: ['image'] }),
  def('long_image_tokens', '长图每段识别精度', { kind: { Named: { options: TIERS, unit: 'Token' } }, default: 140, affectsIndex: true, indexKinds: ['image'] }),
  def('video_frame_tokens', '视频画面识别精度', { kind: { Named: { options: TIERS.slice(0, 1), unit: 'Token' } }, default: 140, affectsIndex: true, indexKinds: ['video'] }),
  def('chunk_tokens', '文本分段长度（Token）', { kind: { Int: { min: 128, max: 512 } }, default: 384, affectsIndex: true, indexKinds: ['document'] }),
  def('index_threads', '建立索引线程数', { group: 'Resource', default: 8 }),
  def('video_preset', '视频识别方式'),
  def('selftest_min_cosine', '设备检测通过标准', { group: 'Advanced' }),
]

const VALUES = { image_tokens: 280, long_image_tokens: 140, video_frame_tokens: 140, chunk_tokens: 384, index_threads: 8, video_preset: 1, selftest_min_cosine: 0.99 }

describe('SelftestStep 测速结果', () => {
  it('显示按当前设置的单个文件耗时，悬停说明含义', async () => {
    mockCore({ get_params: () => VALUES })
    render(<SelftestStep selftest={TESTED} model />)
    await flush()
    expect(text()).toContain('15.0 s')
    expect(document.querySelector('[aria-label="按当前设置，每张图片约 15.0 s"]')).not.toBeNull()
    expect(document.querySelector('[aria-label="按当前设置，每段文本约 400 ms"]')).not.toBeNull()
    expect(text()).not.toContain(SPEED_STALE_TEXT)
  })

  it('测速用到的参数改过时提示耗时是修改前的，不重新检测', async () => {
    const calls = mockCore({ get_params: () => ({ ...VALUES, image_tokens: 560 }) })
    render(<SelftestStep selftest={TESTED} model />)
    await flush()
    expect(text()).toContain(SPEED_STALE_TEXT)
    expect(document.querySelector('[aria-label="按修改前的设置，每张图片约 15.0 s"]')).not.toBeNull()
    expect(calls.some((c) => c.cmd === 'run_selftest')).toBe(false)
  })

  it('没有测速结果（旧的检测结果）时显示样本耗时', async () => {
    mockCore({ get_params: () => VALUES })
    render(<SelftestStep selftest={selftest({ devices: [device()], selected: 0 })} model />)
    await flush()
    expect(document.querySelector('[aria-label="检测样本中每张图片的用时"]')).not.toBeNull()
  })
})

describe('SelftestStep 高级', () => {
  beforeEach(() => {
    // jsdom 没有实现，下拉菜单打开时用它让选项保持可见
    Element.prototype.scrollIntoView = () => {}
  })

  it('只列出影响速度的参数，修改影响索引的参数先确认再保存', async () => {
    const calls = mockCore({ get_params: () => VALUES, param_defs: () => DEFS, set_param: () => null })
    render(<SelftestStep selftest={TESTED} model />)
    await flush()
    await click(button('高级'))
    const d = dialog()
    expect(d.textContent).toContain('高级')
    for (const label of ['图片识别精度', '长图每段识别精度', '视频画面识别精度', '文本分段长度（Token）', '建立索引线程数']) {
      expect(d.textContent).toContain(label)
    }
    expect(d.textContent).not.toContain('视频识别方式')
    expect(d.textContent).not.toContain('设备检测通过标准')

    // 第一个下拉菜单是图片识别精度
    await click(d.querySelector<HTMLElement>('[aria-haspopup="listbox"]')!)
    const option = [...document.querySelectorAll<HTMLElement>('[role="option"]')].find((o) => o.textContent === '较高（560 Token）')!
    await click(option)
    expect(dialog().textContent).toContain('修改后需要重建受影响的索引（图片）')
    expect(calls.some((c) => c.cmd === 'set_param')).toBe(false)

    await click(button('修改并重建'))
    expect(calls.filter((c) => c.cmd === 'set_param').map((c) => c.args)).toEqual([{ key: 'image_tokens', value: 560 }])
    // 回到参数弹窗，显示新值
    expect(dialog().textContent).toContain('较高（560 Token）')
    await click(button('完成'))
    expect(document.querySelector('[role="dialog"]')).toBeNull()
  })

  it('取消确认时不保存', async () => {
    const calls = mockCore({ get_params: () => VALUES, param_defs: () => DEFS, set_param: () => null })
    render(<SelftestStep selftest={TESTED} model />)
    await flush()
    await click(button('高级'))
    await click(dialog().querySelector<HTMLElement>('[aria-haspopup="listbox"]')!)
    await click([...document.querySelectorAll<HTMLElement>('[role="option"]')].find((o) => o.textContent === '较低（140 Token）')!)
    await click(button('取消'))
    expect(calls.some((c) => c.cmd === 'set_param')).toBe(false)
    expect(dialog().textContent).toContain('标准（280 Token）')
  })
})

describe('EstimateStep 测速结果', () => {
  const est: Estimate = { scanning: false, roots: [{ id: 1, path: 'D:\\照片', files: 5, kinds: { image: { files: 5, bytes: 5_000_000 } } }], freeGb: 100 }
  const kinds = { documents: false, images: true, audio: false, video: false }

  it('测速用到的参数改过时提示预估按修改前的速度', async () => {
    mockCore({ index_estimate: () => est, get_params: () => ({ ...VALUES, chunk_tokens: 200 }) })
    render(<EstimateStep kinds={kinds} selftest={TESTED} minFreeGb={5} />)
    await flush()
    expect(text()).toContain('参数已修改，预估按修改前测得的速度，重新检测后更新')
  })

  it('参数没改时不提示', async () => {
    mockCore({ index_estimate: () => est, get_params: () => VALUES })
    render(<EstimateStep kinds={kinds} selftest={TESTED} minFreeGb={5} />)
    await flush()
    expect(text()).toContain('预计用时')
    expect(text()).not.toContain('参数已修改')
  })
})
