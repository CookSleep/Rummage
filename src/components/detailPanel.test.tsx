// @vitest-environment jsdom

import { describe, expect, it, vi } from 'vitest'
import type { FileInfo, SearchHit } from '../types'

import { flush, mockCore, render, text } from '../test/tauri'
import DetailPanel from './DetailPanel'

const HIT: SearchHit = {
  fileId: 7,
  path: 'D:\\资料\\报告.docx',
  name: '报告.docx',
  kind: 'document',
  size: 2048,
  mtime: 1_700_000_000_000,
  cloud: false,
  offline: false,
  score: 1,
  cosine: null,
  z: null,
  matchPct: null,
  lowRelevance: false,
  segments: [],
  nameHit: true,
  textSegments: [],
  width: null,
  height: null,
  snippet: null,
}

const INFO: FileInfo = {
  fileId: 7,
  path: HIT.path,
  name: HIT.name,
  ext: 'docx',
  kind: 'document',
  cloud: false,
  offline: false,
  size: 2048,
  mtime: HIT.mtime,
  width: null,
  height: null,
  status: 'stored',
  job: { stage: 'stored', attempts: 1, lastError: null },
  segments: 3,
  indexedAt: 1_700_000_100_000,
  spaces: [],
}

const panel = (connected: boolean) => (
  <DetailPanel hit={HIT} query="" overlay={false} viewportHeight={800} connected={connected} onHide={vi.fn()} onOpen={vi.fn()} onReveal={vi.fn()} onSimilar={vi.fn()} />
)

/** “索引”一节中“状态”一行的文字 */
function state() {
  const dt = [...document.querySelectorAll('dt')].find((el) => el.textContent === '状态')
  return dt?.nextElementSibling?.textContent
}

describe('DetailPanel', () => {
  it('读取到详情后显示索引状态与分段数', async () => {
    mockCore({ file_details: () => INFO })
    render(panel(true))
    await flush()
    expect(state()).toBe('已索引')
    expect(text()).toContain('3 段')
  })

  it('断开期间读取失败显示原因，重新连上后重新读取并恢复', async () => {
    let fail = true
    const calls = mockCore({
      file_details: () => {
        if (fail) throw { code: 'disconnected', message: '后台服务未连接，请稍后再试。' }
        return INFO
      },
    })
    const { rerender } = render(panel(false))
    await flush()
    expect(state()).toBe('后台服务未连接，请稍后再试。')

    fail = false
    rerender(panel(true))
    await flush()
    expect(state()).toBe('已索引')
    expect(calls.filter((c) => c.cmd === 'file_details').map((c) => c.args)).toEqual([{ fileId: 7 }, { fileId: 7 }])
  })

  it('点击文件名与路径时复制', async () => {
    mockCore({ file_details: () => INFO })
    const writeText = vi.fn(() => Promise.resolve())
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true })
    render(panel(true))
    await flush()
    const button = (label: string) => [...document.querySelectorAll('button')].find((el) => el.textContent === label)!
    button(HIT.name).click()
    button(HIT.path).click()
    await flush()
    expect(writeText.mock.calls).toEqual([[HIT.name], [HIT.path]])
  })

  it('有 onShare 时显示“分享”，点击调用它；没有时不显示', async () => {
    mockCore({ file_details: () => INFO })
    const onShare = vi.fn()
    const share = () => document.querySelector<HTMLButtonElement>('button[aria-label="分享"]')
    const { rerender } = render(panel(true))
    await flush()
    expect(share()).toBeNull()

    rerender(
      <DetailPanel hit={HIT} query="" overlay={false} viewportHeight={800} connected onHide={vi.fn()} onOpen={vi.fn()} onSimilar={vi.fn()} onShare={onShare} />,
    )
    await flush()
    share()!.click()
    expect(onShare).toHaveBeenCalledTimes(1)
  })
})
