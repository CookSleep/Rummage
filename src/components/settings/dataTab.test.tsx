// @vitest-environment jsdom

import { describe, expect, it } from 'vitest'
import type { ConfigImport, ParamDef } from '../../types'

import { button, click, dialog, flush, mockCore, render, section, text, toast, type Handler } from '../../test/tauri'
import Toaster from '../Toast'
import DataTab from './DataTab'

function def(key: string, affectsIndex: boolean): ParamDef {
  return { key, group: 'Index', section: '文档', kind: { Int: { min: 128, max: 512 } }, default: 384, affectsIndex, indexKinds: affectsIndex ? ['document'] : [], label: key, help: '', basis: '', platform: '' }
}

const DEFS = [def('chunk_tokens', true), def('max_doc_mb', false)]

function result(over: Partial<ConfigImport> = {}): ConfigImport {
  return { added: 1, missing: 0, inaccessible: 0, failed: 0, droppedParams: 0, warning: null, ...over }
}

async function open(handlers: Record<string, Handler>) {
  const calls = mockCore({
    storage_info: () => ({ dataDir: 'D:\\Rummage\\data', db: 1, thumbs: 1, models: 1, logs: 1, other: 1 }),
    ...handlers,
  })
  render(
    <>
      <Toaster />
      <DataTab defs={DEFS} offline={false} coreStatus="running" />
    </>,
  )
  await flush()
  return calls
}

describe('DataTab 配置', () => {
  it('显示说明与两个按钮', async () => {
    await open({})
    const el = section('配置')
    expect(text(el)).toContain('导出文件夹、排除规则和参数，可在另一台同类设备上导入。')
    button('导出配置', el)
    button('导入配置', el)
  })

  it('导出成功时提示“配置已导出”，取消时不提示', async () => {
    let saved = false
    const calls = await open({ export_config: () => saved })
    await click(button('导出配置'))
    expect(calls.filter((c) => c.cmd === 'export_config')).toHaveLength(1)
    expect(toast()).toBe('')

    saved = true
    await click(button('导出配置'))
    expect(toast()).toBe('配置已导出')
  })

  it('导出失败时显示原因', async () => {
    await open({
      export_config: () => {
        throw { code: 'internal', message: '配置未能保存，请确认所选位置可以写入后重试。' }
      },
    })
    await click(button('导出配置'))
    expect(toast()).toBe('配置未能保存，请确认所选位置可以写入后重试')
  })

  it('没有丢弃时提示“配置已导入”', async () => {
    const calls = await open({ import_config: () => ({ params: { max_doc_mb: 100 } }), apply_config: () => result() })
    await click(button('导入配置'))
    // 不影响索引的参数不确认
    expect(calls.some((c) => c.cmd === 'apply_config')).toBe(true)
    expect(toast()).toBe('配置已导入')
  })

  it('有丢弃时按实际情况拼出提示', async () => {
    await open({ import_config: () => ({ params: {} }), apply_config: () => result({ missing: 2, droppedParams: 3 }) })
    await click(button('导入配置'))
    expect(toast()).toBe('配置已导入，2 个文件夹不存在已跳过，3 项参数无效已跳过')
  })

  it('取消选择文件时什么都不做', async () => {
    const calls = await open({ import_config: () => null })
    await click(button('导入配置'))
    expect(calls.some((c) => c.cmd === 'apply_config')).toBe(false)
    expect(toast()).toBe('')
    expect(button('导入配置').hasAttribute('disabled')).toBe(false)
  })

  it('会改变影响索引的参数时先确认，取消后不导入', async () => {
    const calls = await open({ import_config: () => ({ params: { chunk_tokens: 256 } }), apply_config: () => result() })
    await click(button('导入配置'))
    expect(text(dialog())).toContain('修改后需要重建受影响的索引（文档）。')
    expect(calls.some((c) => c.cmd === 'apply_config')).toBe(false)

    await click(button('取消', dialog()))
    expect(calls.some((c) => c.cmd === 'apply_config')).toBe(false)

    await click(button('导入配置'))
    await click(button('导入并重建', dialog()))
    expect(calls.filter((c) => c.cmd === 'apply_config')).toHaveLength(1)
    expect(toast()).toBe('配置已导入')
  })

  it('配置不能导入时显示原因', async () => {
    await open({
      import_config: () => {
        throw { code: 'bad_request', message: '这个配置文件来自更新版本的 Rummage，请先更新 Rummage 再导入。' }
      },
    })
    await click(button('导入配置'))
    expect(toast()).toBe('这个配置文件来自更新版本的 Rummage，请先更新 Rummage 再导入')
  })
})
