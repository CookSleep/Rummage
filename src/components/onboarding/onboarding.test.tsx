// @vitest-environment jsdom

import { act } from 'react'
import { describe, expect, it, vi } from 'vitest'
import type { Estimate, RootInfo } from '../../types'

import { button, click, emitEvent, flush, mockCore, render, selftest, setupStatus, text } from '../../test/tauri'
import Toaster from '../Toast'
import EstimateStep from './EstimateStep'
import FoldersStep from './FoldersStep'
import Onboarding from './Onboarding'

const ROOT: RootInfo = { id: 1, path: 'D:\\照片', offline: false, missing: false, files: 5, excludes: [], scanning: false }
/** 添加按钮连同其下一直显示的提示 */
const ADD = '添加文件夹只添加需要搜索的文件夹，不建议添加整个磁盘'
const KINDS = { documents: true, images: true, audio: false, video: false }
const ESTIMATE: Estimate = { scanning: false, roots: [{ id: 1, path: 'D:\\照片', files: 5, kinds: { image: { files: 5, bytes: 5_000_000, slices: 0 } }, scanning: false }], freeGb: 100 }

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

  it('后台服务还在统计的文件夹显示“正在统计文件”，统计完后显示文件数', async () => {
    let root = { ...ROOT, files: 0, scanning: true }
    mockCore({ list_roots: () => [root] })
    render(<FoldersStep />)
    await flush()
    expect(text()).toContain('正在统计文件')
    expect(text()).not.toContain('0 个文件')

    root = { ...ROOT, scanning: false }
    await emitEvent('core-event', { type: 'ScanProgress', rootId: 1, done: true })
    expect(text()).toContain('5 个文件')
    expect(text()).not.toContain('正在统计文件')
  })

  it('不可访问的文件夹区分设备未连接与找不到文件夹', async () => {
    mockCore({
      list_roots: () => [
        { ...ROOT, path: 'E:\\移动硬盘', offline: true },
        { ...ROOT, id: 2, path: 'D:\\旧资料', offline: true, missing: true },
      ],
    })
    render(<FoldersStep />)
    await flush()
    expect(text()).toContain('E:\\移动硬盘设备未连接')
    expect(text()).toContain('D:\\旧资料找不到文件夹')
  })

  it('添加处一直提示只添加需要的文件夹；添加整个磁盘后建议移除，可当场移除', async () => {
    let roots = [ROOT]
    const calls = mockCore({
      list_roots: () => roots,
      add_root: () => {
        roots = [...roots, { ...ROOT, id: 2, path: 'C:\\' }]
        return { id: 2, path: 'C:\\', wholeDrive: true }
      },
      remove_root: () => {
        roots = roots.filter((r) => r.id !== 2)
      },
    })
    render(<FoldersStep />)
    await flush()
    expect(text()).toContain('只添加需要搜索的文件夹，不建议添加整个磁盘')
    expect(text()).not.toContain('建议只添加需要的文件夹')

    await click(button(ADD))
    await flush()
    expect(text()).toContain('「C:\\」是整个磁盘')

    await click(button('移除'))
    expect(calls.find((c) => c.cmd === 'remove_root')?.args).toEqual({ id: 2 })
    expect(text()).not.toContain('「C:\\」是整个磁盘')
  })

  it('添加普通文件夹时不弹窗', async () => {
    mockCore({ list_roots: () => [ROOT], add_root: () => ({ id: 1, path: 'D:\\照片', wholeDrive: false }) })
    render(<FoldersStep />)
    await flush()
    await click(button(ADD))
    await flush()
    expect(text()).not.toContain('建议只添加需要的文件夹')
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

  it('还在统计的文件夹只显示“正在统计文件”，不显示各类型的数量', async () => {
    const est: Estimate = { ...ESTIMATE, scanning: true, roots: [{ ...ESTIMATE.roots[0], files: 0, kinds: {}, scanning: true }] }
    mockCore({ index_estimate: () => est })
    render(<EstimateStep kinds={KINDS} selftest={selftest()} minFreeGb={5} />)
    await flush()
    expect(text()).toContain('正在统计文件')
    expect(text()).not.toContain('图片 0')
    expect(text()).not.toContain('共 0 个文件')
    expect(text()).toContain('还在统计文件，数量会继续增加')
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

describe('Onboarding 导入配置', () => {
  it('第 1 步导入后列出导入的文件夹，文件类型也是导入后的值', async () => {
    let roots: RootInfo[] = []
    let params = { index_documents: true, index_images: false, index_audio: false, index_video: false }
    const calls = mockCore({
      get_params: () => params,
      list_roots: () => roots,
      import_config: () => ({ params: { index_video: true } }),
      apply_config: () => {
        roots = [ROOT]
        params = { ...params, index_video: true }
        return { added: 1, missing: 0, inaccessible: 0, failed: 0, droppedParams: 0, warning: null }
      },
    })
    render(
      <>
        <Toaster />
        <Onboarding status={setupStatus({ onboarded: false, step: 0 })} coreStatus="running" onStatus={vi.fn()} onDone={vi.fn()} />
      </>,
    )
    await flush()
    expect(text()).toContain('还没有添加文件夹')

    await click(button('导入配置'))
    // 引导中不确认重建，直接导入
    expect(calls.some((c) => c.cmd === 'apply_config')).toBe(true)
    expect(text()).toContain('配置已导入')
    // 后台服务添加文件夹后推送 RootsChanged
    await emitEvent('core-event', { type: 'RootsChanged' })
    expect(text()).toContain(ROOT.path)

    await click(button('下一步'))
    const video = [...document.querySelectorAll('button[aria-pressed]')].find((b) => b.textContent?.startsWith('视频'))
    expect(video?.getAttribute('aria-pressed')).toBe('true')
  })

  it('配置来自其他平台时显示原因，不导入', async () => {
    const calls = mockCore({
      get_params: () => ({ index_documents: true }),
      list_roots: () => [],
      import_config: () => {
        throw { code: 'bad_request', message: '这是在 Android 上导出的配置，只能导入 Android 版 Rummage。' }
      },
    })
    render(<Onboarding status={setupStatus({ onboarded: false, step: 0 })} coreStatus="running" onStatus={vi.fn()} onDone={vi.fn()} />)
    await flush()
    await click(button('导入配置'))
    expect(text()).toContain('这是在 Android 上导出的配置，只能导入 Android 版 Rummage。')
    expect(calls.some((c) => c.cmd === 'apply_config')).toBe(false)
  })
})
