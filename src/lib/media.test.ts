import { describe, expect, it } from 'vitest'
import type { SearchHit } from '../types'
import { canPlay, hitEmbed, hitFrameMs, hitPosition, hitRegion } from './media'

function hit(kind: string, name: string, locatorJson?: string): SearchHit {
  return {
    fileId: 1,
    path: `D:\${name}`,
    name,
    kind,
    size: 1,
    mtime: 1,
    cloud: false,
    offline: false,
    missing: false,
    score: 1,
    cosine: 0.5,
    z: 3,
    matchPct: 50,
    lowRelevance: false,
    segments: locatorJson ? [{ segmentId: 9, locatorJson, cosine: 0.5 }] : [],
    nameHit: false,
    textSegments: [],
    width: null,
    height: null,
    snippet: null,
  }
}

describe('命中位置', () => {
  it('音视频取时间段起点，PDF 取起始页，其他为空', () => {
    expect(hitPosition(hit('video', 'a.mp4', '{"start_ms":1000,"end_ms":5000}'))).toEqual({ ms: 1000 })
    expect(hitPosition(hit('audio', 'a.mp3', '{"start_ms":0,"end_ms":10000}'))).toEqual({ ms: 0 })
    expect(hitPosition(hit('document', 'a.PDF', '{"char_start":0,"char_end":5,"page_start":4,"page_end":5}'))).toEqual({ page: 4 })
    expect(hitPosition(hit('document', 'a.docx', '{"char_start":0,"char_end":5,"page_start":4}'))).toBeNull()
    expect(hitPosition(hit('document', 'a.pdf'))).toBeNull()
    expect(hitPosition(hit('document', 'a.pdf', '{"embed":{"kind":"image","n":1,"pos":0,"page":7}}'))).toEqual({ page: 7 })
  })
  it('视频画面取时间段中点', () => {
    expect(hitFrameMs(hit('video', 'a.mp4', '{"start_ms":1000,"end_ms":4001}'))).toBe(2501)
    expect(hitFrameMs(hit('audio', 'a.mp3', '{"start_ms":1000,"end_ms":4000}'))).toBeNull()
  })
  it('长图切片的区域', () => {
    expect(hitRegion(hit('image', 'a.png', '{"region":{"x":0,"y":90,"width":100,"height":100}}'))).toEqual({ x: 0, y: 90, width: 100, height: 100, segmentId: 9 })
    expect(hitRegion(hit('image', 'a.png', '{}'))).toBeNull()
    expect(hitRegion(hit('image', 'a.png', '坏'))).toBeNull()
  })
  it('文档中内嵌的图片与视频显示画面，音频不显示', () => {
    expect(hitEmbed(hit('document', 'a.pdf', '{"embed":{"kind":"image","n":1,"pos":0,"page":7,"width":800,"height":600}}'))).toEqual({ segmentId: 9, width: 800, height: 600 })
    expect(hitEmbed(hit('document', 'a.pptx', '{"embed":{"kind":"video","n":2,"slide":2},"start_ms":0,"end_ms":2000}'))).toEqual({ segmentId: 9, width: null, height: null })
    expect(hitEmbed(hit('document', 'a.pptx', '{"embed":{"kind":"audio","n":3,"slide":3},"start_ms":0,"end_ms":2000}'))).toBeNull()
    expect(hitEmbed(hit('document', 'a.pdf', '{"char_start":0,"char_end":5,"page_start":4}'))).toBeNull()
  })
  it('能在应用内播放的格式', () => {
    expect(canPlay('a.MP4')).toBe(true)
    expect(canPlay('a.mkv')).toBe(false)
    expect(canPlay('mp3')).toBe(false)
  })
})
