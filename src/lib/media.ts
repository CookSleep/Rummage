import type { SearchHit } from '../types'
import { hasThumb } from './masonry'

// `media` 自定义协议的地址（src-tauri/src/search.rs）：只按索引中的文件或片段 ID 取缩略图、预览图和音视频。
// Windows 与 Android 的 WebView 把自定义协议映射为 http://<协议>.localhost，与 Tauri 的 convertFileSrc 相同
const BASE = /Windows|Android/.test(navigator.userAgent) ? 'http://media.localhost' : 'media://localhost'

type Versioned = { fileId: number; mtime: number; size: number }

/** 缩略图地址带文件的修改时间与大小作为版本，内容变了地址就变，可以长期缓存 */
export function thumbUrl(hit: Versioned) {
  return `${BASE}/thumb/${hit.fileId}?v=${hit.mtime}-${hit.size}`
}

/** 视频在 `ms` 毫秒处的画面，由核心在第一次请求时生成并缓存 */
export function frameUrl(hit: Versioned, ms: number) {
  return `${BASE}/frame/${hit.fileId}/${ms}?v=${hit.mtime}-${hit.size}`
}

/** 长图命中那一段的裁剪图，区域由核心按片段 ID 从索引中查出；文档内嵌的图片与视频同样走这里，由核心从文档中取出 */
export function cropUrl(hit: Versioned, segmentId: number) {
  return `${BASE}/crop/${segmentId}?v=${hit.mtime}-${hit.size}`
}

/** 以图搜图的查询图片，搜索框标记的悬停预览用；WebView 不能直接显示的格式和过大的图片取不到 */
export function queryImageUrl(token: number) {
  return `${BASE}/query/${token}`
}

/** 预览用的原图，不缓存 */
export function imageUrl(fileId: number) {
  return `${BASE}/image/${fileId}`
}

/** 应用内播放的音视频，按 Range 分段读取 */
export function playUrl(fileId: number) {
  return `${BASE}/play/${fileId}`
}

/** WebView2 能直接播放的扩展名，对应 Rust `src-tauri/src/search.rs` 的 `play_mime`；其他格式只显示画面与时间段 */
const PLAYABLE = new Set(['mp4', 'm4v', 'mov', 'webm', 'ogv', 'mp3', 'm4a', 'aac', 'wav', 'ogg', 'oga', 'opus', 'flac'])

export function canPlay(name: string) {
  const dot = name.lastIndexOf('.')
  return dot > 0 && PLAYABLE.has(name.slice(dot + 1).toLowerCase())
}

/** 文档的内嵌媒体，对应 Rust `rummage_extract::media::Embedded::locator` */
export interface Embed {
  kind: 'image' | 'audio' | 'video'
  /** 在文档中的序号，从 1 起 */
  n: number
  /** PDF 的页码 */
  page?: number
  /** 演示文稿的幻灯片号 */
  slide?: number
  /** 表格的工作表名 */
  sheet?: string
  /** 图片的像素宽高 */
  width?: number
  height?: number
}

interface Locator {
  start_ms?: number
  end_ms?: number
  page_start?: number
  region?: { x: number; y: number; width: number; height: number }
  embed?: Embed
}

function parse(locatorJson: string): Locator {
  try {
    const v = JSON.parse(locatorJson)
    return v && typeof v === 'object' ? v : {}
  } catch {
    return {}
  }
}

/** 最佳命中片段在原图中的区域（长图切片）；整图命中或不是图片时为空 */
export function hitRegion(hit: SearchHit) {
  const seg = hit.segments[0]
  if (hit.kind !== 'image' || !seg) return null
  const r = parse(seg.locatorJson).region
  return r && r.width > 0 && r.height > 0 ? { ...r, segmentId: seg.segmentId } : null
}

/** 片段是文档的内嵌媒体时给出它，否则为空 */
export function embedOf(locatorJson: string) {
  return parse(locatorJson).embed ?? null
}

/** 最佳命中是文档中内嵌的图片或视频时，卡片显示它的画面（`/crop` 从文档中取出）；宽高未知时为空 */
export function hitEmbed(hit: SearchHit) {
  const seg = hit.segments[0]
  if (hit.kind !== 'document' || !seg) return null
  const e = embedOf(seg.locatorJson)
  if (!e || e.kind === 'audio') return null
  return { segmentId: seg.segmentId, width: e.width ?? null, height: e.height ?? null }
}

/** 音频、视频某个命中时间段的起止毫秒 */
export function timeRange(locatorJson: string) {
  const loc = parse(locatorJson)
  return loc.start_ms !== undefined && loc.end_ms !== undefined ? { start: loc.start_ms, end: loc.end_ms } : null
}

/** 视频卡片与预览用的画面时刻：最佳命中时间段的中点；没有时间段时为空 */
export function hitFrameMs(hit: SearchHit) {
  const r = hit.kind === 'video' && hit.segments[0] ? timeRange(hit.segments[0].locatorJson) : null
  return r ? Math.round((r.start + r.end) / 2) : null
}

/** 卡片显示的命中处的画面：长图命中的那一段、文档内嵌的图片与视频、视频命中的时刻；没有时为空，用缩略图 */
export function hitImageUrl(hit: SearchHit) {
  const region = hitRegion(hit) ?? hitEmbed(hit)
  const frameMs = hitFrameMs(hit)
  return region ? cropUrl(hit, region.segmentId) : frameMs !== null ? frameUrl(hit, frameMs) : null
}

/** 卡片上的画面（命中处的画面，没有时为缩略图）；没有画面的种类、占位文件和离线设备上的文件为空 */
export function hitPreviewUrl(hit: SearchHit) {
  const src = hitImageUrl(hit)
  if (hit.cloud || hit.offline || !(src || hasThumb(hit.kind))) return null
  return src ?? thumbUrl(hit)
}

/** 打开到的命中位置：音视频为最佳命中时间段的起点，PDF 为最佳命中片段的起始页；没有时为空 */
export function hitPosition(hit: SearchHit) {
  const seg = hit.segments[0]
  if (!seg) return null
  const loc = parse(seg.locatorJson)
  if ((hit.kind === 'audio' || hit.kind === 'video') && loc.start_ms !== undefined) return { ms: loc.start_ms }
  const page = loc.page_start ?? loc.embed?.page
  if (/\.pdf$/i.test(hit.name) && page) return { page }
  return null
}
