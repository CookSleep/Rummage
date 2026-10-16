import { useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { BlockedGpu, SelftestStatus, SetupStatus } from '../../types'
import { errorMessage } from '../../lib/errors'
import { blockedName, CPU_FALLBACK_TEXT, cpuFallbackReason, cpuFaster, deviceName, SELFTEST_TEXT, switchedDevice, switchedText } from '../../lib/device'
import { CpuIcon, GpuIcon, ResetIcon, WarningIcon } from '../icons'
import ConfirmDialog, { renderMessage } from '../ConfirmDialog'
import { HoverTip } from '../Tip'
import { DeviceList, DevicePicker } from '../onboarding/SelftestStep'
import { CARD, CARD_ROW, ERROR_TEXT, ICON_TILE, SMALL_BUTTON } from '../onboarding/styles'
import { HEADER_BUTTON, NOTICE_BOX, ROW_DESC, SECTION_TITLE } from './styles'

/** 实际生效的设备及原因：与核心选择后端的规则一致，选了 CPU、没有检测过或选中的显卡不能用时都用 CPU */
function effective(st: SelftestStatus): { gpu: boolean; name: string; reason: string } {
  const cpu = (reason: string) => ({ gpu: false, name: 'CPU', reason })
  if (st.preferred === 'cpu') return cpu('已选择使用 CPU')
  if (!st.devices.length) return cpu('还没有检测过设备')
  const d = st.selected !== null ? st.devices[st.selected] : undefined
  if (!d) return cpu('没有通过检测的设备，将尝试使用 CPU')
  // 设备本次运行中出了问题时，核心已把选中项改为改用的设备；按它现在的情况说明之后能否再用
  const s = switchedDevice(st)
  if (s) {
    const state = st.devices.find((x) => x.id === st.failed)?.state
    const reason = st.blocked.some((b) => b.id === st.failed)
      ? `「${s.from}」已屏蔽，可以在下方解除屏蔽`
      : state === 'absent'
        ? `「${s.from}」不在系统中，重新接入后需重新启动 Rummage 才会使用它`
        : `本次运行中不再使用「${s.from}」，重新启动 Rummage 后会照常使用它`
    return { gpu: d.gpu, name: s.to, reason }
  }
  // 显卡出现可能与驱动有关的问题（如列出显卡时卡住）：本次运行所有显卡都停用
  if (st.gpusOff) return cpu('本次运行中不再使用显卡，重新启动 Rummage 后会重新检测')
  if (st.preferred !== 'auto' && !st.preferredError) return { gpu: d.gpu, name: deviceName(d, st.devices), reason: '已选择使用这块显卡' }
  if (d.gpu) return { gpu: true, name: deviceName(d, st.devices), reason: '检测通过的设备中速度最快' }
  if (cpuFaster(st)) return cpu('检测后选用了 CPU，它比显卡快')
  if (st.devices.some((x) => x.gpu)) return cpu('没有更快的显卡通过检测')
  return cpu(st.blocked.length ? '显卡已屏蔽，可以在下方解除屏蔽' : '没有找到可用的显卡')
}

/**
 * “设备”页：顶部选择使用的设备（自动、检测通过的显卡或 CPU），当前使用的设备与原因、每个候选的检测结果、重新检测。
 * 选了的显卡之后不能用时核心按自动选择，下拉菜单显示“自动”并说明原因。
 * 结果过期时后台服务自动重新检测（显示与手动检测相同）；检测未通过而用 CPU、或原本要用的设备出了问题而改用别的设备时提示，可手动重新检测；
 * 检测通过但 CPU 更快时只在当前设备下说明，不提示。
 * 被屏蔽的显卡单独列出（本次测过未通过的同时留在检测结果中），可以解除屏蔽（确认后删除它的全部屏蔽记录，后台服务随即只重测它）；重新检测不清除屏蔽与未通过记录
 */
export default function DeviceTab({ setup }: { setup: SetupStatus }) {
  const [error, setError] = useState('')
  // 等待确认解除屏蔽的显卡，以及是否正在解除
  const [unblocking, setUnblocking] = useState<BlockedGpu | null>(null)
  const [busy, setBusy] = useState(false)
  const [unblockError, setUnblockError] = useState('')
  // 解除屏蔽后没能当场重新检测的原因，显示在检测结果下方
  const [unblockNote, setUnblockNote] = useState('')
  const st = setup.selftest
  const cur = effective(st)
  // 黄色提示只说检测未通过、显卡已屏蔽与设备出了问题；正在检测、尚未检测看下方当前设备，结果过期多是自动检测出错或被中止，不提示
  const fallback = cpuFallbackReason(st)
  const switched = switchedDevice(st)
  // 下载、导入或删除会替换或删掉检测要用的模型文件（校验只读，不影响）
  const modelBusy = setup.task?.state === 'running' && setup.task.kind !== 'verify'

  const unblock = (b: BlockedGpu) => {
    setUnblockError('')
    setUnblockNote('')
    setBusy(true)
    invoke<string | null>('unblock_gpu', { device: b.id })
      .then((note) => setUnblockNote(note ?? ''))
      .catch((err) => {
        console.warn('unblock_gpu 失败：', err)
        setUnblockError(errorMessage(err, '无法解除屏蔽，请重试。'))
      })
      .finally(() => {
        setBusy(false)
        setUnblocking(null)
      })
  }

  const call = (cmd: string, args?: Record<string, unknown>) => {
    setError('')
    setUnblockNote('')
    invoke(cmd, args).catch((err) => {
      console.warn(`${cmd} 失败：`, err)
      setError(errorMessage(err, '操作失败，请重试。'))
    })
  }

  return (
    <div className="space-y-8">
      <section className="space-y-3">
        <h4 className={SECTION_TITLE}>使用的设备</h4>
        <DevicePicker st={st} />
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
              {cur.gpu ? <GpuIcon className="h-4 w-4" /> : <CpuIcon className="h-4 w-4" />}
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
          <button type="button" className={HEADER_BUTTON} disabled={st.running || !setup.model.ready || modelBusy} onClick={() => call('run_selftest', { onboarding: false })}>
            <ResetIcon className="h-3.5 w-3.5" />
            {st.running ? '正在检测' : st.devices.length ? '重新检测' : '开始检测'}
          </button>
        </div>
        <p className={ROW_DESC}>{SELFTEST_TEXT}期间暂停建立索引。未通过或已屏蔽的显卡在更新显卡驱动或 Rummage 后，再次重新检测才会参与测试。</p>
        {st.devices.length ? <DeviceList devices={st.devices} /> : <p className="text-sm text-gray-500 dark:text-gray-400">还没有检测过</p>}
        {st.error && <p data-selectable-text className={ERROR_TEXT}>{renderMessage(st.error, 'font-semibold')}</p>}
        {unblockNote && !st.running && <p data-selectable-text className={ROW_DESC}>{unblockNote}</p>}
        {!setup.model.ready && <p className={ROW_DESC}>请先下载或导入模型，再检测设备</p>}
        {setup.model.ready && modelBusy && <p className={ROW_DESC}>正在下载、导入或删除模型，完成后才能检测设备</p>}
        {error && <p data-selectable-text className={ERROR_TEXT}>{error}</p>}
      </section>

      {st.blocked.length > 0 && (
        <section className="space-y-3">
          <h4 className={SECTION_TITLE}>已屏蔽的显卡</h4>
          <p className={ROW_DESC}>出现过驱动错误的显卡会被屏蔽，自动检测不再测试它，也不会使用它。</p>
          <div className={CARD}>
            {st.blocked.map((b) => (
              <div key={b.id} className={CARD_ROW}>
                <span className={ICON_TILE}>
                  <GpuIcon className="h-4 w-4" />
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm text-gray-800 dark:text-gray-100">
                    {blockedName(b, st)}
                    <HoverTip label={blockedName(b, st)} wrap />
                  </span>
                  <span data-selectable-text className="mt-0.5 block whitespace-pre-line text-xs text-gray-500 dark:text-gray-400">{b.reason ?? '这块显卡出现过驱动错误。'}</span>
                </span>
                {/* 禁用的按钮收不到悬停，气泡挂在外层 */}
                <span className="shrink-0">
                  <button type="button" className={SMALL_BUTTON} disabled={st.running} onClick={() => setUnblocking(b)}>
                    解除屏蔽
                  </button>
                  <HoverTip label="设备检测进行中" hide={!st.running} />
                </span>
              </div>
            ))}
          </div>
          {unblockError && <p data-selectable-text className={ERROR_TEXT}>{unblockError}</p>}
        </section>
      )}

      {unblocking && (
        <ConfirmDialog
          title="解除屏蔽"
          message={`解除后会立即重新检测「${blockedName(unblocking, st)}」，期间暂停建立索引。

如果问题仍然存在，检测可能再次失败，这块显卡可能重新被屏蔽。`}
          confirmText="解除屏蔽"
          busy={busy ? '正在解除' : undefined}
          onConfirm={() => unblock(unblocking)}
          onCancel={() => setUnblocking(null)}
        />
      )}
    </div>
  )
}
