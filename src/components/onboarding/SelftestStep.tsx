import { useState, type ReactNode } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { DeviceResult, DeviceStatus, SelftestStatus } from '../../types'
import { errorMessage } from '../../lib/errors'
import { deviceName, SELFTEST_TEXT, switchedDevice, switchedText } from '../../lib/device'
import { AudioIcon, BanIcon, CheckCircleIcon, ClockIcon, CpuIcon, DocumentIcon, GpuIcon, ImageIcon, MinusCircleIcon, SpinnerIcon, VideoIcon, XCircleIcon } from '../icons'
import { renderMessage } from '../ConfirmDialog'
import Select from '../Select'
import Tip, { HoverTip } from '../Tip'
import { ROW_DESC, ROW_LABEL } from '../settings/styles'
import { CARD, CARD_ROW, ERROR_TEXT, ICON_TILE, SMALL_BUTTON } from './styles'

function ms(v: number) {
  return v >= 1000 ? `${(v / 1000).toFixed(1)} s` : `${Math.round(v)} ms`
}

const STATUS: Record<DeviceStatus, { label: string; icon: ReactNode }> = {
  pending: { label: '等待检测', icon: <ClockIcon className="h-4 w-4 text-gray-400 dark:text-gray-500" /> },
  testing: { label: '正在检测', icon: <SpinnerIcon className="h-4 w-4 animate-spin text-blue-500" /> },
  passed: { label: '通过', icon: <CheckCircleIcon className="h-4 w-4 text-green-600 dark:text-green-400" /> },
  failed: { label: '未通过', icon: <XCircleIcon className="h-4 w-4 text-red-500 dark:text-red-400" /> },
  blacklisted: { label: '已屏蔽', icon: <BanIcon className="h-4 w-4 text-red-500 dark:text-red-400" /> },
  skipped: { label: '未检测', icon: <MinusCircleIcon className="h-4 w-4 text-gray-400 dark:text-gray-500" /> },
}

/** 一种输入的单条耗时；没有结果时留空，仍占住这一列 */
function Timing({ label, icon, value }: { label: string; icon: ReactNode; value: number | null }) {
  if (value === null) return <span />
  return (
    <Tip label={label} className="gap-1 text-xs text-gray-500 tabular-nums dark:text-gray-400">
      {icon}
      {ms(value)}
    </Tip>
  )
}

/**
 * 每个设备的检测结果；引导与设置页共用。各行是卡片网格的子网格（subgrid），耗时列按所有行中最宽的内容取宽度，
 * 列间靠得近，上下行仍对齐。首尾两列用 auto：子网格的左右内边距会加在边上的列里，固定列宽装不下，图标会压到相邻的列上
 */
export function DeviceList({ devices }: { devices: DeviceResult[] }) {
  return (
    <div className={`${CARD} grid grid-cols-[auto_minmax(0,1fr)_auto_auto_auto_auto_auto] gap-x-3`}>
      {devices.map((d, i) => (
        <div key={i} className="col-span-full grid grid-cols-subgrid items-center px-4 py-2.5">
          <span className={ICON_TILE}>{d.gpu ? <GpuIcon className="h-4 w-4" /> : <CpuIcon className="h-4 w-4" />}</span>
          <span className="truncate text-sm text-gray-800 dark:text-gray-100">
            {deviceName(d, devices)}
            <HoverTip label={deviceName(d, devices)} wrap />
          </span>
          <Timing label="处理一段文本的用时" icon={<DocumentIcon className="h-3.5 w-3.5" />} value={d.textMs} />
          <Timing label="处理一张图片的用时" icon={<ImageIcon className="h-3.5 w-3.5" />} value={d.imageMs} />
          <Timing label="处理一段音频的用时" icon={<AudioIcon className="h-3.5 w-3.5" />} value={d.audioMs} />
          <Timing label="处理一个视频片段的用时" icon={<VideoIcon className="h-3.5 w-3.5" />} value={d.videoMs} />
          <Tip label={STATUS[d.status].label} className="w-full justify-end">{STATUS[d.status].icon}</Tip>
          {d.reason && <p data-selectable-text className="col-[2/-1] mt-1 whitespace-pre-line text-xs text-gray-500 dark:text-gray-400">{d.reason}</p>}
        </div>
      ))}
    </div>
  )
}

/**
 * 选择使用的设备；引导与设置页共用。选项为自动、最近一次检测通过的每块显卡、CPU，目前不能用的显卡由后台拒绝并说明原因。
 * 选了的显卡不能用时核心实际按自动选择，菜单也显示“自动”
 */
export function DevicePicker({ st }: { st: SelftestStatus }) {
  // 选择设备失败的原因，显示在下拉菜单下方
  const [pickError, setPickError] = useState('')
  const options = [
    { label: '自动', value: 'auto' },
    ...st.devices.filter((d) => d.gpu && d.status === 'passed').map((d) => ({ label: deviceName(d, st.devices), value: d.id })),
    { label: 'CPU', value: 'cpu' },
  ]
  const current = !st.preferredError && options.some((o) => o.value === st.preferred) ? st.preferred : 'auto'

  return (
    <div>
      <div className="mb-1 flex items-center justify-between gap-3">
        <span className={ROW_LABEL}>选择设备</span>
        <div className="min-w-0 max-w-[60%]">
          <Select
            label="选择设备"
            value={current}
            disabled={st.running}
            options={options}
            onChange={(device) => {
              // 所选显卡不能用时显示为“自动”，再选“自动”也要发出，把偏好改回自动
              if (device === current && device === st.preferred) return
              setPickError('')
              invoke('set_preferred_device', { device }).catch((err) => {
                console.warn('set_preferred_device 失败：', err)
                setPickError(errorMessage(err, '操作失败，请重试。'))
              })
            }}
          />
        </div>
      </div>
      <div className={ROW_DESC}>“自动”按检测结果选用速度最快的设备。只能选择通过检测的显卡，它之后不能使用时改为自动选择。</div>
      {pickError && <div data-selectable-text className={`mt-0.5 ${ERROR_TEXT}`}>{pickError}</div>}
    </div>
  )
}

/**
 * 第 4 步：设备自检。允许跳过、只用 CPU；
 * 检测在核心进程的推理子进程中进行，进度与每个设备的结果经事件推来，完成后索引立即改用选出的设备。
 * 检测结果列表下方是一行卡片：将使用的设备与说明，右侧是“开始检测 / 重新检测”小按钮；还没检测时再给“只用 CPU”。
 * 检测完成后卡片下方是与设置页相同的设备下拉菜单，可选自动、CPU 或检测通过的显卡，卡片随后台返回的状态更新。结果过期时不拦截，有待建立索引的文件且未暂停时后台服务自动重新检测（期间用 CPU）。页面上的主按钮只有引导底部的“下一步”
 */
export default function SelftestStep({ selftest, model }: { selftest: SelftestStatus; model: boolean }) {
  const [error, setError] = useState('')
  const st = selftest
  const selected = st.selected !== null ? st.devices[st.selected] : undefined
  const cpuOnly = st.preferred === 'cpu'
  const switched = switchedDevice(st)
  const tested = st.devices.length > 0
  // 有检测结果后用下拉菜单选择设备；还没检测时“只用 CPU”是另一条出路
  const offerCpu = !st.running && !cpuOnly && !tested

  const call = (cmd: 'run_selftest' | 'set_preferred_device') => {
    setError('')
    // 引导中先选了只用 CPU、又开始检测时，核心改回自动选择
    invoke(cmd, cmd === 'set_preferred_device' ? { device: 'cpu' } : { onboarding: true }).catch((err) => {
      console.warn(`${cmd} 失败：`, err)
      setError(errorMessage(err, '操作失败，请重试。'))
    })
  }

  return (
    <div>
      <h2 className="text-base font-bold text-gray-800 dark:text-gray-100">检测设备</h2>
      <p className="mt-1 text-sm text-gray-500 dark:text-gray-400">{SELFTEST_TEXT}</p>

      {tested && (
        <div className="mt-4">
          <DeviceList devices={st.devices} />
        </div>
      )}

      <div className={`mt-2 ${CARD}`}>
        <div className={CARD_ROW}>
          <span className={tested || cpuOnly ? 'flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-blue-500 text-white' : ICON_TILE}>
            {selected?.gpu && !cpuOnly ? <GpuIcon className="h-4 w-4" /> : <CpuIcon className="h-4 w-4" />}
          </span>
          <span className="min-w-0 flex-1">
            <span className="block truncate text-sm text-gray-800 dark:text-gray-100">
              {st.running
                ? '正在检测'
                : cpuOnly
                  ? '将使用「CPU」'
                  : selected
                    ? `将使用「${deviceName(selected, st.devices)}」`
                    : tested
                      ? '将使用「CPU」'
                      : '尚未检测'}
            </span>
            {!st.running && (
              <span data-selectable-text className="mt-0.5 block text-xs text-gray-500 dark:text-gray-400">
                {cpuOnly
                  ? tested
                    ? '已选择使用 CPU'
                    : renderMessage('已选择只用 CPU，之后可在「设置 → 设备」中重新检测')
                  : st.error
                    ? <span className="text-red-500 dark:text-red-400">{renderMessage(st.error, 'font-semibold')}</span>
                    : st.stale
                      ? '显卡、模型或所选的文件类型有变化，需要重新检测。也可以直接继续，建立索引时会在后台检测。'
                      : switched
                        ? <span className="text-amber-600 dark:text-amber-400">{switchedText(switched)}</span>
                        : st.preferredError
                          ? <span className="text-amber-600 dark:text-amber-400">{st.preferredError}</span>
                          : selected?.id === st.preferred
                            ? '已选择使用这块显卡'
                            : tested && !selected
                              ? '没有通过检测的设备，将尝试使用 CPU'
                              : !tested
                                ? renderMessage('也可以跳过检测，只用 CPU，之后可在「设置 → 设备」中检测')
                                : '检测通过的设备中速度最快'}
              </span>
            )}
          </span>
          {!st.running && (
            <span className="flex flex-shrink-0 gap-1.5">
              {offerCpu && (
                <button type="button" className={SMALL_BUTTON} onClick={() => call('set_preferred_device')}>
                  只用 CPU
                </button>
              )}
              <button type="button" className={SMALL_BUTTON} disabled={!model} onClick={() => call('run_selftest')}>
                {tested ? '重新检测' : '开始检测'}
              </button>
            </span>
          )}
        </div>
      </div>
      {error && <p data-selectable-text className={`mt-2 ${ERROR_TEXT}`}>{error}</p>}
      {!st.running && tested && (
        <div className="mt-4">
          <DevicePicker st={st} />
        </div>
      )}
    </div>
  )
}
