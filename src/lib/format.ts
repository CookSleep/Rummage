import type { FileInfo, OpenAt, SearchHit, SemanticUnavailable } from '../types'

/** 有内容索引的文件种类及名称，按图片、文档、音频、视频的顺序；其他文件只按文件名索引 */
export const KIND_LABELS: Record<string, string> = {
  image: '图片',
  document: '文档',
  audio: '音频',
  video: '视频',
}

export function kindLabel(kind: string) {
  return KIND_LABELS[kind] ?? '其他'
}

/** 能“找相似的”：只按文件名索引的文件没有内容向量 */
export function canFindSimilar(hit: SearchHit) {
  return hit.kind in KIND_LABELS
}

/** 按路径本身的写法拼回：Windows 路径用反斜杠，Android 路径用斜杠并保留开头的“/” */
function joinPath(path: string, parts: string[]) {
  if (path.includes('\\')) return parts.join('\\')
  return `${path.startsWith('/') ? '/' : ''}${parts.join('/')}`
}

/** 路径的最后一段，用作文件夹名 */
export const folderName = (path: string) => path.replace(/[\\/]+$/, '').split(/[\\/]/).pop() || path

/** 所在文件夹的缩写：保留开头（盘符或第一段）和最后两级，中间用“…”代替 */
export function abbreviateDir(path: string) {
  const parts = path.split(/[\\/]/).filter(Boolean)
  const dirs = parts.slice(0, -1)
  if (dirs.length <= 3) return joinPath(path, dirs)
  return joinPath(path, [dirs[0], '…', ...dirs.slice(-2)])
}

/** 过长的文件夹路径：保留开头（盘符或第一段）和尽量多的末尾几级，中间用“…”代替 */
export function abbreviatePath(path: string, max = 60) {
  if (path.length <= max) return path
  const parts = path.split(/[\\/]/).filter(Boolean)
  const tail: string[] = []
  for (let i = parts.length - 1; i > 0; i--) {
    if (tail.length && joinPath(path, [parts[0], '…', parts[i], ...tail]).length > max) break
    tail.unshift(parts[i])
  }
  return joinPath(path, [parts[0], '…', ...tail])
}

/** 过长的文件名：保留开头和末尾约三分之一（含扩展名），中间用“…”代替 */
export function abbreviateName(name: string, max = 40) {
  const chars = Array.from(name)
  if (chars.length <= max) return name
  const tail = Math.floor((max - 1) / 3)
  return `${chars.slice(0, max - 1 - tail).join('')}…${chars.slice(-tail).join('')}`
}

/** 补零到两位，如 5 → 05 */
export function pad2(n: number) {
  return String(n).padStart(2, '0')
}

/** 音视频中的时刻，如 03:12、1:02:05 */
export function formatClock(ms: number) {
  const s = Math.floor(ms / 1000)
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  return h ? `${h}:${pad2(m)}:${pad2(s % 60)}` : `${pad2(m)}:${pad2(s % 60)}`
}

/** “第 3 页”“第 3–4 页”这样的范围；没有起点时为空 */
function span(start: number | undefined, end: number | undefined, unit: string) {
  if (!start) return ''
  return end && end !== start ? `第 ${start}–${end} ${unit}` : `第 ${start} ${unit}`
}

const EMBED_KINDS: Record<string, string> = { image: '图片', audio: '音频', video: '视频' }

/** 文档内嵌媒体的位置：“第 5 页中的图片”“第 2 张幻灯片中的视频”“工作表“销售”中的图片”，Word 等没有页码的为“文档中的图片” */
function embedText(e: { kind: string; page?: number; slide?: number; sheet?: string }) {
  const what = EMBED_KINDS[e.kind] ?? '内容'
  if (e.page) return `第 ${e.page} 页中的${what}`
  if (e.slide) return `第 ${e.slide} 张幻灯片中的${what}`
  if (e.sheet) return `工作表“${e.sheet}”中的${what}`
  return `文档中的${what}`
}

/** 命中位置：文档内嵌媒体的位置（及时间段），音频段、视频的时间段（如 03:12–03:22），文本片段的页码或幻灯片、标题与行号；没有时为空 */
export function locatorText(locatorJson: string) {
  try {
    const loc = JSON.parse(locatorJson) as {
      heading?: string | null
      line_start?: number
      line_end?: number
      start_ms?: number
      end_ms?: number
      page_start?: number
      page_end?: number
      slide_start?: number
      slide_end?: number
      embed?: { kind: string; page?: number; slide?: number; sheet?: string }
    }
    const time = loc.start_ms !== undefined && loc.end_ms !== undefined ? `${formatClock(loc.start_ms)}–${formatClock(loc.end_ms)}` : ''
    if (loc.embed) return [embedText(loc.embed), time].filter(Boolean).join(' ')
    if (time) return time
    const where = span(loc.page_start, loc.page_end, '页') || span(loc.slide_start, loc.slide_end, '张幻灯片')
    return [where, loc.heading, span(loc.line_start, loc.line_end, '行')].filter(Boolean).join(' · ')
  } catch {
    return ''
  }
}

/** “打开”按钮在默认程序能跳到命中位置时的文案 */
export function openAtText(at: OpenAt) {
  if (at.ms !== undefined) return `打开到 ${formatClock(at.ms)}`
  return `打开到第 ${at.page} 页`
}

/**
 * 把正文按查询中的词切开，标出要高亮的部分（不分大小写）。查询按空白分词；只有一个字的词在查询不止一个词时不高亮，
 * 免得满屏都是。没有可高亮的词时整段原样返回
 */
export function highlight(text: string, query: string) {
  const words = query.trim().split(/\s+/).filter(Boolean)
  const terms = words.filter((w) => words.length === 1 || [...w].length > 1).sort((a, b) => b.length - a.length)
  if (!terms.length) return [{ text, hit: false }]
  const re = new RegExp(`(${terms.map((t) => t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|')})`, 'gi')
  // 带捕获组的 split：奇数位是匹配到的词
  return text
    .split(re)
    .map((part, i) => ({ text: part, hit: i % 2 === 1 }))
    .filter((p) => p.text)
}

/** 匹配方式 */
export function matchText(hit: SearchHit) {
  const parts = []
  if (hit.cosine !== null) parts.push('内容相近')
  if (hit.nameHit) parts.push('文件名含搜索词')
  if (hit.textSegments.length) parts.push('正文含搜索词')
  return parts.join(' · ')
}

/**
 * 匹配度，由核心按校准分数换算（`rummage-core` 的 `server/search.rs`）；
 * 没有校准分数（只有关键词命中、还没有校准）时为空
 */
export function matchPercent(hit: SearchHit) {
  return hit.matchPct === null ? null : `${hit.matchPct}%`
}

/** 折叠区的标题，如“匹配度较低的结果（12）” */
export function lowRelevanceText(count: number) {
  return `匹配度较低的结果（${count}）`
}

export function formatSize(bytes: number) {
  if (bytes < 1024) return `${bytes} B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let v = bytes / 1024
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i++
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`
}

/** 剩余时间，如“不到 1 分钟”“约 5 分钟”“约 2 小时 5 分钟”“约 1 天 3 小时”；先取整到分钟再分档 */
export function formatDuration(sec: number) {
  if (sec < 60) return '不到 1 分钟'
  const min = Math.round(sec / 60)
  if (min < 60) return `约 ${min} 分钟`
  if (min < 24 * 60) {
    const m = min % 60
    return `约 ${Math.floor(min / 60)} 小时${m ? ` ${m} 分钟` : ''}`
  }
  const h = Math.floor(min / 60) % 24
  return `约 ${Math.floor(min / 1440)} 天${h ? ` ${h} 小时` : ''}`
}

/** 本地时间，形如 2026-10-13 09:05 */
export function formatTime(ms: number) {
  const d = new Date(ms)
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())} ${pad2(d.getHours())}:${pad2(d.getMinutes())}`
}

/**
 * 详情栏中的索引状态。没有作业的文件（`rummage-index` 的 `scan` 只为开启了内容识别的图片、文档、音频、视频建作业）：
 * 仅在云端、超过上限，或类型不支持、未开启内容识别时从未识别过内容，只能按文件名搜索；
 * 已识别过的文件变成这些情况时作业保留（`scan/write.rs`），不会走到这里，有片段时仍按已建的索引说明
 */
export function indexStateText(info: FileInfo) {
  const job = info.job
  if (!job) {
    if (info.segments > 0) return '已索引'
    if (info.status === 'cloud') return '仅在云端，只按文件名搜索'
    if (info.status === 'too_large') return info.kind === 'audio' || info.kind === 'video' ? '超过时长上限，只按文件名搜索' : '超过大小上限，只按文件名搜索'
    if (info.kind in KIND_LABELS) return `未开启${kindLabel(info.kind)}的内容识别，只按文件名搜索`
    return '不支持识别这类文件的内容，只按文件名搜索'
  }
  if (job.stage === 'stored') return '已索引'
  if (job.stage === 'failed') return `索引失败：${job.lastError ?? '原因未知。'}`
  if (job.stage === 'pending') return job.lastError ? `等待重试：${job.lastError}` : '等待索引'
  return '正在索引'
}

/** 语义检索不可用时的说明；`what` 为以图搜图或找相似（不走文件名通道）时的操作名，文字搜索为空 */
export function unavailableText(reason: SemanticUnavailable | null, what: '以图搜图' | '找相似' | null) {
  if (reason === 'paused') {
    return what
      ? `正在检测设备或下载模型，完成后才能${what}。`
      : '正在检测设备或下载模型，完成前只显示文件名或正文中含有搜索词的结果。'
  }
  if (reason === 'no_model') {
    return what
      ? `尚未下载文字模型，无法${what}。可在「设置 → 模型」中下载或导入。`
      : '尚未下载文字模型，只显示文件名或正文中含有搜索词的结果。可在「设置 → 模型」中下载或导入。'
  }
  if (what && reason === 'images_off') return `尚未选择按内容查找图片，无法${what}。可在「设置 → 文件夹」的文件类型中勾选“图片”。`
  if (what && reason === 'no_mmproj') return `尚未下载图片与音视频模型，无法${what}。可在「设置 → 模型」中下载或导入。`
  if (reason === 'query_too_long') return what ? '搜索内容过长，请缩短后再试。' : '搜索内容过长，只显示文件名或正文中含有搜索词的结果。请缩短后再试。'
  return what ? `内容识别无法使用，无法${what}。请重新启动 Rummage。` : '内容识别无法使用，只显示文件名或正文中含有搜索词的结果。'
}

/** 胶囊通知、空状态等一句话提示的文字：只有一句（全文只有末尾一个“。”，没有其他“。！？”）时去掉句末句号，后台服务返回的一句话错误也一样；多句时原样 */
export function trimPeriod(msg: string) {
  if (!msg.endsWith('。')) return msg
  const body = msg.slice(0, -1)
  return /[。！？]/.test(body) ? msg : body
}
