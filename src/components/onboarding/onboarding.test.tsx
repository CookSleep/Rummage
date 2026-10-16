// @vitest-environment jsdom

import { act } from 'react'
import { describe, expect, it, vi } from 'vitest'
import type { Estimate, RootInfo } from '../../types'

import { button, click, flush, mockCore, render, selftest, setupStatus, text } from '../../test/tauri'
import EstimateStep from './EstimateStep'
import FoldersStep from './FoldersStep'
import Onboarding from './Onboarding'

const ROOT: RootInfo = { id: 1, path: 'D:\\照片', offline: false, files: 5, excludes: [] }
const KINDS = { documents: true, images: true, audio: false, video: false }
const ESTIMATE: Estimate = { scanning: false, roots: [{ id: 1, path: 'D:\\照片', files: 5, kinds: { image: { files: 5, bytes: 5_000_000 } } }], freeGb: 100 }

/** 第一次调用失败，之后成功 */
function failOnce(message: string, value: unknown) {
  let failed = false
  return () => {
    if (failed) return value
    failed = true
    throw { code: 'db', message }
  }
}

describe('FoldersStep', () => {
  it('读取文件夹列表失败时显示原因与“重试”，重试成功后显示文件夹并去掉错误条', async () => {
    const calls = mockCore({ list_roots: failOnce('无法读取文件夹列表：索引数据库不可用。', [ROOT]) })
    render(<FoldersStep />)
    await flush()
    expect(text()).toContain('无法读取文件夹列表：索引数据库不可用。')
    expect(text()).not.toContain('D:\\照片')

    await click(button('重试'))
    expect(calls.filter((c) => c.cmd === 'list_roots')).toHaveLength(2)
    expect(text()).toContain('D:\\照片')
    expect(text()).toContain('5 个文件')
    expect(text()).not.toContain('无法读取文件夹列表')
  })
})

describe('EstimateStep', () => {
  it('预估失败时显示原因与“重试”，重试成功后显示各文件夹的统计', async () => {
    mockCore({ index_estimate: failOnce('无法统计文件：索引数据库不可用。', ESTIMATE) })
    render(<EstimateStep kinds={KINDS} selftest={selftest()} minFreeGb={5} />)
    await flush()
    expect(text()).toContain('无法统计文件：索引数据库不可用。')

    await click(button('重试'))
    expect(text()).toContain('D:\\照片')
    expect(text()).toContain('图片 5')
    expect(text()).not.toContain('无法统计文件')
  })
})

describe('Onboarding', () => {
  /** 从第 2 步（文件类型）开始的引导，开着视频，识别方式为“找物体” */
  async function atKinds(setParam: () => unknown) {
    const calls = mockCore({
      get_params: () => ({ index_documents: true, index_images: true, index_audio: true, index_video: true, video_preset: 1 }),
      list_roots: () => [ROOT],
      set_param: setParam,
    })
    render(<Onboarding status={setupStatus({ onboarded: false, step: 1 })} coreStatus="running" onStatus={vi.fn()} onDone={vi.fn()} />)
    await flush()
    return calls
  }

  const preset = (title: string) => {
    const el = [...document.querySelectorAll<HTMLElement>('[role="radio"]')].find((r) => r.textContent?.startsWith(title))
    if (!el) throw new Error(`找不到识别方式：${title}`)
    return el
  }

  it('选择视频识别方式时保存 video_preset，返回的提示显示在下方', async () => {
    const note = '设置已保存，但未能安排重建索引，请稍后再试。'
    const calls = await atKinds(() => note)
    expect(preset('找物体').getAttribute('aria-checked')).toBe('true')

    await click(preset('找动作'))
    expect(calls.filter((c) => c.cmd === 'set_param').map((c) => c.args)).toEqual([{ key: 'video_preset', value: 2 }])
    expect(preset('找动作').getAttribute('aria-checked')).toBe('true')
    expect(text()).toContain(note)
  })

  it('保存识别方式成功且没有提示时不显示文字', async () => {
    await atKinds(() => null)
    await click(preset('找说话内容'))
    expect(preset('找说话内容').getAttribute('aria-checked')).toBe('true')
    expect(text()).not.toContain('未能')
  })

  it('保存识别方式失败时回到原来的方式并显示原因', async () => {
    await atKinds(() => {
      throw { code: 'db', message: '设置未能保存：索引数据库不可用。' }
    })
    await click(preset('找动作'))
    expect(preset('找物体').getAttribute('aria-checked')).toBe('true')
    expect(preset('找动作').getAttribute('aria-checked')).toBe('false')
    expect(text()).toContain('设置未能保存：索引数据库不可用。')
  })

  it('文件类型保存失败时开关回到原样并显示原因', async () => {
    await atKinds(() => {
      throw { code: 'db', message: '设置未能保存：索引数据库不可用。' }
    })
    const video = () => [...document.querySelectorAll('button[aria-pressed]')].find((b) => b.textContent?.startsWith('视频'))
    await click(video() as HTMLElement)
    expect(video()?.getAttribute('aria-pressed')).toBe('true')
    expect(text()).toContain('设置未能保存：索引数据库不可用。')
  })

  it('连续切换类型时，前一项保存失败只回滚那一项', async () => {
    let fail = () => {}
    mockCore({
      get_params: () => ({ index_documents: true, index_images: true, index_audio: false, index_video: false }),
      list_roots: () => [ROOT],
      // 音频的保存等勾选了视频之后才失败
      set_param: (args) =>
        args.key === 'index_audio'
          ? new Promise((_, reject) => {
            fail = () => reject({ code: 'db', message: '设置未能保存：索引数据库不可用。' })
          })
          : null,
    })
    render(<Onboarding status={setupStatus({ onboarded: false, step: 1 })} coreStatus="running" onStatus={vi.fn()} onDone={vi.fn()} />)
    await flush()
    const card = (title: string) => [...document.querySelectorAll<HTMLElement>('button[aria-pressed]')].find((b) => b.textContent?.startsWith(title))!
    await click(card('音频'))
    await click(card('视频'))
    expect(card('视频').getAttribute('aria-pressed')).toBe('true')
    await act(async () => fail())
    await flush()
    expect(card('音频').getAttribute('aria-pressed')).toBe('false')
    expect(card('视频').getAttribute('aria-pressed')).toBe('true')
  })

  it('读取文件夹列表失败时仍恢复到记下的步骤', async () => {
    mockCore({
      get_params: () => ({ index_documents: true }),
      list_roots: () => {
        throw { code: 'db', message: '无法读取文件夹列表：索引数据库不可用。' }
      },
    })
    render(<Onboarding status={setupStatus({ onboarded: false, step: 1 })} coreStatus="running" onStatus={vi.fn()} onDone={vi.fn()} />)
    await flush()
    expect(text()).toContain('希望按内容查找哪些文件')
  })

  it('读取设置失败时显示“重试”，重试成功后显示文件类型', async () => {
    mockCore({
      get_params: failOnce('无法读取设置：索引数据库不可用。', { index_documents: true }),
      list_roots: () => [ROOT],
    })
    render(<Onboarding status={setupStatus({ onboarded: false, step: 1 })} coreStatus="running" onStatus={vi.fn()} onDone={vi.fn()} />)
    await flush()
    expect(text()).toContain('无法读取设置：索引数据库不可用。')
    expect(text()).not.toContain('希望按内容查找哪些文件')

    await click(button('重试'))
    expect(text()).not.toContain('无法读取设置')
    expect(text()).toContain('希望按内容查找哪些文件')
  })
})
