import type { ParamDef, ParamValue } from '../types'
import { KIND_LABELS } from './format'

/**
 * 设置项下方的说明。数字在说明后的括号中写允许范围，如“一次搜索最多显示多少个结果（10–500）。”；
 * 默认值显示为输入框的占位文字，开关与选项的可选值控件本身已经表明
 */
export function describeParam(def: ParamDef) {
  const kind = def.kind
  if (kind === 'Bool' || 'Named' in kind) return def.help
  const r = 'Int' in kind ? kind.Int : kind.Float
  return `${def.help.replace(/。$/, '')}（${r.min}–${r.max}）。`
}

/** 数字参数的类型 */
export type NumberKind = Extract<ParamDef['kind'], { Int: unknown } | { Float: unknown }>

/** 数字输入框的内容转为参数值；不是数字或越界时返回气泡中显示的原因 */
export function parseParamInput(kind: NumberKind, text: string): { value: number } | { error: string } {
  const int = 'Int' in kind
  const r = int ? kind.Int : kind.Float
  const t = text.trim()
  const value = Number(t)
  if (!t || !Number.isFinite(value)) return { error: int ? '请输入整数' : '请输入数字' }
  if (int && !Number.isInteger(value)) return { error: '请输入整数' }
  if (value < r.min || value > r.max) return { error: `范围 ${r.min}–${r.max}` }
  return { value }
}

/** 这些参数修改后受影响的索引，如“图片、文档”；都不影响索引时为空串 */
export function rebuildLabel(changes: [ParamDef, ParamValue][]) {
  const labels = changes.flatMap(([d]) => d.indexKinds.map((k) => KIND_LABELS[k] ?? k))
  return [...new Set(labels)].join('、')
}
