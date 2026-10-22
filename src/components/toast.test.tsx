// @vitest-environment jsdom

import { act } from 'react'
import { describe, expect, it } from 'vitest'

import { button, dialog, render, text } from '../test/tauri'
import ConfirmDialog from './ConfirmDialog'
import Toaster, { showToast } from './Toast'

const toasts = () => [...document.querySelectorAll('[role="status"]')].map((el) => el.textContent)

describe('Toaster', () => {
  it('一句话的消息去掉句末句号', () => {
    render(<Toaster />)
    act(() => showToast('已导出诊断信息。', 'success'))
    expect(toasts()).toEqual(['已导出诊断信息'])
  })

  it('多句的消息改用弹窗，保留句号，点“知道了”关闭', () => {
    render(<Toaster />)
    act(() => showToast('校验未通过。请重新下载模型。', 'error'))
    expect(toasts()).toEqual([])
    expect(text(dialog())).toBe('操作失败校验未通过。请重新下载模型。知道了')
    act(() => button('知道了').click())
    expect(document.querySelector('[role="dialog"]')).toBeNull()
  })

  it('过长或多行的消息改用弹窗，路径的英文引号换成「」，每行一段', () => {
    render(<Toaster />)
    act(() => showToast('无法读取 "D:\\资料\\很长很长的文件夹名\\更深一层的文件夹"，请确认这个文件夹仍然存在并且有权限读取'))
    expect(toasts()).toEqual([])
    expect(text(dialog())).toContain('「D:\\资料\\很长很长的文件夹名\\更深一层的文件夹」')
    act(() => button('知道了').click())

    act(() => showToast('配置已导入\n设置已保存', 'info'))
    expect(dialog().querySelectorAll('p')).toHaveLength(2)
  })

  it('多条过长的消息依次显示，关闭一个再显示下一个', () => {
    render(<Toaster />)
    act(() => showToast('第一条。还有一句。', 'error'))
    act(() => showToast('第二条。还有一句。', 'error'))
    expect(text(dialog())).toContain('第一条')
    act(() => button('知道了').click())
    expect(text(dialog())).toContain('第二条')
    act(() => button('知道了').click())
    expect(document.querySelector('[role="dialog"]')).toBeNull()
  })

  it('Esc 只关闭最上层的弹窗', () => {
    let lowerClosed = false
    render(
      <>
        <ConfirmDialog title="下层" message="下层" confirmText="确定" onConfirm={() => {}} onCancel={() => (lowerClosed = true)} />
        <Toaster />
      </>,
    )
    act(() => showToast('校验未通过。请重新下载模型。', 'error'))
    act(() => {
      window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))
    })
    expect(text()).not.toContain('校验未通过')
    expect(lowerClosed).toBe(false)
  })

  it('弹窗与胶囊通知互不替换', () => {
    render(<Toaster />)
    act(() => showToast('校验未通过。请重新下载模型。', 'error'))
    act(() => showToast('已复制路径'))
    expect(toasts()).toEqual(['已复制路径'])
    expect(text(dialog())).toContain('校验未通过')
  })

  it('同一时间只显示一条，新的替换旧的', () => {
    render(<Toaster />)
    act(() => showToast('第一条'))
    act(() => showToast('第二条'))
    expect(toasts()).toEqual(['第二条'])
  })
})
