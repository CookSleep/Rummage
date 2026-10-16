import type { SearchFilter } from '../types'
import { kindLabel } from './format'

// 搜索页的筛选条，条件在本次运行内保持

/** 类型多选菜单的选项，键为文件种类；`other` 是只按文件名索引的文件 */
export const KIND_OPTIONS = ['document', 'image', 'audio', 'video', 'other'].map((kind) => ({ kind, label: kindLabel(kind) }))

/** 按修改时间筛选；天数为空表示不限，0 表示今天 */
export const TIME_OPTIONS = [
  { value: 'any', label: '任意时间', days: null },
  { value: 'today', label: '今天', days: 0 },
  { value: 'week', label: '最近 7 天', days: 7 },
  { value: 'month', label: '最近 30 天', days: 30 },
  { value: 'year', label: '最近一年', days: 365 },
  { value: 'custom', label: '自定义', days: null },
] as const

export type TimeRange = (typeof TIME_OPTIONS)[number]['value']

export interface Filters {
  kinds: string[]
  /** 勾选的根目录 ID；为空表示全部文件夹，之后新添加的文件夹也包括在内 */
  rootIds: number[]
  time: TimeRange
  /** 自定义的起止日期（本地时区的 `YYYY-MM-DD`，都含当天），空字符串表示这一端不限；`time` 为 `custom` 时生效 */
  from: string
  to: string
}

export const DEFAULT_FILTERS: Filters = {
  kinds: KIND_OPTIONS.map((o) => o.kind),
  rootIds: [],
  time: 'any',
  from: '',
  to: '',
}

const DAY_MS = 86_400_000

/** `YYYY-MM-DD` 在本地时区当天 0 点的 Date；格式不对时为 null */
function localDay(s: string, offset = 0) {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(s)
  return m ? new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]) + offset) : null
}

/** 自定义范围的起点晚于终点 */
export const isReversed = (from: string, to: string) => !!from && !!to && from > to

/** 触发器上的自定义范围，如“2025-03-01 – 2025-06-30”“2025-03-01 以后”；都没填时为空 */
export function customLabel(from: string, to: string) {
  if (from && to) return `${from} – ${to}`
  if (from) return `${from} 以后`
  if (to) return `${to} 以前`
  return ''
}

/**
 * 换成搜索请求的筛选条件；“今天”从本地时间的零点算起，其余从现在往前推。
 * 自定义范围按本地时区含当天：起点为当天 0 点，终点为次日 0 点前 1 毫秒，与命令行的 `--since`、`--until` 相同
 */
export function toSearchFilter(f: Filters, now = new Date()): SearchFilter {
  const kinds = f.kinds.length === KIND_OPTIONS.length ? [] : f.kinds
  if (f.time === 'custom') {
    const next = localDay(f.to, 1)
    return { kinds, rootIds: f.rootIds, since: localDay(f.from)?.getTime() ?? null, until: next ? next.getTime() - 1 : null }
  }
  const days = TIME_OPTIONS.find((o) => o.value === f.time)?.days ?? null
  const since = days === null ? null : days === 0 ? new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime() : now.getTime() - days * DAY_MS
  return { kinds, rootIds: f.rootIds, since, until: null }
}
