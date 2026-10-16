import { useCallback, useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { Estimate, SelftestStatus } from '../../types'
import { errorMessage } from '../../lib/errors'
import { cpuName, deviceName, switchedDevice, switchedText } from '../../lib/device'
import type { Kinds } from '../../lib/kinds'
import { diskShort, estimateIndex, formatSizeRange, formatTimeRange, pickSpeeds } from '../../lib/estimate'
import { formatSize } from '../../lib/format'
import { isAndroid } from '../../lib/platform'
import { useCoreEvent } from '../../hooks/useCoreEvent'
import ChangeDataDirButton from '../ChangeDataDirButton'
import { WarningIcon } from '../icons'
import LoadError from '../LoadError'
import { NOTICE_BOX } from '../settings/styles'
import { HoverTip } from '../Tip'
import PathText from '../PathText'
import { ERROR_TEXT, ROW_LONG_TEXT, SMALL_BUTTON } from './styles'

interface Props {
  kinds: Kinds
  preset?: number
  selftest: SelftestStatus
  /** 参数 `min_free_disk_gb`：剩余空间低于它时自动暂停建立索引 */
  minFreeGb: number
}

/**
 * 第 5 步：按各文件夹中各类文件的数量与大小，结合设备检测测得的速度预估索引时间与占用空间。
 * 文件夹还在扫描时数量会继续增加，扫描结束后自动刷新。
 * 预估占用的上限可能用尽数据目录所在磁盘的剩余空间时提示，并可更改数据目录；不阻止继续，空间不足时由自动暂停处理
 */
export default function EstimateStep({ kinds, preset, selftest, minFreeGb }: Props) {
  const [est, setEst] = useState<Estimate | null>(null)
  const [error, setError] = useState('')
  const [dirError, setDirError] = useState('')

  const load = useCallback(() => {
    invoke<Estimate>('index_estimate')
      .then((e) => {
        setEst(e)
        setError('')
      })
      .catch((err) => {
        console.warn('无法预估索引：', err)
        setError(errorMessage(err, '无法统计文件，请稍后再试。'))
      })
  }, [])

  useEffect(() => {
    load()
  }, [load])

  useCoreEvent((ev) => {
    if (ev.type === 'ScanProgress' && ev.done) load()
  })

  const st = selftest
  // 原本要用的设备本次运行中出了问题时，核心已把 selected 改为剩下的设备中最快的（附 failed），这里按实际生效的设备显示
  const sel = st.preferred === 'cpu' || st.selected === null ? undefined : st.devices[st.selected]
  const device = sel ? deviceName(sel, st.devices) : cpuName(st)
  const switched = switchedDevice(st)
  const r = est && estimateIndex(est, kinds, pickSpeeds(st), preset)

  return (
    <div>
      <h2 className="text-base font-bold text-gray-800 dark:text-gray-100">准备建立索引</h2>
      <p className="mt-1 text-sm text-gray-500 dark:text-gray-400">索引在后台进行，开始后就能搜索，最近修改的文件最先可以搜到。</p>

      {est && (
        <ul className="mt-4 space-y-2">
          {est.roots.map((root) => {
            const n = (k: string) => root.kinds[k]?.files ?? 0
            const parts = [
              kinds.documents && `文档 ${n('document')}`,
              kinds.images && `图片 ${n('image')}`,
              kinds.audio && `音频 ${n('audio')}`,
              kinds.video && `视频 ${n('video')}`,
              `共 ${root.files} 个文件`,
            ].filter(Boolean)
            return (
              <li key={root.id} className="rounded-xl bg-black/[0.03] px-3 py-2.5 dark:bg-white/[0.04]">
                <p data-selectable-text className={`text-sm text-gray-700 dark:text-gray-200 ${ROW_LONG_TEXT}`}>
                  <PathText path={root.path} />
                  <HoverTip label={root.path} wrap />
                </p>
                <p className="text-xs text-gray-400 dark:text-gray-500">{parts.join(' · ')}</p>
              </li>
            )
          })}
        </ul>
      )}

      {r && (
        <dl className="mt-4 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 text-sm">
          <dt className="text-gray-500 dark:text-gray-400">预计用时</dt>
          <dd className="text-gray-700 dark:text-gray-200">{formatTimeRange(r.time)}<span className="whitespace-nowrap">（使用「{device}」）</span></dd>
          <dt className="text-gray-500 dark:text-gray-400">占用空间</dt>
          <dd className="text-gray-700 dark:text-gray-200">{formatSizeRange(r.disk)}</dd>
        </dl>
      )}
      {r && switched && <p className="mt-2 text-xs text-amber-600 dark:text-amber-400">{switchedText(switched)}</p>}
      {r && est.freeGb != null && diskShort(r.disk.high, est.freeGb, minFreeGb) && (
        <div className={`mt-3 ${NOTICE_BOX}`}>
          <WarningIcon className="mt-px h-4 w-4 flex-shrink-0" />
          <div className="flex-1 space-y-1.5">
            <p>数据目录所在磁盘的剩余空间可能不够。</p>
            <p>
              剩余 {formatSize(est.freeGb * 2 ** 30)}，索引最多约占用 {formatSize(r.disk.high)}。剩余空间低于 {minFreeGb} GB 时，建立索引会自动暂停。
            </p>
            {/* Android 上数据目录是应用私有目录，不能更改 */}
            {!isAndroid && <ChangeDataDirButton label="更改数据目录" className={`mt-1 ${SMALL_BUTTON}`} onError={setDirError} />}
            {dirError && <p data-selectable-text className={ERROR_TEXT}>{dirError}</p>}
          </div>
        </div>
      )}
      {est?.scanning && <p className="mt-3 text-xs text-gray-400 dark:text-gray-500">还在统计文件，数量会继续增加</p>}
      <p className="mt-3 text-xs text-gray-400 dark:text-gray-500">
        {r && r.nameOnly > 0 ? `其余 ${r.nameOnly} 个文件只按文件名索引。` : ''}预估基于设备检测的速度{r && r.audioFiles + r.videoFiles > 0 ? '，音频和视频的时长按文件大小估算' : ''}，实际用时受文件内容影响。
      </p>
      {error && <LoadError message={error} onRetry={load} className="mt-2" />}
    </div>
  )
}
