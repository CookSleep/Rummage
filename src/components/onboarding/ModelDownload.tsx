import { useState, type ReactNode } from 'react'
import type { ModelFileStatus, ModelSource, ModelTask, ProxySetting, SelftestStatus, SetupStatus } from '../../types'
import { formatSize } from '../../lib/format'
import { isAndroid, openExternal } from '../../lib/platform'
import { ChevronRightIcon, CpuIcon, DocumentIcon, FileIcon, ImageIcon, NpuIcon, PauseIcon, PlayIcon } from '../icons'
import { HoverTip } from '../Tip'
import ProxyForm, { PROXY_LABEL } from './ProxyForm'
import { CARD_ROW, CARD_ROW_HOVER, ICON_TILE, INPUT, ROW_ACTIONS, ROW_ICON_TILE, ROW_LONG_TEXT, SEGMENTS, SMALL_BUTTON, segmentClass } from './styles'

// 引导的“下载模型”与设置的“模型”页共用的部分：模型行、导入行、下载源选择、代理状态、任务进度

export const MODEL_LABEL = '文字模型'
export const MMPROJ_LABEL = '图片与音视频模型'
export const GENERIC_LABEL = '通用模型'
export const NPU_LABEL = 'NPU 模型'

/** 一行模型的用途、名称与说明；`mmproj`、`npu` 与核心命令的参数一致 */
export interface ModelInfo {
  mmproj: boolean
  npu: boolean
  label: string
  model: string
  desc: string
  icon: ReactNode
}

/** 电脑上的两个模型 */
export const MODELS: ModelInfo[] = [
  { mmproj: false, npu: false, label: MODEL_LABEL, model: 'EmbeddingGemma 2', desc: '识别所有文件都要用到。', icon: <DocumentIcon className="h-4 w-4" /> },
  { mmproj: true, npu: false, label: MMPROJ_LABEL, model: 'EmbeddingGemma 2 视觉与音频组件', desc: '识别图片、音频和视频时使用。', icon: <ImageIcon className="h-4 w-4" /> },
]

/** Android 上 NPU 的设备标识（与核心的 `CandidateReport::device_id` 相同） */
export const NPU_DEVICE = 'gpu:NPU|'

/** Android：NPU 最近一次检测未通过或已屏蔽 */
export function npuFailed(st: SelftestStatus) {
  return st.devices.some((d) => d.id === NPU_DEVICE && (d.status === 'failed' || d.status === 'blacklisted')) || st.blocked.some((b) => b.id === NPU_DEVICE)
}

/**
 * 这一平台的模型行。Android 上为本机芯片的 NPU 模型（芯片有 NPU 版时）与通用模型（GPU 与 CPU 用，NPU 用不了时的退路）；
 * 一个 LiteRT 模型包含全部模态，没有单独的图片与音视频模型
 */
export function modelInfos(status: SetupStatus): ModelInfo[] {
  if (!isAndroid) return MODELS
  const generic: ModelInfo = {
    mmproj: false,
    npu: false,
    label: GENERIC_LABEL,
    model: 'EmbeddingGemma 2',
    desc: status.npu ? '本机 NPU 用不了时，用 GPU 或 CPU 识别。' : '用 GPU 或 CPU 识别所有文件。',
    icon: <CpuIcon className="h-4 w-4" />,
  }
  if (!status.npu) return [generic]
  const npu: ModelInfo = {
    mmproj: false,
    npu: true,
    label: NPU_LABEL,
    model: 'EmbeddingGemma 2 NPU 版',
    desc: `为本机芯片${status.npuChip ? `（${status.npuChip}）` : ''}编译，用 NPU 识别，速度最快。`,
    icon: <NpuIcon className="h-4 w-4" />,
  }
  return [npu, generic]
}

/** 模型行对应的文件状态 */
export function rowFile(status: SetupStatus, info: ModelInfo): ModelFileStatus {
  if (info.npu && status.npu) return status.npu
  return info.mmproj ? status.mmproj : status.model
}

/**
 * Android 引导要求的模型：已装好通用模型时用它；否则芯片有 NPU 版、NPU 也没有检测未通过时用 NPU 模型，再否则用通用模型。
 * NPU 检测未通过时，引导回到下载模型一步改为下载通用模型
 */
export function requiredModel(status: SetupStatus): ModelInfo {
  const [first, ...rest] = modelInfos(status)
  const generic = rest.length ? rest[rest.length - 1] : first
  if (!first.npu || status.model.ready || npuFailed(status.selftest)) return generic
  return first
}

/** Android：至少有一个模型可用（能检测设备、建立索引） */
export function anyModelReady(status: SetupStatus) {
  return status.model.ready || Boolean(status.npu?.ready)
}

/** 任务进度中把文件名换成模型的用途名称（`TaskProgress` 的 `label`） */
export function modelLabel(status: SetupStatus) {
  return (file: string) => {
    if (!isAndroid) return file === status.mmproj.name ? MMPROJ_LABEL : MODEL_LABEL
    if (file === status.npu?.name) return ` ${NPU_LABEL}`
    if (file === status.model.name) return GENERIC_LABEL
    if (file.startsWith('libQnn')) return ' NPU 运行库'
    return file === 'tokenizer.json' ? '分词器' : ` ${file}`
  }
}

/**
 * 一个模型的行：用途、说明、模型名称（链接到该文件的下载页）与文件名、大小与状态，右侧是操作。
 * `running` 时 `partialBytes` 是开始前的值，不显示
 */
export function ModelRow({ info, file, running, actions }: { info: ModelInfo; file: ModelFileStatus; running: boolean; actions?: ReactNode }) {
  const state = file.ready
    ? `${formatSize(file.bytes)} · 已下载`
    : !running && file.partialBytes > 0
      ? `${formatSize(file.bytes)} · 未下载完`
      : `${formatSize(file.bytes)} · 未下载`
  return (
    <div className={CARD_ROW}>
      <span className={`${ROW_ICON_TILE} ${ICON_TILE}`}>{info.icon}</span>
      <span className="min-w-0 flex-1">
        <span className="block text-sm text-gray-800 dark:text-gray-100">{info.label}</span>
        <span className="mt-0.5 block text-xs text-gray-500 dark:text-gray-400">{info.desc}</span>
        <span className="mt-0.5 flex min-w-0 items-baseline gap-1.5 text-xs max-sm:flex-wrap">
          <ExternalLink href={file.url} className="flex-shrink-0">
            {info.model}
          </ExternalLink>
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
export function ImportRow({ disabled, picking, onImport, tokenizerUrl, npu }: { disabled: boolean; picking: boolean; onImport: () => void; tokenizerUrl: string | null; npu: boolean }) {
  // 手机上默认折叠成一行，少占屏幕
  const [open, setOpen] = useState(!isAndroid)
  if (!open) {
    return (
      <button type="button" className={`${CARD_ROW} ${CARD_ROW_HOVER}`} aria-expanded={false} onClick={() => setOpen(true)}>
        <span className={ICON_TILE}>
          <FileIcon className="h-4 w-4" />
        </span>
        <span className="min-w-0 flex-1 text-sm text-gray-800 dark:text-gray-100">导入已下载的文件</span>
        <ChevronRightIcon className="h-4 w-4 flex-shrink-0 text-gray-400 dark:text-gray-500" />
      </button>
    )
  }
  return (
    <div className={CARD_ROW}>
      <span className={`${ROW_ICON_TILE} ${ICON_TILE}`}>
        <FileIcon className="h-4 w-4" />
      </span>
      <span className="min-w-0 flex-1">
        <span className="block text-sm text-gray-800 dark:text-gray-100">导入已下载的文件</span>
        <span className="mt-0.5 block text-xs text-gray-500 dark:text-gray-400">
          {picking
            ? '正在读取所选文件'
            : tokenizerUrl
              ? (
                  <>
                    直接下载不顺利时的备选方式：点击上面的模型名称打开下载页，下载列出的 .litertlm 文件，再下载分词器
                    <ExternalLink href={tokenizerUrl} className="ml-1">
                      tokenizer.json
                    </ExternalLink>
                    ，一起导入。导入时会自动识别文件，校验通过后即可使用。
                    {npu && 'NPU 模型所需的运行库不能导入，导入模型后点“下载”获取，这一项不从 Hugging Face 下载。'}
                  </>
                )
              : '直接下载不顺利时的备选方式：点击上面的模型名称打开下载页，下载列出的 .gguf 文件后导入。导入时会自动识别是哪个模型，校验通过后即可使用。'}
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

/** 在系统浏览器中打开的链接 */
function ExternalLink({ href, className = '', children }: { href: string; className?: string; children: ReactNode }) {
  return (
    <a
      href={href}
      target="_blank"
      rel="noopener noreferrer"
      onClick={(e) => {
        e.preventDefault()
        openExternal(href)
      }}
      className={`text-blue-500 hover:text-blue-600 dark:text-blue-400 dark:hover:text-blue-300 ${className}`}
    >
      {children}
    </a>
  )
}

/** 下载源的选择；自定义地址在切换到其他源时保留 */
export interface SourceChoice {
  type: ModelSource['type']
  url: string
}

const SOURCES: { value: ModelSource['type']; label: string }[] = [
  { value: 'official', label: 'Hugging Face' },
  { value: 'modelscope', label: '魔搭' },
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
        <p className="mt-1 text-gray-400 dark:text-gray-500">直连 Hugging Face 较慢或失败时，可以换用魔搭或设置代理</p>
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
  return <ProgressBar text={taskText(task, label)} done={task.done} total={task.total} speed={task.speed} bar={task.kind !== 'delete'} className={className} />
}

/** 暂停或出错中断的下载（`interrupted`）：已下载的部分，进度条停在那里 */
export function PausedProgress({ label, file, interrupted, className = '' }: { label: string; file: ModelFileStatus; interrupted: boolean; className?: string }) {
  return <ProgressBar text={`${interrupted ? '下载中断' : '已暂停'}：${label}`} done={file.partialBytes} total={file.bytes} bar className={className} paused />
}

function ProgressBar({ text, done, total, speed, bar, paused, className }: { text: string; done: number; total: number; speed?: number | null; bar: boolean; paused?: boolean; className: string }) {
  return (
    <div className={className}>
      <div className="flex items-baseline justify-between gap-x-3 text-xs text-gray-500 dark:text-gray-400 max-sm:flex-wrap max-sm:gap-y-0.5">
        <span className="truncate max-sm:whitespace-normal">{text}</span>
        {total > 0 && (
          <span className="flex-shrink-0">
            {formatSize(done)} / {formatSize(total)}
            {speed ? ` · ${formatSize(speed)}/s` : ''}
          </span>
        )}
      </div>
      {bar && (
        <div className="mt-1.5 h-1.5 overflow-hidden rounded-full bg-black/[0.06] dark:bg-white/[0.08]">
          <div className={`h-full rounded-full transition-all duration-200 ${paused ? 'bg-gray-400 dark:bg-gray-500' : 'bg-blue-500'}`} style={{ width: `${total ? (done / total) * 100 : 0}%` }} />
        </div>
      )}
    </div>
  )
}

/** 进度条右侧暂停或继续下载的图标按钮 */
export function PauseResumeButton({ paused, disabled, onClick }: { paused: boolean; disabled?: boolean; onClick: () => void }) {
  const label = paused ? '继续下载' : '暂停下载'
  return (
    <button
      type="button"
      aria-label={label}
      disabled={disabled}
      onClick={onClick}
      className="relative flex flex-shrink-0 rounded-lg p-1.5 text-gray-500 transition hover:bg-black/[0.05] hover:text-gray-700 disabled:cursor-not-allowed disabled:opacity-50 dark:text-gray-400 dark:hover:bg-white/[0.08] dark:hover:text-gray-200"
    >
      {paused ? <PlayIcon className="h-5 w-5" /> : <PauseIcon className="h-5 w-5" />}
      <HoverTip label={label} />
    </button>
  )
}

/** 任务结束后的说明的颜色 */
export function taskMessageClass(t: ModelTask) {
  if (t.state === 'failed') return 'text-red-500 dark:text-red-400'
  if (t.state === 'done') return 'text-green-600 dark:text-green-400'
  return 'text-gray-500 dark:text-gray-400'
}
