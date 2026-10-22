import type { ParamDef, ParamValue } from '../types'
import { errorMessage } from './errors'
import { KIND_LABELS } from './format'
import { showToast } from '../components/Toast'

/** 保存参数；已保存但需要告诉用户的提示与保存失败的原因用胶囊通知。返回是否保存成功 */
export async function saveParam(setParam: (key: string, value: ParamValue) => Promise<string | null>, key: string, value: ParamValue) {
  try {
    const warning = await setParam(key, value)
    if (warning) showToast(warning)
    return true
  } catch (err) {
    showToast(errorMessage(err, '设置未能保存，请重试。'), 'error')
    return false
  }
}

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

/** 修改影响索引的参数前的确认文字 */
export function rebuildConfirmText(changes: [ParamDef, ParamValue][]) {
  const affected = changes.filter(([d]) => d.affectsIndex)
  const text = `修改后需要重建受影响的索引（${rebuildLabel(affected)}）。\n\n重建期间旧索引仍可搜索。`
  // 以图搜图只比较同一视觉 token 档位的向量，文字查询不受档位影响
  if (!affected.some(([d]) => d.indexKinds.includes('image'))) return text
  return `${text}\n\n重建完成前，以图搜图可能找不到尚未重建的图片（文字搜图片不受影响）。`
}
