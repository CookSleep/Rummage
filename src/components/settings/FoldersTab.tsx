import { useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { ParamValue, ParamValues, RootInfo, SetupStatus } from '../../types'
import { errorMessage } from '../../lib/errors'
import { useSaveParam } from '../../hooks/useSaveParam'
import { KIND_PARAMS, type Kinds, kindsOf, needsMmproj } from '../../lib/kinds'
import { WarningIcon } from '../icons'
import FoldersStep from '../onboarding/FoldersStep'
import KindsStep from '../onboarding/KindsStep'
import { ERROR_TEXT, SMALL_BUTTON, SMALL_PRIMARY_BUTTON } from '../onboarding/styles'
import { NOTICE_BOX } from './styles'

/** 占位提示：写法同 .gitignore（crates/rummage-index/src/scan/exclude.rs，不支持 `!` 取反与 `#` 注释），只举最常用的几种 */
const EXCLUDE_PLACEHOLDER = ['每行一条，写法同 .gitignore，例如：', 'node_modules', '*.tmp', '/草稿'].join('\n')

/** 一个文件夹的排除规则编辑区，展开在文件夹行下方；保存后核心重新扫描这个文件夹 */
function Excludes({ root, onClose, onSaved }: { root: RootInfo; onClose: () => void; onSaved: () => void }) {
  const [text, setText] = useState(root.excludes.join('\n'))
  const [error, setError] = useState('')
  const [saving, setSaving] = useState(false)

  const save = () => {
    setSaving(true)
    setError('')
    invoke('set_root_excludes', { id: root.id, excludes: text.split('\n') })
      .then(() => {
        onClose()
        onSaved()
      })
      .catch((err) => {
        console.warn('无法保存排除规则：', err)
        setError(errorMessage(err, '排除规则未能保存，请重试。'))
      })
      .finally(() => setSaving(false))
  }

  return (
    <div className="space-y-2">
      <textarea
        value={text}
        onChange={(e) => {
          setText(e.target.value)
          setError('')
        }}
        onKeyDown={(e) => {
          // 第一次 Esc 只退出编辑，不关闭设置
          if (e.key !== 'Escape') return
          e.stopPropagation()
          e.currentTarget.blur()
        }}
        rows={4}
        spellCheck={false}
        placeholder={EXCLUDE_PLACEHOLDER}
        aria-label="排除规则"
        className="w-full resize-y rounded-xl border border-transparent bg-white px-3 py-2 font-mono text-xs text-gray-700 shadow-sm outline-none transition placeholder:text-gray-400 focus:border-blue-300 dark:bg-white/[0.06] dark:text-gray-200 dark:shadow-none dark:focus:border-blue-500/50"
      />
      {error && <p data-selectable-text className={ERROR_TEXT}>{error}</p>}
      <div className="flex justify-end gap-2">
        <button type="button" onClick={onClose} className={SMALL_BUTTON}>
          取消
        </button>
        <button type="button" disabled={saving} onClick={save} className={SMALL_PRIMARY_BUTTON}>
          保存
        </button>
      </div>
    </div>
  )
}

interface Props {
  values: ParamValues
  setParam: (key: string, value: ParamValue) => Promise<string | null>
  /** 还没取到过引导状态时为空；后台服务断开后保留上次的值（`useSetup`） */
  setup: SetupStatus | null
  /** 转到“模型”页 */
  onModels: () => void
}

/**
 * “文件夹”页：被索引的文件夹（沿用引导的第 1 步，加上每个文件夹的排除规则）与文件类型开关（引导的第 2 步）。
 * 开着图片、音频或视频但缺少图片与音视频模型时，在开关下方提示，并提供转到“模型”页下载的入口
 */
export default function FoldersTab({ values, setParam, setup, onModels }: Props) {
  // 只显示最近改动的那一项的提示或保存失败的原因（含“设置已保存，但未能安排重建索引”这类提示）
  const { errors, save } = useSaveParam(setParam)
  const [last, setLast] = useState('')
  const error = errors[last] ?? ''
  const kinds = kindsOf(values)

  const setKind = (key: keyof Kinds, on: boolean) => {
    setLast(KIND_PARAMS[key])
    void save(KIND_PARAMS[key], on)
  }

  return (
    <div className="space-y-8">
      <FoldersStep
        section
        editor={{
          label: (r) => (r.excludes.length ? `排除 ${r.excludes.length} 条` : '排除规则'),
          panel: (r, close, reload) => <Excludes root={r} onClose={close} onSaved={reload} />,
        }}
      />
      <div>
        <KindsStep section kinds={kinds} mmprojBytes={setup?.mmproj.bytes ?? 0} mmprojReady={setup?.mmproj.ready ?? false} onChange={setKind} />
        {error && <p data-selectable-text className={`mt-2 ${ERROR_TEXT}`}>{error}</p>}
        {needsMmproj(kinds) && setup && !setup.mmproj.ready && (
          <div className={`mt-4 ${NOTICE_BOX}`}>
            <WarningIcon className="mt-px h-4 w-4 flex-shrink-0" />
            <span className="flex-1">尚未下载图片与音视频模型，图片、音频和视频目前只能按文件名搜索。下载或导入后会自动识别它们的内容。</span>
            <button type="button" className="flex-shrink-0 font-medium hover:underline" onClick={onModels}>
              前往下载
            </button>
          </div>
        )}
      </div>
    </div>
  )
}
