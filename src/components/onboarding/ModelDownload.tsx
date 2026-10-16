import { useState, type ReactNode } from 'react'
import type { ModelFileStatus, ModelSource, ModelTask, ProxySetting, SetupStatus } from '../../types'
import { formatSize } from '../../lib/format'
import { openExternal } from '../../lib/platform'
import { DocumentIcon, FileIcon, ImageIcon } from '../icons'
import { HoverTip } from '../Tip'
import ProxyForm, { PROXY_LABEL } from './ProxyForm'
import { CARD_ROW, ICON_TILE, INPUT, ROW_ACTIONS, ROW_LONG_TEXT, SMALL_BUTTON, segmentClass, SEGMENTS } from './styles'

// 引导的“下载模型”与设置的“模型”页共用的部分：模型行、导入行、下载源选择、代理状态、任务进度

export const MODEL_LABEL = '文字模型'
export const MMPROJ_LABEL = '图片与音视频模型'

/** 两个模型的用途、名称与说明；`mmproj` 与核心命令的参数一致 */
export const MODELS = [
  { mmproj: false, label: MODEL_LABEL, model: 'EmbeddingGemma 2', desc: '识别所有文件都要用到。', icon: <DocumentIcon className="h-4 w-4" /> },
  { mmproj: true, label: MMPROJ_LABEL, model: 'EmbeddingGemma 2 视觉与音频组件', desc: '识别图片、音频和视频时使用。', icon: <ImageIcon className="h-4 w-4" /> },
]

/** 任务进度中把文件名换成模型的用途名称（`TaskProgress` 的 `label`） */
export function modelLabel(status: SetupStatus) {
  return (file: string) => (file === status.mmproj.name ? MMPROJ_LABEL : MODEL_LABEL)
}

/**
 * 一个模型的行：用途、说明、模型名称（链接到该文件的下载页）与文件名、大小与状态，右侧是操作。
 * `url` 是模型文件页面的前缀；`running` 时 `partialBytes` 是开始前的值，不显示
 */
export function ModelRow({ info, url, file, running, actions }: { info: (typeof MODELS)[number]; url: string; file: ModelFileStatus; running: boolean; actions?: ReactNode }) {
  const state = file.ready
    ? `${formatSize(file.bytes)} · 已下载`
    : !running && file.partialBytes > 0
      ? `已下载 ${formatSize(file.partialBytes)} / ${formatSize(file.bytes)}`
      : `${formatSize(file.bytes)} · 未下载`
  const href = `${url}/${file.name}`
  return (
    <div className={CARD_ROW}>
      <span className={ICON_TILE}>{info.icon}</span>
      <span className="min-w-0 flex-1">
        <span className="block text-sm text-gray-800 dark:text-gray-100">{info.label}</span>
        <span className="mt-0.5 block text-xs text-gray-500 dark:text-gray-400">{info.desc}</span>
        <span className="mt-0.5 flex min-w-0 items-baseline gap-1.5 text-xs max-sm:flex-wrap">
          <a
            href={href}
            target="_blank"
            rel="noopener noreferrer"
            onClick={(e) => {
              e.preventDefault()
              openExternal(href)
            }}
            className="flex-shrink-0 text-blue-500 hover:text-blue-600 dark:text-blue-400 dark:hover:text-blue-300"
          >
            {info.model}
          </a>
          <span data-selectable-text className={`min-w-0 text-gray-400 dark:text-gray-500 ${ROW_LONG_TEXT}`}>
            {file.name}
            <HoverTip label={file.name} />
          </span>
        </span>
        <span className="mt-0.5 block text-xs text-gray-500 dark:text-gray-400">
          {state}
          {file.ready && file.verified === true && <span className="text-green-600 dark:text-green-400"> · 校验通过</span>}
          {file.ready && file.verified === false && <span className="text-red-500 dark:text-red-400"> · 校验未通过</span>}
          {file.ready && file.verified === null && ' · 未校验'}
        </span>
      </span>
      {actions && <span className={`gap-1.5 ${ROW_ACTIONS}`}>{actions}</span>}
    </div>
  )
}

/**
 * “导入已下载的文件”的行。`picking`：`import_models` 还没返回（选择文件中，Android 上还可能在复制所选文件），
 * 显示状态并禁用导入
 */
export function ImportRow({ disabled, picking, onImport }: { disabled: boolean; picking: boolean; onImport: () => void }) {
  return (
    <div className={CARD_ROW}>
      <span className={ICON_TILE}>
        <FileIcon className="h-4 w-4" />
      </span>
      <span className="min-w-0 flex-1">
        <span className="block text-sm text-gray-800 dark:text-gray-100">导入已下载的文件</span>
        <span className="mt-0.5 block text-xs text-gray-500 dark:text-gray-400">
          {picking ? '正在读取所选文件' : '点击上面的模型名称打开下载页，下载列出的 .gguf 文件后导入。自动识别是哪个模型，校验通过后使用。'}
        </span>
      </span>
      <span className={ROW_ACTIONS}>
        <button type="button" className={SMALL_BUTTON} disabled={disabled || picking} onClick={onImport}>
          导入
        </button>
      </span>
    </div>
  )
}

/** 下载源的选择；自定义地址在切换到其他源时保留 */
export interface SourceChoice {
  type: ModelSource['type']
  url: string
}

const SOURCES: { value: ModelSource['type']; label: string }[] = [
  { value: 'official', label: 'Hugging Face' },
  { value: 'mirror', label: '镜像 hf-mirror.com' },
  { value: 'custom', label: '自定义' },
]

export function toSource(c: SourceChoice): ModelSource {
  return c.type === 'custom' ? { type: 'custom', url: c.url } : { type: c.type }
}

export function SourcePicker({ value, onChange, disabled }: { value: SourceChoice; onChange: (c: SourceChoice) => void; disabled?: boolean }) {
  return (
    <div>
      <p className="mb-1.5 text-xs text-gray-500 dark:text-gray-400">下载源</p>
      <div className={SEGMENTS}>
        {SOURCES.map((s) => (
          <button key={s.value} type="button" disabled={disabled} className={segmentClass(value.type === s.value)} onClick={() => onChange({ ...value, type: s.value })}>
            {s.label}
          </button>
        ))}
      </div>
      {value.type === 'custom' && (
        <input
          value={value.url}
          onChange={(e) => onChange({ ...value, url: e.target.value })}
          spellCheck={false}
          placeholder="https://hf-mirror.com"
          aria-label="下载源地址"
          className={`mt-2 ${INPUT}`}
        />
      )}
    </div>
  )
}

/** 当前代理状态，可展开修改 */
export function ProxyStatus({ proxy, onProxy }: { proxy: ProxySetting; onProxy: (p: ProxySetting) => void }) {
  const [open, setOpen] = useState(false)
  return (
    <>
      <div className="text-xs text-gray-500 dark:text-gray-400">
        网络代理：{PROXY_LABEL[proxy.mode]}
        {proxy.mode === 'custom' && !proxy.url.includes('@') ? `（${proxy.url}）` : ''}
        <button type="button" className="ml-2 text-blue-500 hover:text-blue-600 dark:text-blue-400 dark:hover:text-blue-300" onClick={() => setOpen((v) => !v)}>
          {open ? '收起' : '更改'}
        </button>
        <p className="mt-1 text-gray-400 dark:text-gray-500">直连 Hugging Face 较慢或失败时，可以换用镜像或设置代理</p>
      </div>
      {open && <ProxyForm saved={proxy} onSaved={onProxy} />}
    </>
  )
}

/** `label` 把文件名换成给用户看的名称（含前导空格）；不给时显示文件名 */
function taskText(t: ModelTask, label: (file: string) => string) {
  const n = t.count > 1 ? `（${t.index}/${t.count}）` : ''
  const name = label(t.file ?? '')
  if (t.kind === 'delete') return t.file ? `正在删除${name}` : '正在删除模型'
  if (t.kind === 'verify') return t.file ? `正在校验${name}${n}` : '正在校验'
  if (t.phase === 'preparing') return '正在连接下载源'
  if (t.phase === 'checking') return `正在核对${name}${n}`
  if (t.phase === 'copying') return `正在导入${name}${n}`
  return `正在下载${name}${n}`
}

/** 进行中的任务：说明、字节数与速度、进度条（删除没有进度，不显示进度条） */
export function TaskProgress({ task, label = (f) => ` ${f}`, className = '' }: { task: ModelTask; label?: (file: string) => string; className?: string }) {
  return (
    <div className={className}>
      <div className="flex items-baseline justify-between gap-x-3 text-xs text-gray-500 dark:text-gray-400 max-sm:flex-wrap max-sm:gap-y-0.5">
        <span className="truncate max-sm:whitespace-normal">{taskText(task, label)}</span>
        {task.total > 0 && (
          <span className="flex-shrink-0">
            {formatSize(task.done)} / {formatSize(task.total)}
            {task.speed ? ` · ${formatSize(task.speed)}/s` : ''}
          </span>
        )}
      </div>
      {task.kind !== 'delete' && (
        <div className="mt-1.5 h-1.5 overflow-hidden rounded-full bg-black/[0.06] dark:bg-white/[0.08]">
          <div className="h-full rounded-full bg-blue-500 transition-all duration-200" style={{ width: `${task.total ? (task.done / task.total) * 100 : 0}%` }} />
        </div>
      )}
    </div>
  )
}

/** 任务结束后的说明的颜色 */
export function taskMessageClass(t: ModelTask) {
  if (t.state === 'failed') return 'text-red-500 dark:text-red-400'
  if (t.state === 'done') return 'text-green-600 dark:text-green-400'
  return 'text-gray-500 dark:text-gray-400'
}
