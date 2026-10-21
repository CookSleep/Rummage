import { useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { ParamValues, SelftestStatus, SetupStatus } from '../../types'
import { errorMessage } from '../../lib/errors'
import { isAndroid } from '../../lib/platform'
import { CPU_FALLBACK_TEXT, cpuFallbackReason, cpuFaster, cpuName, deviceName, selftestText, switchedDevice, switchedText } from '../../lib/device'
import { ResetIcon, WarningIcon } from '../icons'
import { renderMessage } from '../ConfirmDialog'
import { HoverTip } from '../Tip'
import { anyModelReady } from '../onboarding/ModelDownload'
import { DeviceIcon, DeviceList, DevicePicker } from '../onboarding/SelftestStep'
import { CARD, CARD_ROW, ERROR_TEXT } from '../onboarding/styles'
import { HEADER_BUTTON, NOTICE_BOX, ROW_DESC, SECTION_TITLE } from './styles'

/** 实际生效的设备及原因：与核心选择后端的规则一致，选了 CPU、没有检测过或选中的显卡不能用时都用 CPU */
function effective(st: SelftestStatus): { id?: string; gpu: boolean; name: string; reason: string } {
  const cpu = (reason: string) => ({ gpu: false, name: cpuName(st), reason })
  if (st.preferred === 'cpu') return cpu('已选择使用 CPU')
  if (!st.devices.length) return cpu('还没有检测过设备')
  const d = st.selected !== null ? st.devices[st.selected] : undefined
  if (!d) return cpu('没有通过检测的设备，将尝试使用 CPU')
  // 设备本次运行中出了问题时，核心已把选中项改为改用的设备；按它现在的情况说明之后能否再用
  const s = switchedDevice(st)
  if (s) {
    const state = st.devices.find((x) => x.id === st.failed)?.state
    const reason = st.blocked.some((b) => b.id === st.failed)
      ? `「${s.from}」已屏蔽，可以在检测结果中解除屏蔽`
      : state === 'absent'
        ? `「${s.from}」不在系统中，重新接入后需重新启动 Rummage 才会使用它`
        : `本次运行中不再使用「${s.from}」，重新启动 Rummage 后会照常使用它`
    return { id: d.id, gpu: d.gpu, name: s.to, reason }
  }
  // 显卡出现可能与驱动有关的问题（如列出显卡时卡住）：本次运行所有显卡都停用
  if (st.gpusOff) return cpu(isAndroid ? '本次运行中不再使用 GPU 与 NPU，重新启动 Rummage 后会重新检测' : '本次运行中不再使用显卡，重新启动 Rummage 后会重新检测')
  if (st.preferred !== 'auto' && !st.preferredError) return { id: d.id, gpu: d.gpu, name: deviceName(d, st.devices), reason: isAndroid ? '已选择使用这个设备' : '已选择使用这块显卡' }
  if (d.gpu) return { id: d.id, gpu: true, name: deviceName(d, st.devices), reason: '检测通过的设备中速度最快' }
  if (cpuFaster(st)) return cpu(isAndroid ? '检测后选用了 CPU，它比 GPU 与 NPU 快' : '检测后选用了 CPU，它比显卡快')
  if (st.devices.some((x) => x.gpu)) return cpu(isAndroid ? '没有更快的 GPU 或 NPU 通过检测' : '没有更快的显卡通过检测')
  if (isAndroid) return cpu(st.blocked.length ? 'GPU 与 NPU 已屏蔽，可以在检测结果中解除屏蔽' : '没有找到可用的 GPU 或 NPU')
  return cpu(st.blocked.length ? '显卡已屏蔽，可以在检测结果中解除屏蔽' : '没有找到可用的显卡')
}

/**
 * “设备”页：顶部选择使用的设备（自动、检测通过的显卡或 CPU），当前使用的设备与原因、每个候选的检测结果、重新检测。
 * 选了的显卡之后不能用时核心按自动选择，下拉菜单显示“自动”并说明原因。
 * 结果过期时后台服务自动重新检测（显示与手动检测相同）；检测未通过而用 CPU、或原本要用的设备出了问题而改用别的设备时提示，可手动重新检测；
 * 检测通过但 CPU 更快时只在当前设备下说明，不提示。
 * 检测结果中也列出被屏蔽的显卡，可以解除屏蔽；未通过的可以单独重新检测（`DeviceList`）；标题行的“重新检测”不清除屏蔽与未通过记录。
 * Android 上的设备为 CPU、GPU 与 NPU（LiteRT），规则相同
 */
export default function DeviceTab({ setup, values }: { setup: SetupStatus; values: ParamValues | null }) {
  const [error, setError] = useState('')
  const st = setup.selftest
  const cur = effective(st)
  // 黄色提示只说检测未通过、显卡已屏蔽与设备出了问题；正在检测、尚未检测看下方当前设备，结果过期多是自动检测出错或被中止，不提示
  const fallback = cpuFallbackReason(st)
  const switched = switchedDevice(st)
  // 下载、导入或删除会替换或删掉检测要用的模型文件（校验只读，不影响）
  const modelBusy = setup.task?.state === 'running' && setup.task.kind !== 'verify'
  const modelReady = isAndroid ? anyModelReady(setup) : setup.model.ready

  const call = (cmd: string, args?: Record<string, unknown>) => {
    setError('')
    invoke(cmd, args).catch((err) => {
      console.warn(`${cmd} 失败：`, err)
      setError(errorMessage(err, '操作失败，请重试。'))
    })
  }

  return (
    <div className="space-y-8">
      <section className="space-y-3">
        <h4 className={SECTION_TITLE}>使用的设备</h4>
        <DevicePicker st={st} cpu={!isAndroid || setup.model.ready} />
        {(fallback === 'failed' || fallback === 'blocked' || fallback === 'gpusOff') && (
          <div className={NOTICE_BOX}>
            <WarningIcon className="mt-px h-4 w-4 flex-shrink-0" />
            <span>{CPU_FALLBACK_TEXT[fallback]}，已改用 CPU 建立索引，速度较慢。</span>
          </div>
        )}
        {switched && (
          <div className={NOTICE_BOX}>
            <WarningIcon className="mt-px h-4 w-4 flex-shrink-0" />
            <span>{switchedText(switched)}</span>
          </div>
        )}
        {st.preferredError && !st.running && (
          <div className={NOTICE_BOX}>
            <WarningIcon className="mt-px h-4 w-4 flex-shrink-0" />
            <span data-selectable-text>{st.preferredError}</span>
          </div>
        )}
        <div className={CARD}>
          <div className={CARD_ROW}>
            <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-blue-500 text-white">
              <DeviceIcon id={cur.id} gpu={cur.gpu} />
            </span>
            <span className="min-w-0 flex-1">
              <span className="block truncate text-sm text-gray-800 dark:text-gray-100">
                {st.running ? '正在检测' : cur.name}
                {!st.running && <HoverTip label={cur.name} wrap />}
              </span>
              {!st.running && <span data-selectable-text className="mt-0.5 block text-xs text-gray-500 dark:text-gray-400">{cur.reason}</span>}
            </span>
          </div>
        </div>
      </section>

      <section className="space-y-3">
        <div className="flex items-center justify-between gap-3">
          <h4 className={SECTION_TITLE}>检测结果</h4>
          <button type="button" className={HEADER_BUTTON} disabled={st.running || !modelReady || modelBusy} onClick={() => call('run_selftest', { onboarding: false })}>
            <ResetIcon className="h-3.5 w-3.5" />
            {st.running ? '正在检测' : st.devices.length ? '重新检测' : '开始检测'}
          </button>
        </div>
        <p className={ROW_DESC}>{selftestText(setup)}期间暂停建立索引。{isAndroid ? '未通过的 GPU、NPU 不再参与检测，可以单独重新检测。' : '未通过的显卡在更新显卡驱动或 Rummage 后才会再参与检测，也可以单独重新检测。'}</p>
        {st.devices.length || st.blocked.length ? <DeviceList st={st} values={values} /> : <p className="text-sm text-gray-500 dark:text-gray-400">还没有检测过</p>}
        {st.error && <p data-selectable-text className={ERROR_TEXT}>{renderMessage(st.error, 'font-semibold')}</p>}
        {!modelReady && <p className={ROW_DESC}>请先下载或导入模型，再检测设备</p>}
        {modelReady && modelBusy && <p className={ROW_DESC}>正在下载、导入或删除模型，完成后才能检测设备</p>}
        {error && <p data-selectable-text className={ERROR_TEXT}>{error}</p>}
      </section>

    </div>
  )
}
