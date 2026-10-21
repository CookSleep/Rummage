import { describe, expect, it } from 'vitest'
import type { Estimate, SelftestStatus } from '../types'
import { CPU_FALLBACK, diskShort, estimateIndex, formatSizeRange, formatTimeRange, pickSpeeds } from './estimate'

const est: Estimate = {
  scanning: false,
  freeGb: null,
  roots: [
    { id: 1, path: 'D:\资料', files: 120, kinds: { document: { files: 10, bytes: 160_000, slices: 0 }, other: { files: 5, bytes: 1000, slices: 0 } }, scanning: false },
    { id: 2, path: 'E:\照片', files: 50, kinds: { image: { files: 40, bytes: 80_000_000, slices: 0 } }, scanning: false },
    // 2 个约 1 小时的 128 kbps 录音（共 115 MB）
    { id: 3, path: 'F:\录音', files: 2, kinds: { audio: { files: 2, bytes: 115_200_000, slices: 0 } }, scanning: false },
    // 3 个视频共 360 MB：按 16 Mbps 约 3 分钟，按 1 Mbps 约 48 分钟
    { id: 4, path: 'G:\视频', files: 3, kinds: { video: { files: 3, bytes: 360_000_000, slices: 0 } }, scanning: false },
  ],
}

const selftest = (over: Partial<SelftestStatus>): SelftestStatus => ({
  running: false,
  preferred: 'auto',
  preferredError: null,
  devices: [
    { id: 'cpu', gpu: false, pciId: null, name: 'CPU', status: 'passed', reason: null, textMs: 30, imageMs: 2000, audioMs: 900, videoMs: 6500, speed: null, state: null },
    { id: 'gpu:RTX 4080 SUPER|0000:01:00.0', gpu: true, pciId: '0000:01:00.0', name: 'RTX 4080 SUPER', status: 'passed', reason: null, textMs: 7, imageMs: 39, audioMs: 48, videoMs: 300, speed: null, state: null },
  ],
  selected: 1,
  error: null,
  stale: false,
  auto: false,
  failed: null,
  gpusOff: false, blocked: [],
  ...over,
})

describe('pickSpeeds', () => {
  it('优先用选中的设备，只用 CPU 时用 CPU 的检测结果，没有结果时用内置值', () => {
    expect(pickSpeeds(selftest({}))).toEqual({ textMs: 7, imageMs: 39, audioMs: 48, videoMs: 300, sliceMs: 39, gpu: true, measured: { text: false, image: false, audio: false, video: false } })
    expect(pickSpeeds(selftest({ preferred: 'cpu' }))).toEqual({ textMs: 30, imageMs: 2000, audioMs: 900, videoMs: 6500, sliceMs: 2000, gpu: false, measured: { text: false, image: false, audio: false, video: false } })
    expect(pickSpeeds(selftest({ devices: [], selected: null }))).toEqual(CPU_FALLBACK)
    // 只测了文本时图片用内置值
    const textOnly = selftest({ devices: [{ id: 'cpu', gpu: false, pciId: null, name: 'CPU', status: 'passed', reason: null, textMs: 25, imageMs: null, audioMs: null, videoMs: null, speed: null, state: null }], selected: 0 })
    expect(pickSpeeds(textOnly)).toEqual({ textMs: 25, imageMs: CPU_FALLBACK.imageMs, audioMs: CPU_FALLBACK.audioMs, videoMs: CPU_FALLBACK.videoMs, sliceMs: CPU_FALLBACK.sliceMs, gpu: false, measured: { text: false, image: false, audio: false, video: false } })
  })
})

describe('estimateIndex', () => {
  it('按开启的类型统计，范围下限不超过上限', () => {
    const all = estimateIndex(est, { documents: true, images: true, audio: false, video: false }, CPU_FALLBACK)
    expect(all.docFiles).toBe(10)
    expect(all.imageFiles).toBe(40)
    expect(all.nameOnly).toBe(125)
    expect(all.time.low).toBeGreaterThan(0)
    expect(all.time.low).toBeLessThan(all.time.high)
    expect(all.disk.low).toBeLessThan(all.disk.high)
    // 图片约 40 × 1.8 秒为主
    expect(all.time.low).toBeGreaterThanOrEqual(72)

    const docs = estimateIndex(est, { documents: true, images: false, audio: false, video: false }, CPU_FALLBACK)
    expect(docs.imageFiles).toBe(0)
    expect(docs.nameOnly).toBe(165)
    expect(docs.time.high).toBeLessThan(all.time.high)
    // 160 KB 文档至少 100 个切片：27 ms × 3 × 100
    expect(docs.time.low).toBeGreaterThanOrEqual(8)

    const none = estimateIndex(est, { documents: false, images: false, audio: false, video: false }, CPU_FALLBACK)
    expect(none.time).toEqual({ low: 0, high: 0 })
    expect(none.disk.low).toBeGreaterThan(0)
  })

  it('长图的每段按黄金样本的图片耗时计，不按整图档位换算', () => {
    const kinds = { documents: false, images: true, audio: false, video: false }
    const base = estimateIndex(est, kinds, CPU_FALLBACK)
    const long: Estimate = { ...est, roots: [{ ...est.roots[1], kinds: { image: { files: 40, bytes: 80_000_000, slices: 40 } } }] }
    const r = estimateIndex(long, kinds, CPU_FALLBACK)
    expect(r.imageFiles).toBe(40)
    expect(r.time.low - base.time.low).toBeCloseTo(40 * 1.8, 1)
    expect(r.disk.high).toBeGreaterThan(base.disk.high)
  })

  it('音频按文件大小粗估时长，每 8 秒一段', () => {
    const audio = estimateIndex(est, { documents: false, images: false, audio: true, video: false }, CPU_FALLBACK)
    expect(audio.audioFiles).toBe(2)
    expect(audio.nameOnly).toBe(173)
    // 115.2 MB：按 WAV 约 653 秒、82 段；按 64 kbps 约 4 小时、1800 段
    expect(audio.time.low).toBeCloseTo((115_200_000 / 176_400 / 8) * 0.8, 1)
    expect(audio.time.high).toBeCloseTo(1800 * 0.8 * 1.3, 1)
    expect(audio.disk.low).toBeLessThan(audio.disk.high)
  })

  it('视频按文件大小粗估时长，按预设的片段长度与帧数折算', () => {
    const kinds = { documents: false, images: false, audio: false, video: true }
    const objects = estimateIndex(est, kinds, CPU_FALLBACK)
    expect(objects.videoFiles).toBe(3)
    expect(objects.nameOnly).toBe(172)
    // 找物体：180 秒、每 2 秒一段 2 帧，90 段 × 6000 ms × 2/6 × 1.2，再按 CPU 上 140 档帧 × 2
    expect(objects.time.low).toBeCloseTo(90 * 6 * (2 / 6) * 1.2 * 2, 1)
    expect(objects.time.high).toBeCloseTo((2880 / 2) * 6 * (2 / 6) * 2 * 2, 1)
    // 找说话内容：每 6 秒一段 6 帧
    const speech = estimateIndex(est, kinds, CPU_FALLBACK, 3)
    expect(speech.time.low).toBeCloseTo(30 * 6 * 1.2 * 2, 1)
    expect(speech.disk.low).toBeLessThan(objects.disk.low)
  })
})

describe('format', () => {
  it('时间与空间范围', () => {
    expect(formatTimeRange({ low: 5, high: 40 })).toBe('不到 1 分钟')
    expect(formatTimeRange({ low: 30, high: 200 })).toBe('约 1–3 分钟')
    expect(formatTimeRange({ low: 600, high: 620 })).toBe('约 10 分钟')
    expect(formatTimeRange({ low: 1800, high: 9000 })).toBe('约 30 分钟到 2.5 小时')
    expect(formatTimeRange({ low: 7200, high: 40000 })).toBe('约 2–11 小时')
    expect(formatSizeRange({ low: 2_000_000, high: 5_000_000 })).toBe('约 1.9 MB–4.8 MB')
    expect(formatSizeRange({ low: 1000, high: 1000 })).toBe('约 1000 B')
  })
})

describe('测速结果', () => {
  const speed = { textMs: 400, imageMs: 5000, audioMs: null, videoMs: 3000, params: { image_tokens: 280 } }

  it('有测速结果的项直接用它，其余用黄金样本的耗时', () => {
    const st = selftest({ preferred: 'cpu' })
    st.devices[0] = { ...st.devices[0], speed }
    expect(pickSpeeds(st)).toEqual({ textMs: 400, imageMs: 5000, audioMs: 900, videoMs: 3000, sliceMs: 2000, gpu: false, measured: { text: true, image: true, audio: false, video: true } })
  })

  it('测速结果不再按档位、帧数与切片长度换算，只留解码开销', () => {
    const measured = { textMs: 400, imageMs: 5000, audioMs: 900, videoMs: 3000, sliceMs: 2000, gpu: false, measured: { text: true, image: true, audio: true, video: true } }
    const images = estimateIndex(est, { documents: false, images: true, audio: false, video: false }, measured)
    // 40 张 × 5 秒 × 解码开销 1–1.3
    expect(images.time.low).toBeCloseTo(40 * 5, 1)
    expect(images.time.high).toBeCloseTo(40 * 5 * 1.3, 1)
    // 找物体：90 段 × 3 秒 × 抽帧开销 1.2
    const video = estimateIndex(est, { documents: false, images: false, audio: false, video: true }, measured)
    expect(video.time.low).toBeCloseTo(90 * 3 * 1.2, 1)
    // 160 KB 文档至少 100 个切片，每段 0.4 秒
    const docs = estimateIndex(est, { documents: true, images: false, audio: false, video: false }, measured)
    expect(docs.time.low).toBeCloseTo(Math.max(10, 160_000 / 1600) * 0.4, 1)
    // 同一组耗时按黄金样本换算时区间更宽
    const golden = estimateIndex(est, { documents: false, images: true, audio: false, video: false }, { ...measured, measured: { text: false, image: false, audio: false, video: false } })
    expect(golden.time.low).toBeGreaterThan(images.time.low)
  })
})

describe('pickSpeeds 回退', () => {
  it('核心把选中项改为 CPU 时按 CPU 预估', () => {
    expect(pickSpeeds(selftest({ selected: 0, failed: 'gpu:RTX 4080 SUPER|0000:01:00.0' }))).toEqual({ textMs: 30, imageMs: 2000, audioMs: 900, videoMs: 6500, sliceMs: 2000, gpu: false, measured: { text: false, image: false, audio: false, video: false } })
  })
})

describe('diskShort', () => {
  it('预估上限加上剩余空间下限超过剩余空间时提示', () => {
    const gb = 2 ** 30
    expect(diskShort(3 * gb, 10, 2)).toBe(false)
    expect(diskShort(9 * gb, 10, 2)).toBe(true)
    expect(diskShort(8 * gb, 10, 2)).toBe(false)
    // 剩余空间未知时不提示
    expect(diskShort(100 * gb, null, 2)).toBe(false)
  })
})
