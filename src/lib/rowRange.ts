/** 定高列表中要渲染的行 [start, end)：可视区域及上下各多 `overscan` 行，其余行不渲染（虚拟化） */
export function rowRange(scrollTop: number, viewHeight: number, rowHeight: number, count: number, overscan = 5) {
  const start = Math.max(0, Math.floor(scrollTop / rowHeight) - overscan)
  const end = Math.min(count, Math.ceil((scrollTop + viewHeight) / rowHeight) + overscan)
  return { start, end: Math.max(start, end) }
}
