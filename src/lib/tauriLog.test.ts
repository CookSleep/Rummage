// @vitest-environment jsdom

import { describe, expect, it, vi } from 'vitest'

const listeners: ((record: { level: number; message: string }) => void)[] = []

vi.mock('@tauri-apps/plugin-log', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@tauri-apps/plugin-log')>()),
  attachLogger: (fn: (typeof listeners)[number]) => {
    listeners.push(fn)
    return Promise.resolve(() => {})
  },
}))

describe('installTauriLog', () => {
  it('Rust 侧日志按级别用对应的 console 方法打印', async () => {
    const { LogLevel } = await import('@tauri-apps/plugin-log')
    const info = vi.spyOn(console, 'info').mockImplementation(() => {})
    const debug = vi.spyOn(console, 'debug').mockImplementation(() => {})
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const { installTauriLog } = await import('./tauriLog')
    installTauriLog()
    listeners[0]({ level: LogLevel.Info, message: '已启动' })
    listeners[0]({ level: LogLevel.Debug, message: '细节' })
    expect(info).toHaveBeenCalledWith('已启动')
    expect(debug).toHaveBeenCalledWith('细节')
    expect(warn).not.toHaveBeenCalled()
    listeners[0]({ level: LogLevel.Warn, message: '警告' })
    expect(warn).toHaveBeenCalledWith('警告')
  })
})
