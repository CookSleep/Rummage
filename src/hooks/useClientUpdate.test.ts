import { beforeEach, describe, expect, it, vi } from 'vitest'

const installUpdate = vi.hoisted(() => vi.fn(async () => {}))
const downloadUpdate = vi.hoisted(() => vi.fn(async (..._args: unknown[]) => {}))
const getReadyUpdate = vi.hoisted(() => vi.fn(async (): Promise<string | null> => null))
const showToast = vi.hoisted(() => vi.fn())
const setup = vi.hoisted(() => ({ task: null as { kind: string; state: string } | null, selftest: { running: false } }))

vi.mock('../lib/clientUpdate', () => ({ downloadUpdate, getReadyUpdate, installUpdate }))
vi.mock('../components/Toast', () => ({ showToast }))
vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === 'setup_status') return setup
    throw new Error(`未模拟的命令 ${cmd}`)
  }),
}))

const { requestClientUpdateInstall, startClientUpdateDownload, startClientUpdateInstall, useClientUpdate } = await import('./useClientUpdate')

describe('应用内更新的安装', () => {
  beforeEach(() => {
    installUpdate.mockClear()
    downloadUpdate.mockClear()
    showToast.mockClear()
    setup.task = null
    setup.selftest.running = false
    useClientUpdate.setState({ status: 'ready', readyVersion: '1.2.0', confirm: null })
  })

  it('没有进行中的操作且已下载最新版本时直接安装', async () => {
    await requestClientUpdateInstall('1.2.0')
    await vi.waitFor(() => expect(installUpdate).toHaveBeenCalled())
    expect(useClientUpdate.getState().confirm).toBeNull()
  })

  it('后台服务正在下载模型或检测设备时先确认，确认后才安装', async () => {
    setup.task = { kind: 'download', state: 'running' }
    await requestClientUpdateInstall('1.2.0')
    const confirm = useClientUpdate.getState().confirm
    expect(confirm?.title).toBe('安装更新')
    expect(confirm?.message).toContain('中断')
    expect(useClientUpdate.getState().status).toBe('ready')
    expect(installUpdate).not.toHaveBeenCalled()
    confirm?.onConfirm()
    expect(useClientUpdate.getState().confirm).toBeNull()
    await vi.waitFor(() => expect(installUpdate).toHaveBeenCalled())

    setup.task = null
    setup.selftest.running = true
    useClientUpdate.setState({ status: 'ready', confirm: null })
    await requestClientUpdateInstall('1.2.0')
    expect(useClientUpdate.getState().confirm?.title).toBe('安装更新')
  })

  it('查询后台服务期间重复点击“安装”只安装一次', async () => {
    await Promise.all([requestClientUpdateInstall('1.2.0'), requestClientUpdateInstall('1.2.0')])
    expect(installUpdate).toHaveBeenCalledTimes(1)
    expect(showToast).not.toHaveBeenCalled()
  })

  it('校验或删除模型时不需要确认', async () => {
    setup.task = { kind: 'verify', state: 'running' }
    await requestClientUpdateInstall('1.2.0')
    expect(useClientUpdate.getState().confirm).toBeNull()
    expect(installUpdate).toHaveBeenCalled()
  })

  it('已下载的版本过旧时，取消保留安装包，也可改为下载新版本', async () => {
    await requestClientUpdateInstall('1.3.0')
    const confirm = useClientUpdate.getState().confirm
    expect(confirm?.confirmText).toBe('继续安装')
    expect(confirm?.extra?.label).toBe('下载新版本')
    expect(confirm?.danger).toBe(false)
    expect(useClientUpdate.getState()).toMatchObject({ status: 'ready', readyVersion: '1.2.0' })
    confirm?.extra?.onClick()
    expect(useClientUpdate.getState().confirm).toBeNull()
    expect(downloadUpdate).toHaveBeenCalledWith('1.3.0', expect.any(Function))
    expect(installUpdate).not.toHaveBeenCalled()
  })

  it('已下载的版本过旧且有进行中的操作时，提醒会中断并以警示色显示“继续安装”', async () => {
    setup.task = { kind: 'download', state: 'running' }
    await requestClientUpdateInstall('1.3.0')
    const confirm = useClientUpdate.getState().confirm
    expect(confirm?.message.split('\n\n')).toEqual(['最新版本为 v1.3.0，已下载的是 v1.2.0。', '后台服务正在下载、导入模型或检测设备。', '安装更新会关闭 Rummage 和后台服务，并中断这些操作。'])
    expect(confirm?.danger).toBe(true)
  })

  it('下载失败后用错误提示显示原因，并按磁盘上的实际情况显示安装包状态', async () => {
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    downloadUpdate.mockRejectedValueOnce({ code: 'network', message: '网络连接失败，请检查网络和代理设置后重试。' })
    getReadyUpdate.mockResolvedValueOnce('1.2.0')
    await startClientUpdateDownload('1.3.0')
    expect(useClientUpdate.getState()).toMatchObject({ status: 'ready', readyVersion: '1.2.0' })
    expect(showToast).toHaveBeenCalledWith('网络连接失败，请检查网络和代理设置后重试。', 'error')
  })

  it('安装失败后用错误提示显示原因，按钮恢复为可重试', async () => {
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    installUpdate.mockRejectedValueOnce({ code: 'update', message: '安装程序文件被占用，请稍后重试。' })
    getReadyUpdate.mockResolvedValueOnce('1.2.0')
    await startClientUpdateInstall()
    expect(useClientUpdate.getState().status).toBe('ready')
    expect(showToast).toHaveBeenCalledWith('安装程序文件被占用，请稍后重试。', 'error')
  })
})
