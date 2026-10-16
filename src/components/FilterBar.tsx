import { useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { RootInfo } from '../types'
import { customLabel, KIND_OPTIONS, TIME_OPTIONS, type Filters, type TimeRange } from '../lib/filter'
import { abbreviatePath } from '../lib/format'
import { isTauri } from '../lib/platform'
import { useCoreEvent } from '../hooks/useCoreEvent'
import DateRangePopover from './DateRangePopover'
import { FilterIcon } from './icons'
import MultiSelect from './MultiSelect'
import Select from './Select'

interface Props {
  filters: Filters
  onChange: (next: Filters) => void
  /** 变化时重新读取文件夹列表（设置中可能增删了文件夹） */
  refresh: unknown
  /** 后台服务是否已连接；连上时重新读取文件夹列表 */
  connected: boolean
}

/** 菜单里只显示文件夹名 */
const folderName = (path: string) => path.replace(/[\\/]+$/, '').split(/[\\/]/).pop() || path

/** 下拉框的宽度一致 */
const SELECT_WIDTH = 'w-32'

/**
 * 搜索框下方的筛选条：类型、文件夹两个多选菜单（至少留一项），修改时间下拉框。
 * 修改时间的“自定义”：选项右侧的按钮打开起止日期的弹出层；还没设范围或已选自定义时点选项本身也打开它，确定后才选中
 */
export default function FilterBar({ filters, onChange, refresh, connected }: Props) {
  const [roots, setRoots] = useState<RootInfo[]>([])
  // 读取过文件夹列表；读到之前不禁用文件夹筛选
  const [loaded, setLoaded] = useState(false)
  const [editing, setEditing] = useState(false)
  const range = customLabel(filters.from, filters.to)

  const loadRoots = () =>
    invoke<RootInfo[]>('list_roots')
      .then((list) => {
        setRoots(list)
        setLoaded(true)
      })
      .catch((err) => console.warn('无法读取文件夹列表：', err))

  useEffect(() => {
    if (isTauri && connected) loadRoots()
  }, [refresh, connected])

  // 命令行或 MCP 增删文件夹时刷新
  useCoreEvent((ev) => {
    if (ev.type === 'RootsChanged' && connected) loadRoots()
  })

  // 勾选的文件夹被移除后去掉它，都移除了就回到全部文件夹
  const rootIds = filters.rootIds
  useEffect(() => {
    if (!roots.length) return
    const kept = rootIds.filter((id) => roots.some((r) => r.id === id))
    if (kept.length !== rootIds.length) onChange({ ...filters, rootIds: kept })
  }, [roots, rootIds, filters, onChange])

  const allRoots = roots.map((r) => r.id)

  return (
    <div role="group" aria-label="筛选" className="mt-2.5 flex flex-wrap items-center justify-center gap-2 px-1">
      <div className={SELECT_WIDTH}>
        <MultiSelect
          label="类型"
          values={filters.kinds}
          onChange={(kinds) => onChange({ ...filters, kinds })}
          options={KIND_OPTIONS.map((o) => ({ label: o.label, value: o.kind }))}
          allText="全部类型"
          unit="种类型"
          tip="只勾选要找的文件类型，匹配更准"
        />
      </div>
      {/* 全部勾选存为空列表，之后新添加的文件夹也包括在内 */}
      <div className={SELECT_WIDTH}>
        <MultiSelect
          label="文件夹"
          values={rootIds.length ? rootIds : allRoots}
          onChange={(ids) => onChange({ ...filters, rootIds: ids.length === allRoots.length ? [] : ids })}
          options={roots.map((r) => ({ label: folderName(r.path), value: r.id, tip: abbreviatePath(r.path) }))}
          allText="全部文件夹"
          unit="个文件夹"
          disabled={loaded && !roots.length}
          tip={loaded && !roots.length ? '还没有添加文件夹' : undefined}
        />
      </div>
      {/* 自定义范围比其他选项长，按内容定宽 */}
      <div className="relative min-w-32">
        <Select<TimeRange>
          label="修改时间"
          value={filters.time}
          // 选项右侧的按钮键盘够不到：已选自定义时再选一次也打开弹出层
          onChange={(time) => (time === 'custom' && (!range || filters.time === 'custom') ? setEditing(true) : onChange({ ...filters, time }))}
          options={TIME_OPTIONS.map((o) =>
            o.value === 'custom'
              ? { label: o.label, value: o.value, display: range || o.label, action: { label: '选择起止日期', icon: <FilterIcon className="h-3.5 w-3.5" />, onClick: () => setEditing(true) } }
              : { label: o.label, value: o.value },
          )}
        />
        {editing && (
          <DateRangePopover
            from={filters.from}
            to={filters.to}
            onApply={(from, to) => {
              onChange({ ...filters, time: 'custom', from, to })
              setEditing(false)
            }}
            onClose={() => setEditing(false)}
          />
        )}
      </div>
    </div>
  )
}
