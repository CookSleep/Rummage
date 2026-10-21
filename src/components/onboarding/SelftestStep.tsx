import { useState, type ReactNode } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { DeviceResult, DeviceStatus, ParamValues, SelftestStatus } from '../../types'
import { errorMessage } from '../../lib/errors'
import { isAndroid } from '../../lib/platform'
import { deviceName, SPEED_STALE_TEXT, speedStale, switchedDevice, switchedText } from '../../lib/device'
import { useParamValues } from '../../hooks/useParamValues'
import { AudioIcon, BanIcon, CheckCircleIcon, ClockIcon, CpuIcon, DocumentIcon, GpuIcon, ImageIcon, MinusCircleIcon, NpuIcon, SpinnerIcon, VideoIcon, XCircleIcon } from '../icons'
import ConfirmDialog, { renderMessage } from '../ConfirmDialog'
import Select from '../Select'
import Tip, { HoverTip } from '../Tip'
import { ROW_DESC, ROW_LABEL } from '../settings/styles'
import { NPU_DEVICE } from './ModelDownload'
import SpeedParamsDialog from './SpeedParamsDialog'
import { CARD, CARD_ROW, ERROR_TEXT, ICON_TILE, ROW_ACTIONS, ROW_ICON_TILE, ROW_LONG_TEXT, SMALL_BUTTON } from './styles'

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

/**
 * 一种输入的单个文件（单段）耗时：有测速结果时用它（`speed`，`what` 如“每张图片”），否则用黄金样本的耗时（`sample`）；
 * 没有结果时留空，仍占住这一列。`stale` 时测速用的参数已改过
 */
function Timing({ what, icon, speed, sample, stale }: { what: string; icon: ReactNode; speed: number | null | undefined; sample: number | null; stale: boolean }) {
  const value = speed ?? sample
  if (value === null) return <span />
  const label = speed == null ? `检测样本中${what}的用时` : `按${stale ? '修改前的' : '当前'}设置，${what}约 ${ms(speed)}`
  return (
    <Tip label={label} className="gap-1 whitespace-nowrap text-xs text-gray-500 tabular-nums dark:text-gray-400">
      {icon}
      {ms(value)}
    </Tip>
  )
}

/** 设备的图标：CPU、显卡或 NPU（Android 的 `gpu:NPU|`） */
export function DeviceIcon({ id, gpu, className = 'h-4 w-4' }: { id?: string; gpu: boolean; className?: string }) {
  if (!gpu) return <CpuIcon className={className} />
  return id === NPU_DEVICE ? <NpuIcon className={className} /> : <GpuIcon className={className} />
}

/**
 * 每个设备的检测结果；引导与设置页共用。各行是卡片网格的子网格（subgrid），耗时列按所有行中最宽的内容取宽度，
 * 列间靠得近，上下行仍对齐。首尾两列用 auto：子网格的左右内边距会加在边上的列里，固定列宽装不下，图标会压到相邻的列上。
 * 耗时为按测速时的参数处理单个文件（单段）的用时，`values`（当前参数）与测速时不同时在上方提示。
 * 已屏蔽的设备也在这里，不在本次结果中的补在最后，显示为已屏蔽。按钮按状态：
 * - 未通过：有未通过记录，之后的检测不再测它；“重新检测”删除它的未通过记录并只重测它。
 * - 已屏蔽：出现过严重错误；“解除屏蔽”确认后删除它的全部屏蔽记录，后台服务随即只重测它。
 */
export function DeviceList({ st, values }: { st: SelftestStatus; values: ParamValues | null }) {
  // 没能当场重测的原因或出错，显示在列表下方
  const [note, setNote] = useState('')
  const [error, setError] = useState('')
  // 请求还没返回，之后由 `st.running` 隐藏按钮
  const [busy, setBusy] = useState(false)
  // 等待确认解除屏蔽的设备
  const [unblocking, setUnblocking] = useState<DeviceResult | null>(null)
  const blocked = (id: string) => st.blocked.find((b) => b.id === id)
  const rows: DeviceResult[] = [
    ...st.devices.map((d) => {
      // 驱动或运行库更新后的手动检测会重测已屏蔽的显卡，等待与检测中照常显示
      const b = blocked(d.id)
      return b && d.status !== 'pending' && d.status !== 'testing' ? { ...d, status: 'blacklisted' as const, reason: b.reason ?? d.reason } : d
    }),
    ...st.blocked
      .filter((b) => !st.devices.some((d) => d.id === b.id))
      .map((b) => ({ id: b.id, gpu: true, pciId: b.pciId, name: b.name, status: 'blacklisted' as const, reason: b.reason, textMs: null, imageMs: null, audioMs: null, videoMs: null, speed: null, state: null })),
  ]
  const stale = (d: DeviceResult) => speedStale(d, values)
  const clear = (d: DeviceResult) => {
    const failed = d.status === 'failed'
    setNote('')
    setError('')
    setBusy(true)
    invoke<string | null>('unblock_gpu', { device: d.id, failed })
      .then((n) => setNote(n ?? ''))
      .catch((err) => {
        console.warn('unblock_gpu 失败：', err)
        setError(errorMessage(err, failed ? '无法重新检测，请重试。' : '无法解除屏蔽，请重试。'))
      })
      .finally(() => {
        setBusy(false)
        setUnblocking(null)
      })
  }
  return (
    <div>
      {rows.some(stale) && <p className="mb-2 text-xs text-amber-600 dark:text-amber-400">{SPEED_STALE_TEXT}</p>}
      <div className={`${CARD} grid grid-cols-[auto_minmax(0,1fr)_auto_auto_auto_auto_auto] gap-x-3 max-sm:grid-cols-[auto_minmax(0,1fr)_auto]`}>
        {rows.map((d) => (
          <div key={d.id} className="col-span-full grid grid-cols-subgrid items-center px-4 py-2.5">
            <span className={ICON_TILE}>
              <DeviceIcon id={d.id} gpu={d.gpu} />
            </span>
            <span className="truncate text-sm text-gray-800 dark:text-gray-100 max-sm:whitespace-normal">
              {deviceName(d, rows)}
              <HoverTip label={deviceName(d, rows)} wrap />
            </span>
            {/* 手机宽度下耗时放到名称下面一行 */}
            <span className="contents max-sm:col-[2/-1] max-sm:row-start-2 max-sm:mt-1 max-sm:flex max-sm:flex-wrap max-sm:gap-x-3">
              <Timing what="每段文本" icon={<DocumentIcon className="h-3.5 w-3.5" />} speed={d.speed?.textMs} sample={d.textMs} stale={stale(d)} />
              <Timing what="每张图片" icon={<ImageIcon className="h-3.5 w-3.5" />} speed={d.speed?.imageMs} sample={d.imageMs} stale={stale(d)} />
              <Timing what="每段音频" icon={<AudioIcon className="h-3.5 w-3.5" />} speed={d.speed?.audioMs} sample={d.audioMs} stale={stale(d)} />
              <Timing what="每个视频片段" icon={<VideoIcon className="h-3.5 w-3.5" />} speed={d.speed?.videoMs} sample={d.videoMs} stale={stale(d)} />
            </span>
            <Tip label={STATUS[d.status].label} className="w-full justify-end max-sm:col-start-3 max-sm:row-start-1">{STATUS[d.status].icon}</Tip>
            {(d.reason || d.status === 'blacklisted') && (
              <p data-selectable-text className="col-[2/-1] mt-1 whitespace-pre-line text-xs text-gray-500 dark:text-gray-400">
                {d.reason ?? (isAndroid ? '这个设备出现过严重错误。' : '这块显卡出现过驱动错误。')}
              </p>
            )}
            {d.gpu && (d.status === 'failed' || d.status === 'blacklisted') && !st.running && (
              <span className="col-[2/-1] mt-1.5 flex justify-end">
                <button type="button" className={SMALL_BUTTON} disabled={busy} onClick={() => (d.status === 'failed' ? clear(d) : setUnblocking(d))}>
                  {d.status === 'failed' ? '重新检测' : '解除屏蔽'}
                </button>
              </span>
            )}
          </div>
        ))}
      </div>
      {note && !st.running && <p data-selectable-text className={`mt-2 ${ROW_DESC}`}>{note}</p>}
      {error && <p data-selectable-text className={`mt-2 ${ERROR_TEXT}`}>{error}</p>}
      {unblocking && (
        <ConfirmDialog
          title="解除屏蔽"
          message={`解除后会立即重新检测「${deviceName(unblocking, rows)}」，期间暂停建立索引。

如果问题仍然存在，检测可能再次失败，${isAndroid ? '它' : '这块显卡'}可能重新被屏蔽。`}
          confirmText="解除屏蔽"
          busy={busy ? '正在解除' : undefined}
          onConfirm={() => clear(unblocking)}
          onCancel={() => setUnblocking(null)}
        />
      )}
    </div>
  )
}

/**
 * 选择使用的设备；引导与设置页共用。选项为自动、最近一次检测通过的每块显卡、CPU，目前不能用的显卡由后台拒绝并说明原因。
 * 选了的显卡不能用时核心实际按自动选择，菜单也显示“自动”。`cpu` 为假时（Android 上只有 NPU 模型，CPU 要用通用模型）不列出 CPU
 */
export function DevicePicker({ st, cpu }: { st: SelftestStatus; cpu: boolean }) {
  // 选择设备失败的原因，显示在下拉菜单下方
  const [pickError, setPickError] = useState('')
  const options = [
    { label: '自动', value: 'auto' },
    ...st.devices.filter((d) => d.gpu && d.status === 'passed' && !st.blocked.some((b) => b.id === d.id)).map((d) => ({ label: deviceName(d, st.devices), value: d.id })),
    ...(cpu ? [{ label: 'CPU', value: 'cpu' }] : []),
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
      <div className={ROW_DESC}>“自动”按检测结果选用速度最快的设备。只能选择通过检测的{isAndroid ? ' GPU 或 NPU' : '显卡'}，它之后不能使用时改为自动选择。</div>
      {pickError && <div data-selectable-text className={`mt-0.5 ${ERROR_TEXT}`}>{pickError}</div>}
    </div>
  )
}

/**
 * 第 4 步：设备自检。允许跳过、只用 CPU（`cpu` 为假时没有这条出路，见 `DevicePicker`）；
 * 检测在核心进程的推理子进程中进行，进度与每个设备的结果经事件推来，完成后索引立即改用选出的设备。
 * 检测结果列表下方是一行卡片：将使用的设备与说明，右侧是“开始检测 / 重新检测”小按钮；还没检测时再给“只用 CPU”。
 * 检测完成后卡片下方是与设置页相同的设备下拉菜单，可选自动、CPU 或检测通过的显卡，卡片随后台返回的状态更新。结果过期时不拦截，有待建立索引的文件且未暂停时后台服务自动重新检测（期间用 CPU）。页面上的主按钮只有引导底部的“下一步”。
 * 卡片上的“高级”打开只列出影响速度的参数的弹窗（`SpeedParamsDialog`）；改了测速用到的参数时检测结果上方提示耗时是修改前的，不自动重新检测。
 * 未通过的设备可在结果中重新检测，已屏蔽的设备也在结果中，可以解除屏蔽，与设置页相同（`DeviceList`）
 */
export default function SelftestStep({ selftest, model, cpu, text }: { selftest: SelftestStatus; model: boolean; cpu: boolean; text: string }) {
  const [error, setError] = useState('')
  const [advanced, setAdvanced] = useState(false)
  const values = useParamValues(true)
  const st = selftest
  const selected = st.selected !== null ? st.devices[st.selected] : undefined
  const cpuOnly = st.preferred === 'cpu'
  const switched = switchedDevice(st)
  const tested = st.devices.length > 0
  // 有检测结果后用下拉菜单选择设备；还没检测时“只用 CPU”是另一条出路
  const offerCpu = cpu && !st.running && !cpuOnly && !tested

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
      <p className="mt-1 text-sm text-gray-500 dark:text-gray-400">{text}</p>

      {/* 还没检测过时也列出已屏蔽的设备，以便解除屏蔽 */}
      {(tested || st.blocked.length > 0) && (
        <div className="mt-4">
          <DeviceList st={st} values={values} />
        </div>
      )}

      <div className={`mt-2 ${CARD}`}>
        <div className={CARD_ROW}>
          <span className={`${ROW_ICON_TILE} ${tested || cpuOnly ? 'flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-blue-500 text-white' : ICON_TILE}`}>
            <DeviceIcon id={selected?.id} gpu={Boolean(selected?.gpu) && !cpuOnly} />
          </span>
          <span className="min-w-0 flex-1">
            <span className={`block text-sm text-gray-800 dark:text-gray-100 ${ROW_LONG_TEXT}`}>
              {st.running
                ? '正在检测'
                : cpuOnly
                  ? '将使用「CPU」'
                  : selected
                    ? `将使用「${deviceName(selected, st.devices)}」`
                    : tested
                      ? cpu ? '将使用「CPU」' : '没有可用的设备'
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
                      ? `${isAndroid ? '' : '显卡、'}模型或所选的文件类型有变化，需要重新检测。也可以直接继续，建立索引时会在后台检测。`
                      : switched
                        ? <span className="text-amber-600 dark:text-amber-400">{switchedText(switched)}</span>
                        : st.preferredError
                          ? <span className="text-amber-600 dark:text-amber-400">{st.preferredError}</span>
                          : selected?.id === st.preferred
                            ? isAndroid
                              ? '已选择使用这个设备'
                              : '已选择使用这块显卡'
                            : tested && !selected
                              ? cpu ? '没有通过检测的设备，将尝试使用 CPU' : '没有通过检测的设备，CPU 和 GPU 要用通用模型'
                              : !tested
                                ? cpu ? renderMessage('也可以跳过检测，只用 CPU，之后可在「设置 → 设备」中检测') : '检测后选用通过检测的设备'
                                : '检测通过的设备中速度最快'}
              </span>
            )}
          </span>
          {!st.running && (
            <span className={`gap-1.5 ${ROW_ACTIONS}`}>
              {offerCpu && (
                <button type="button" className={SMALL_BUTTON} onClick={() => call('set_preferred_device')}>
                  只用 CPU
                </button>
              )}
              <button type="button" className={SMALL_BUTTON} disabled={!values} onClick={() => setAdvanced(true)}>
                高级
              </button>
              <button type="button" className={SMALL_BUTTON} disabled={!model} onClick={() => call('run_selftest')}>
                {tested ? '重新检测' : '开始检测'}
              </button>
            </span>
          )}
        </div>
      </div>
      {error && <p data-selectable-text className={`mt-2 ${ERROR_TEXT}`}>{error}</p>}
      {advanced && values && <SpeedParamsDialog values={values} onClose={() => setAdvanced(false)} />}
      {!st.running && tested && (
        <div className="mt-4">
          <DevicePicker st={st} cpu={cpu} />
        </div>
      )}
    </div>
  )
}
