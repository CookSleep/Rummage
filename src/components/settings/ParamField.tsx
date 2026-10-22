import { useEffect, useRef, useState } from 'react'
import type { ParamDef, ParamValue } from '../../types'
import { describeParam, parseParamInput, type NumberKind } from '../../lib/params'
import { ResetIcon } from '../icons'
import Select from '../Select'
import { HoverTip } from '../Tip'
import ViewportTooltip from '../ViewportTooltip'
import Toggle from './Toggle'
import { ROW_DESC, ROW_LABEL } from './styles'

interface Props {
  def: ParamDef
  value: ParamValue
  onChange: (value: ParamValue) => void
}

/**
 * 参数表中的一项：名称、控件、说明与默认值、允许范围、恢复默认。
 * 数字在失焦或回车时提交；输入无效时在输入框上方以气泡说明原因，回车不提交，失焦则恢复为已保存的值
 */
export default function ParamField({ def, value, onChange }: Props) {
  const [text, setText] = useState(String(value))
  const [invalid, setInvalid] = useState('')
  // Esc 放弃编辑时跳过随后失焦触发的提交：失焦在同一事件中发生，状态还没更新
  const cancelRef = useRef(false)
  const kind = def.kind

  // 保存成功、恢复默认或其他窗口改了值时跟上
  useEffect(() => {
    setText(String(value))
    setInvalid('')
  }, [value])

  const commit = (blur: boolean) => {
    if (cancelRef.current) {
      cancelRef.current = false
      return
    }
    // 清空即采用占位文字中的默认值
    if (!text.trim()) {
      setText(String(def.default))
      setInvalid('')
      if (def.default !== value) onChange(def.default)
      return
    }
    // 只有数字输入框调用
    const res = parseParamInput(kind as NumberKind, text)
    // 回车时保留气泡等用户改正，失焦时放弃这次输入
    if ('error' in res) {
      setText(blur ? String(value) : text)
      setInvalid(blur ? '' : res.error)
      return
    }
    setInvalid('')
    if (res.value !== value) onChange(res.value)
  }

  const control =
    kind === 'Bool' ? (
      <Toggle checked={value === true} onChange={onChange} label={def.label} />
    ) : 'Named' in kind ? (
      <div className="min-w-0">
        <Select value={value as number} onChange={onChange} options={kind.Named.options.map(([v, label]) => ({ label: kind.Named.unit ? `${label}（${v} ${kind.Named.unit}）` : label, value: v }))} />
      </div>
    ) : (
      <span className="relative inline-flex shrink-0">
        <input
          value={text}
          onChange={(e) => {
            const res = parseParamInput(kind, e.target.value)
            setText(e.target.value)
            // 清空时显示默认值作占位文字，不提示
            setInvalid('error' in res && e.target.value.trim() ? res.error : '')
          }}
          onBlur={() => commit(true)}
          onKeyDown={(e) => {
            // 阻止默认动作：提交可能弹出确认框并聚焦“修改”按钮，这次回车不能落到它上面替用户确认
            if (e.key === 'Enter') {
              e.preventDefault()
              commit(false)
            }
            // 第一次 Esc 只放弃编辑、恢复原值，不关闭设置
            if (e.key === 'Escape') {
              e.stopPropagation()
              setText(String(value))
              setInvalid('')
              cancelRef.current = true
              e.currentTarget.blur()
            }
          }}
          placeholder={String(def.default)}
          inputMode={'Int' in kind ? 'numeric' : 'decimal'}
          spellCheck={false}
          aria-label={def.label}
          aria-invalid={Boolean(invalid)}
          className={`w-28 rounded-xl border bg-black/[0.04] px-3 py-1.5 text-xs text-gray-700 outline-none transition dark:bg-white/[0.06] dark:text-gray-200 ${invalid ? 'border-red-300 dark:border-red-500/50' : 'border-transparent focus:border-blue-300 dark:focus:border-blue-500/50'}`}
        />
        <ViewportTooltip visible={Boolean(invalid)} className="whitespace-nowrap">{invalid}</ViewportTooltip>
      </span>
    )

  const isDefault = value === def.default

  return (
    <div className="block">
      {/* 手机宽度下输入框与下拉菜单一律放到名称下方，同一页的各项对齐；开关窄，留在名称右侧 */}
      <div className="mb-1 flex items-center justify-between gap-3 max-sm:flex-wrap max-sm:gap-y-1.5">
        <span className={`min-w-0 truncate max-sm:whitespace-normal ${kind === 'Bool' ? '' : 'max-sm:basis-full'} ${ROW_LABEL}`}>{def.label}</span>
        {/* 控件最多占六成宽，过长的选项在下拉菜单中截断 */}
        <div className="flex min-w-0 max-w-[60%] items-center gap-1.5 max-sm:max-w-full">
          {control}
          <button
            type="button"
            disabled={isDefault}
            onClick={() => onChange(def.default)}
            aria-label={`${def.label}恢复默认`}
            className="shrink-0 rounded-full p-1 text-gray-400 transition hover:bg-black/5 hover:text-gray-600 disabled:pointer-events-none disabled:opacity-30 dark:hover:bg-white/10 dark:hover:text-gray-200"
          >
            <ResetIcon className="h-3.5 w-3.5" />
            <HoverTip label="恢复默认" />
          </button>
        </div>
      </div>
      <div data-selectable-text className={ROW_DESC}>{describeParam(def)}</div>
    </div>
  )
}
