// @vitest-environment jsdom

import { act } from 'react'
import { describe, expect, it } from 'vitest'

import { render } from '../test/tauri'
import Toaster, { showToast } from './Toast'

const toasts = () => [...document.querySelectorAll('[role="status"]')].map((el) => el.textContent)

describe('Toaster', () => {
  it('一句话的消息去掉句末句号', () => {
    render(<Toaster />)
    act(() => showToast('已导出诊断信息。', 'success'))
    expect(toasts()).toEqual(['已导出诊断信息'])
  })

  it('多句的消息保留句号', () => {
    render(<Toaster />)
    act(() => showToast('校验未通过。请重新下载模型。', 'error'))
    expect(toasts()).toEqual(['校验未通过。请重新下载模型。'])
  })

  it('同一时间只显示一条，新的替换旧的', () => {
    render(<Toaster />)
    act(() => showToast('第一条'))
    act(() => showToast('第二条'))
    expect(toasts()).toEqual(['第二条'])
  })
})
