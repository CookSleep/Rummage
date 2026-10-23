import { useEffect, useRef, useState } from 'react'
import type { ParamDef, ParamGroup, ParamValue, ParamValues } from '../../types'
import { useIndexStatus } from '../../hooks/useIndexStatus'
import { useParamChanges } from '../../hooks/useParamChanges'
import { rebuildConfirmText } from '../../lib/params'
import { isAndroid } from '../../lib/platform'
import { rebuildNotice } from '../../lib/rebuild'
import ConfirmDialog from '../ConfirmDialog'
import { ResetIcon } from '../icons'
import BackgroundRunRow from './BackgroundRunRow'
import DiagnosticsSection from './DiagnosticsSection'
import HistorySection from './HistorySection'
import ParamField from './ParamField'
import { HEADER_BUTTON, NOTICE_BOX, ROW_DESC, SECTION_TITLE } from './styles'

/** 视频识别方式为“自定义”（0）时才生效、才显示的几项 */
const VIDEO_CUSTOM_KEYS = ['video_window_s', 'video_overlap_s', 'video_frames', 'video_audio']
/** 打开“发热时放慢”时才生效、才显示 */
const SLOW_LEVEL_KEY = 'thermal_slow_headroom'
/** 打开“低电量时暂停”时才生效、才显示 */
const LOW_BATTERY_KEY = 'low_battery_percent'
/** 自检门槛改了以后要重新检测设备才生效，不自动检测 */
const SELFTEST_KEY = 'selftest_min_cosine'

const GROUP_DESC: Record<ParamGroup, string> = {
  Search: '修改后立即生效。',
  Index: '修改后重建受影响的索引，重建期间旧索引仍可搜索。',
  Resource: `控制建立索引时占用的${isAndroid ? '手机' : '电脑'}资源，不影响已建立的索引。`,
  Advanced: '一般不需要修改。',
}

interface Props {
  group: ParamGroup
  defs: ParamDef[]
  values: ParamValues
  /** 保存一项，返回已保存、但需要告诉用户的提示；失败时抛出带 `message` 的错误 */
  setParam: (key: string, value: ParamValue) => Promise<string | null>
  /** 打开“设备”页 */
  onDevice: () => void
  /** 打开时滚动到的参数 */
  focus?: string
}

/**
 * 由参数表生成的参数页：一页一组，按参数表中的小标题（`section`）分区，小标题为空的不在这里显示。
 * 影响索引的参数提交前先确认；单项与“全部恢复默认”走同一套确认。
 * 文件类型开关在“文件夹”页；“搜索”页下方是搜索历史，“高级”页下方是诊断，各自的参数在那里显示；
 * Android 的“资源”页顶部是“后台运行”（电池优化白名单）
 */
export default function ParamsTab({ group, defs, values, setParam, onDevice, focus }: Props) {
  const [recheck, setRecheck] = useState(false)
  const focusRef = useRef<HTMLDivElement>(null)
  // 参数定义与值都到了才有这一行，只在打开时滚动一次
  const focused = useRef(false)
  useEffect(() => {
    if (focused.current || !focusRef.current) return
    focused.current = true
    focusRef.current.scrollIntoView({ block: 'center' })
  })
  const { epoch, pending, request, confirm, cancel } = useParamChanges(values, setParam, (key) => key === SELFTEST_KEY && setRecheck(true))
  // 本页有影响索引的参数时，在顶部说明按新设置重建的情况
  const indexStatus = useIndexStatus(true)
  const rebuild = defs.some((d) => d.group === group && d.affectsIndex) ? indexStatus?.rebuild : null
  const custom = values.video_preset === 0
  const slow = values.slow_when_hot !== false
  const lowBattery = values.pause_on_low_battery !== false
  // 只在某个平台显示的参数（`platform`）在其他平台上不显示
  const all = defs.filter((d) => d.group === group && d.section && (!d.platform || d.platform === (isAndroid ? 'android' : 'windows')))
  const list = all.filter((d) => (custom || !VIDEO_CUSTOM_KEYS.includes(d.key)) && (slow || d.key !== SLOW_LEVEL_KEY) && (lowBattery || d.key !== LOW_BATTERY_KEY))
  // 参数表保证同一小标题的参数相邻
  const sections: { title: string; defs: ParamDef[] }[] = []
  for (const d of list) {
    const last = sections[sections.length - 1]
    if (last?.title === d.section) last.defs.push(d)
    else sections.push({ title: d.section, defs: [d] })
  }
  const desc = GROUP_DESC[group]

  // 暂时隐藏的参数也一起恢复
  const changed = all.filter((d) => values[d.key] !== d.default)

  return (
    <div className="space-y-8">
      <div className="space-y-4">
        <div className="flex items-start justify-between gap-3">
          {desc && <p className={`pt-1.5 ${ROW_DESC}`}>{desc}</p>}
          <button
            type="button"
            disabled={!changed.length}
            onClick={() => request(changed.map((d) => [d, d.default]))}
            className={`ml-auto ${HEADER_BUTTON}`}
          >
            <ResetIcon className="h-3.5 w-3.5" />
            全部恢复默认
          </button>
        </div>
        {rebuild && <div className={NOTICE_BOX}>{rebuildNotice(rebuild)}</div>}
      </div>

      {group === 'Resource' && isAndroid && <BackgroundRunRow />}

      {sections.map((sec) => (
        <section key={sec.title} className="space-y-4">
          <h4 className={SECTION_TITLE}>{sec.title}</h4>
          {sec.defs.map((d) => (
            <div key={`${d.key}-${epoch}`} ref={d.key === focus ? focusRef : undefined}>
              <ParamField def={d} value={values[d.key] ?? d.default} onChange={(v) => request([[d, v]])} />
              {d.key === SELFTEST_KEY && recheck && (
                <div className={`mt-2 ${NOTICE_BOX}`}>
                  <span className="flex-1">需要重新检测设备，新的通过标准才会生效。</span>
                  <button type="button" className="flex-shrink-0 font-medium hover:underline" onClick={onDevice}>
                    前往检测
                  </button>
                </div>
              )}
            </div>
          ))}
        </section>
      ))}

      {group === 'Search' && <HistorySection defs={defs} values={values} setParam={setParam} />}
      {group === 'Advanced' && <DiagnosticsSection defs={defs} values={values} setParam={setParam} />}

      {pending && (
        <ConfirmDialog
          title="重建索引"
          message={rebuildConfirmText(pending)}
          confirmText="修改并重建"
          onConfirm={confirm}
          onCancel={cancel}
        />
      )}
    </div>
  )
}
