import type { DeviceResult, Estimate, SelftestStatus } from '../types'
import { formatSize } from './format'
import { type Kinds, VIDEO_PRESETS } from './kinds'

// 引导中的索引时间与空间预估。只求量级，结果都是范围，系数的依据写在各常量旁

/**
 * 单条推理耗时（毫秒）：有测速结果时为按当时的参数处理一段文本、一张图片、一段音频、一个视频片段的耗时，
 * 否则为黄金样本的中位数，按档位与长度换算
 */
export interface Speeds {
  textMs: number
  imageMs: number
  audioMs: number
  /** 没有测速结果时为黄金样本的视频片段：6 帧，带声音与只看画面两种的中位数 */
  videoMs: number
  /** 是显卡测出的速度：黄金样本耗时换算到默认档位的倍数按显卡与 CPU 分别取 */
  gpu: boolean
  /** 各项是测速结果，不需要换算 */
  measured: { text: boolean; image: boolean; audio: boolean; video: boolean }
}

/**
 * 没有检测结果（跳过检测）时的 CPU 速度：文本、图片为自检实测（i7-12700H，debug 构建），
 * 音频为 CPU 实测（6 线程，5–10 秒的音频）；视频片段没有 CPU 实测，按 6 帧 70 档约为 3 张 140 档图片、
 * 另加 6 段 1 秒的音频估计
 */
export const CPU_FALLBACK: Speeds = {
  textMs: 27,
  imageMs: 1800,
  audioMs: 800,
  videoMs: 6000,
  gpu: false,
  measured: { text: false, image: false, audio: false, video: false },
}

/** 没有测速结果时：黄金文本样本只有一二十个 token，一个 384 token 的切片约为它的 3–10 倍（GPU 接近下限，CPU 接近上限） */
const CHUNK_COST = [3, 10]
/** 每个切片对应的原文字节数：384 token、重叠 15%，中英文约 900–1600 字节 */
const BYTES_PER_CHUNK = [900, 1600]
/** 长截图按区域切成几张、解码与缩放的开销，图片按单条耗时的 1–1.5 倍算 */
const IMAGE_COST = [1, 1.5]
/** 没有测速结果时：自检用 140 档图片，默认的 280 档在显卡上约慢 1.2–1.5 倍、CPU 上约 2.6 倍（SM8550 上建索引实测每张约 17 s，自检 5.2 s，含解码等开销） */
const IMAGE_TIER = { gpu: [1.2, 1.5], cpu: [2.6, 2.6] }
/** 每条向量：768 维 f16 + 256 维 int8 粗排 + 行与索引的开销 */
const VECTOR_BYTES = [1800, 2600]
/** 每个文件的元数据行与文件名全文索引（trigram） */
const FILE_BYTES = [300, 800]
/** 缩略图在浏览结果时才生成，最多每张约 40 KB */
const THUMB_BYTES = [0, 40 * 1024]
/** 片段正文另存一份（文档预览），含重叠与库的开销 */
const TEXT_STORE = [1, 1.3]
/**
 * 音频每秒的字节数，用来由文件大小粗估时长：预估时文件可能有几千个，不逐个运行 ffprobe 读时长。
 * 下限按 64 kbps 的压缩录音（时长偏长），上限按 CD 音质的 WAV（44.1 kHz 双声道 16 位，时长偏短）
 */
const AUDIO_BYTES_PER_S = [176_400, 8000]
/** 默认每 8 秒一段（段长 10 秒、重叠 2 秒），每段一次推理 */
const AUDIO_STEP_S = 8
/** 音频解码与切段的开销，按单条耗时的 1–1.3 倍算 */
const AUDIO_COST = [1, 1.3]
/** 视频每秒的字节数，同样由文件大小粗估时长：下限按 16 Mbps 的手机录像（时长偏短），上限按 1 Mbps 的压缩视频（时长偏长） */
const VIDEO_BYTES_PER_S = [2_000_000, 125_000]
/** 没有测速结果时：黄金样本的视频片段有 6 帧，每段的推理耗时按帧数折算 */
const GOLDEN_VIDEO_FRAMES = 6
/** 视频解码抽帧的开销，按推理耗时的 1.2–2 倍算 */
const VIDEO_COST = [1.2, 2]
/** 没有测速结果时：自检的视频帧为 70 档，默认的 140 档在显卡上约慢 1.1–1.9 倍、CPU 上约 2 倍 */
const FRAME_TIER = { gpu: [1.1, 1.9], cpu: [2, 2] }

export interface Range {
  low: number
  high: number
}

export interface IndexEstimate {
  /** 秒 */
  time: Range
  /** 字节 */
  disk: Range
  /** 将识别内容的文件数 */
  docFiles: number
  imageFiles: number
  audioFiles: number
  videoFiles: number
  /** 只按文件名索引的文件数 */
  nameOnly: number
}

/** 预估用的设备：选中的设备；只用 CPU 或没有可用结果时用检测中的 CPU 结果 */
export function estimateDevice(st: SelftestStatus): DeviceResult | undefined {
  const selected = st.selected !== null && st.preferred !== 'cpu' ? st.devices[st.selected] : undefined
  return selected ?? st.devices.find((d) => !d.gpu && d.status === 'passed')
}

/** 预估用的速度（设备见 `estimateDevice`）：各项有测速结果时用它，否则用黄金样本的耗时，再没有就用 `CPU_FALLBACK` */
export function pickSpeeds(st: SelftestStatus): Speeds {
  const dev = estimateDevice(st)
  const s = dev?.speed
  return {
    textMs: s?.textMs ?? dev?.textMs ?? CPU_FALLBACK.textMs,
    imageMs: s?.imageMs ?? dev?.imageMs ?? CPU_FALLBACK.imageMs,
    audioMs: s?.audioMs ?? dev?.audioMs ?? CPU_FALLBACK.audioMs,
    videoMs: s?.videoMs ?? dev?.videoMs ?? CPU_FALLBACK.videoMs,
    gpu: dev?.gpu ?? false,
    measured: { text: s?.textMs != null, image: s?.imageMs != null, audio: s?.audioMs != null, video: s?.videoMs != null },
  }
}

/** 按开启的类型汇总全部文件夹并换算时间与空间；`preset` 为视频预设，“自定义”或未知时按“找物体”估算 */
export function estimateIndex(est: Estimate, kinds: Kinds, speeds: Speeds, preset?: number): IndexEstimate {
  let docFiles = 0
  let docBytes = 0
  let imageFiles = 0
  let audioFiles = 0
  let audioBytes = 0
  let videoFiles = 0
  let videoBytes = 0
  let total = 0
  for (const root of est.roots) {
    total += root.files
    for (const [kind, t] of Object.entries(root.kinds)) {
      if (kind === 'image' && kinds.images) {
        imageFiles += t.files
      } else if (kind === 'audio' && kinds.audio) {
        audioFiles += t.files
        audioBytes += t.bytes
      } else if (kind === 'video' && kinds.video) {
        videoFiles += t.files
        videoBytes += t.bytes
      } else if (kind === 'document' && kinds.documents) {
        docFiles += t.files
        docBytes += t.bytes
      }
    }
  }
  // 每个文件至少一个切片、一段音频
  const chunksLow = Math.max(docFiles, docBytes / BYTES_PER_CHUNK[1])
  const chunksHigh = docFiles + docBytes / BYTES_PER_CHUNK[0]
  const segments = (i: 0 | 1) => Math.max(audioFiles, audioBytes / AUDIO_BYTES_PER_S[i] / AUDIO_STEP_S)
  const video = VIDEO_PRESETS.find((p) => p.value === preset) ?? VIDEO_PRESETS[0]
  // 每个视频至少一段
  const windows = (i: 0 | 1) => Math.max(videoFiles, videoBytes / VIDEO_BYTES_PER_S[i] / video.windowS)
  const tier = speeds.gpu ? 'gpu' : 'cpu'
  // 测速结果已是当前参数下单个切片、图片与视频片段的耗时，不再按档位与长度换算
  const m = speeds.measured
  const timeMs = (i: 0 | 1) =>
    (i ? chunksHigh : chunksLow) * speeds.textMs * (m.text ? 1 : CHUNK_COST[i])
    + imageFiles * speeds.imageMs * IMAGE_COST[i] * (m.image ? 1 : IMAGE_TIER[tier][i])
    + segments(i) * speeds.audioMs * AUDIO_COST[i]
    + windows(i) * speeds.videoMs * VIDEO_COST[i] * (m.video ? 1 : (video.frames / GOLDEN_VIDEO_FRAMES) * FRAME_TIER[tier][i])
  const disk = (i: 0 | 1) =>
    ((i ? chunksHigh : chunksLow) + imageFiles * IMAGE_COST[i] + segments(i) + windows(i)) * VECTOR_BYTES[i]
    + docBytes * TEXT_STORE[i]
    + total * FILE_BYTES[i]
    + imageFiles * THUMB_BYTES[i]
  return {
    time: { low: timeMs(0) / 1000, high: timeMs(1) / 1000 },
    disk: { low: disk(0), high: disk(1) },
    docFiles,
    imageFiles,
    audioFiles,
    videoFiles,
    nameOnly: Math.max(0, total - docFiles - imageFiles - audioFiles - videoFiles),
  }
}

/**
 * 数据目录所在磁盘的剩余空间可能不够建立索引：预估占用的上限加上自动暂停的下限 `min_free_disk_gb` 超过剩余空间。
 * 剩余空间查询失败时不提示
 */
export function diskShort(diskHigh: number, freeGb: number | null, minFreeGb: number) {
  if (freeGb == null) return false
  return diskHigh + minFreeGb * 2 ** 30 > freeGb * 2 ** 30
}

/** 时长的粗略写法 */
function roughDuration(sec: number) {
  if (sec < 60) return '1 分钟'
  if (sec < 3600) return `${Math.round(sec / 60)} 分钟`
  const h = sec / 3600
  return `${h < 10 ? Number(h.toFixed(1)) : Math.round(h)} 小时`
}

/** 时间范围，如“约 3–8 分钟”“不到 1 分钟” */
export function formatTimeRange(r: Range) {
  if (r.high < 60) return '不到 1 分钟'
  const low = roughDuration(r.low)
  const high = roughDuration(r.high)
  if (low === high) return `约 ${low}`
  // 单位相同时只写一次
  const unit = (s: string) => s.replace(/^[\d.]+ /, '')
  if (unit(low) === unit(high)) return `约 ${low.split(' ')[0]}–${high}`
  return `约 ${low}到 ${high}`
}

/** 空间范围，如“约 12 MB–30 MB” */
export function formatSizeRange(r: Range) {
  const low = formatSize(Math.round(r.low))
  const high = formatSize(Math.round(r.high))
  return low === high ? `约 ${low}` : `约 ${low}–${high}`
}
