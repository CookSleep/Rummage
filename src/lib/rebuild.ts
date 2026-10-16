import type { RebuildStatus } from '../types'
import { KIND_LABELS } from './format'

// 按新设置重建的提示文字

/** 计数用的量词 */
const UNITS: Record<string, string> = { image: '张', document: '个', audio: '个', video: '个' }

/** 还要重建的文件，如“1,234 张图片、56 个视频” */
export function rebuildCounts(byKind: Record<string, number>) {
  return Object.keys(KIND_LABELS)
    .filter((k) => byKind[k] > 0)
    .map((k) => `${byKind[k].toLocaleString('en-US')} ${UNITS[k]}${KIND_LABELS[k]}`)
    .join('、')
}

/** 设置页的提示 */
export function rebuildNotice(r: RebuildStatus) {
  return `按新设置还需重建 ${rebuildCounts(r.byKind)}，期间旧索引仍可搜索。`
}
