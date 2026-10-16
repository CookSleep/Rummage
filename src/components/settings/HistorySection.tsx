import { useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { ParamDef, ParamValue, ParamValues } from '../../types'
import { errorMessage } from '../../lib/errors'
import { useSaveParam } from '../../hooks/useSaveParam'
import { ERROR_TEXT, SMALL_BUTTON } from '../onboarding/styles'
import ParamField from './ParamField'
import { ROW_DESC, ROW_LABEL, SECTION_TITLE } from './styles'

/** 搜索历史的参数：参数表中小标题为空，不在参数页的列表中，只在本区显示 */
const HISTORY_KEYS = ['save_history', 'history_limit']

interface Props {
  defs: ParamDef[]
  values: ParamValues
  setParam: (key: string, value: ParamValue) => Promise<string | null>
}

/** “搜索”页底部的搜索历史：保存开关、条数上限与清空 */
export default function HistorySection({ defs, values, setParam }: Props) {
  const { errors, save } = useSaveParam(setParam)
  const [status, setStatus] = useState('')
  const [clearError, setClearError] = useState('')

  const clear = () => {
    setStatus('')
    setClearError('')
    invoke('clear_history')
      .then(() => setStatus('已清空搜索历史'))
      .catch((err) => {
        console.warn('无法清空搜索历史：', err)
        setClearError(errorMessage(err, '未能清空搜索历史，请重试。'))
      })
  }

  return (
    <section className="space-y-4">
      <div>
        <h4 className={SECTION_TITLE}>搜索历史</h4>
        <p className={`mt-1 ${ROW_DESC}`}>只保存在本机。搜索框为空时，点击搜索框或按向下键即可看到最近的搜索。</p>
      </div>
      {HISTORY_KEYS.map((key) => {
        const def = defs.find((d) => d.key === key)
        if (!def) return null
        return <ParamField key={key} def={def} value={values[key] ?? def.default} error={errors[key]} onChange={(v) => void save(key, v)} />
      })}
      <div>
        <div className="mb-1 flex items-center justify-between gap-3">
          <span className={ROW_LABEL}>清空搜索历史</span>
          <button type="button" className={SMALL_BUTTON} onClick={clear}>
            清空
          </button>
        </div>
        {status && <div data-selectable-text className={ROW_DESC}>{status}</div>}
        {clearError && <div data-selectable-text className={ERROR_TEXT}>{clearError}</div>}
      </div>
    </section>
  )
}
