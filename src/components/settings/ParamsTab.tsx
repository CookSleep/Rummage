import { useState } from 'react'
import type { ParamDef, ParamGroup, ParamValue, ParamValues } from '../../types'
import { useIndexStatus } from '../../hooks/useIndexStatus'
import { useSaveParam } from '../../hooks/useSaveParam'
import { rebuildLabel } from '../../lib/params'
import { rebuildNotice } from '../../lib/rebuild'
import ConfirmDialog from '../ConfirmDialog'
import { ResetIcon } from '../icons'
import DiagnosticsSection from './DiagnosticsSection'
import HistorySection from './HistorySection'
import ParamField from './ParamField'
import { HEADER_BUTTON, NOTICE_BOX, ROW_DESC, SECTION_TITLE } from './styles'

/** 视频识别方式为“自定义”（0）时才生效、才显示的几项 */
const VIDEO_CUSTOM_KEYS = ['video_window_s', 'video_overlap_s', 'video_frames', 'video_audio']
/** 自检门槛改了以后要重新检测设备才生效，不自动检测 */
const SELFTEST_KEY = 'selftest_min_cosine'

const GROUP_DESC: Record<ParamGroup, string> = {
  Search: '修改后立即生效。',
  Index: '修改后重建受影响的索引，重建期间旧索引仍可搜索。',
  Resource: '控制建立索引时占用的电脑资源，不影响已建立的索引。',
  Advanced: '一般不需要修改。',
}

function confirmText(changes: [ParamDef, ParamValue][]) {
  const affected = changes.filter(([d]) => d.affectsIndex)
  const text = `修改后需要重建受影响的索引（${rebuildLabel(affected)}）。\n\n重建期间旧索引仍可搜索。`
  // 以图搜图只比较同一视觉 token 档位的向量，文字查询不受档位影响
  if (!affected.some(([d]) => d.indexKinds.includes('image'))) return text
  return `${text}\n\n重建完成前，以图搜图可能找不到尚未重建的图片（文字搜图片不受影响）。`
}

interface Props {
  group: ParamGroup
  defs: ParamDef[]
  values: ParamValues
  /** 保存一项，返回已保存、但需要告诉用户的提示；失败时抛出带 `message` 的错误 */
  setParam: (key: string, value: ParamValue) => Promise<string | null>
  /** 打开“设备”页 */
  onDevice: () => void
}

/**
 * 由参数表生成的参数页：一页一组，按参数表中的小标题（`section`）分区，小标题为空的不在这里显示。
 * 影响索引的参数提交前先确认；单项与“全部恢复默认”走同一套确认。
 * 文件类型开关在“文件夹”页；“搜索”页下方是搜索历史，“高级”页下方是诊断，各自的参数在那里显示
 */
export default function ParamsTab({ group, defs, values, setParam, onDevice }: Props) {
  const { errors, save } = useSaveParam(setParam)
  // 等待确认的修改
  const [pending, setPending] = useState<[ParamDef, ParamValue][] | null>(null)
  // 取消确认后递增，让输入框回到当前值
  const [epoch, setEpoch] = useState(0)
  const [recheck, setRecheck] = useState(false)
  // 本页有影响索引的参数时，在顶部说明按新设置重建的情况
  const indexStatus = useIndexStatus(true)
  const rebuild = defs.some((d) => d.group === group && d.affectsIndex) ? indexStatus?.rebuild : null
  const custom = values.video_preset === 0
  const list = defs.filter((d) => d.group === group && d.section && (custom || !VIDEO_CUSTOM_KEYS.includes(d.key)))
  // 参数表保证同一小标题的参数相邻
  const sections: { title: string; defs: ParamDef[] }[] = []
  for (const d of list) {
    const last = sections[sections.length - 1]
    if (last?.title === d.section) last.defs.push(d)
    else sections.push({ title: d.section, defs: [d] })
  }
  const desc = GROUP_DESC[group]

  const apply = async (changes: [ParamDef, ParamValue][]) => {
    for (const [d, v] of changes) {
      if ((await save(d.key, v)) && d.key === SELFTEST_KEY) setRecheck(true)
    }
  }

  const request = (changes: [ParamDef, ParamValue][]) => {
    const real = changes.filter(([d, v]) => values[d.key] !== v)
    if (!real.length) return
    if (real.some(([d]) => d.affectsIndex)) {
      setPending(real)
      return
    }
    void apply(real)
  }

  const changed = list.filter((d) => values[d.key] !== d.default)

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

      {sections.map((sec) => (
        <section key={sec.title} className="space-y-4">
          <h4 className={SECTION_TITLE}>{sec.title}</h4>
          {sec.defs.map((d) => (
            <div key={`${d.key}-${epoch}`}>
              <ParamField def={d} value={values[d.key] ?? d.default} error={errors[d.key]} onChange={(v) => request([[d, v]])} />
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
          message={confirmText(pending)}
          confirmText="修改并重建"
          onConfirm={() => {
            void apply(pending)
            setPending(null)
          }}
          onCancel={() => {
            setPending(null)
            setEpoch((n) => n + 1)
          }}
        />
      )}
    </div>
  )
}
