// @vitest-environment jsdom

import { describe, expect, it, vi } from 'vitest'

import { type Handler, button, click, flush, mockCore, render, setupStatus, text, toast } from '../../test/tauri'
import Toaster from '../Toast'
import SettingsModal from './SettingsModal'

const VALUES = { index_documents: true, index_images: true, index_audio: true, index_video: true }
const APPEARANCE = { uiFont: 'builtin', uiFontName: '', monoFont: 'builtin', monoFontName: '' } as const

/** 打开设置（默认在“文件夹”页） */
async function open(setParam: Handler, setup = setupStatus()) {
  const calls = mockCore({ param_defs: () => [], get_params: () => VALUES, get_notices: () => [], list_roots: () => [], set_param: setParam })
  render(
    <>
      <Toaster />
      <SettingsModal onClose={vi.fn()} coreStatus="running" setup={setup} setupError="" onSetup={vi.fn()} onSetupRetry={vi.fn()} appearance={APPEARANCE} onAppearance={vi.fn()} />
    </>,
  )
  await flush()
  return calls
}

/** 文件类型卡片 */
function kind(title: string) {
  const el = [...document.querySelectorAll<HTMLButtonElement>('button[aria-pressed]')].find((b) => b.textContent?.startsWith(title))
  if (!el) throw new Error(`找不到文件类型：${title}`)
  return el
}

describe('FoldersTab 文件类型开关', () => {
  it('保存成功时以参数名调用 set_param，开关随之改变，不显示提示', async () => {
    const calls = await open(() => null)
    expect(kind('图片').getAttribute('aria-pressed')).toBe('true')

    await click(kind('图片'))
    expect(calls.filter((c) => c.cmd === 'set_param').map((c) => c.args)).toEqual([{ key: 'index_images', value: false }])
    expect(kind('图片').getAttribute('aria-pressed')).toBe('false')
    expect(toast()).toBe('')
  })

  it('保存失败时用胶囊通知说明原因，开关保持原样', async () => {
    await open(() => {
      throw { code: 'db', message: '设置未能保存：索引数据库不可用。' }
    })
    await click(kind('音频'))
    expect(toast()).toBe('设置未能保存：索引数据库不可用')
    expect(kind('音频').getAttribute('aria-pressed')).toBe('true')
  })

  it('已保存但未能安排重建索引时，开关改变并用胶囊通知显示返回的提示', async () => {
    await open(() => '设置已保存，但未能安排重建索引，请稍后再试。')
    await click(kind('视频'))
    expect(kind('视频').getAttribute('aria-pressed')).toBe('false')
    expect(toast()).toBe('设置已保存，但未能安排重建索引，请稍后再试')
  })


  it('开着图片等类型但缺少图片与音视频模型时提示，“前往下载”转到模型页', async () => {
    const setup = setupStatus()
    await open(() => null, { ...setup, mmproj: { ...setup.mmproj, ready: false } })
    expect(text()).toContain('尚未下载图片与音视频模型')

    await click(button('前往下载'))
    expect(text()).toContain('识别文件内容需要本地模型')
  })

  it('图片与音视频模型已就绪时不提示', async () => {
    await open(() => null)
    expect(text()).not.toContain('尚未下载图片与音视频模型')
  })
})
