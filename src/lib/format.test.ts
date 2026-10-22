import { describe, expect, it } from 'vitest'
import type { FileInfo, SearchHit } from '../types'
import { abbreviateDir, abbreviateName, abbreviatePath, formatClock, formatDuration, formatSize, highlight, indexStateText, locatorText, lowRelevanceText, matchPercent, matchText, openAtText, trimPeriod, unavailableText } from './format'

describe('abbreviateDir', () => {
  it('短路径原样显示所在文件夹，长路径省略中间', () => {
    expect(abbreviateDir('D:\\照片\\a.jpg')).toBe('D:\\照片')
    expect(abbreviateDir('D:\\资料\\工作\\a.md')).toBe('D:\\资料\\工作')
    expect(abbreviateDir('D:\\资料\\工作\\2025\\总结\\a.md')).toBe('D:\\…\\2025\\总结')
  })

  it('Android 路径保留斜杠', () => {
    expect(abbreviateDir('/storage/emulated/0/DCIM/Camera/a.jpg')).toBe('/storage/…/DCIM/Camera')
    expect(abbreviateDir('/sdcard/DCIM/a.jpg')).toBe('/sdcard/DCIM')
  })
})

describe('abbreviatePath', () => {
  it('不超长时原样显示，超长时保留开头和尽量多的末尾几级', () => {
    expect(abbreviatePath('D:\\资料\\工作')).toBe('D:\\资料\\工作')
    expect(abbreviatePath('D:\\资料\\工作\\2025\\总结', 12)).toBe('D:\\…\\2025\\总结')
    expect(abbreviatePath('/storage/emulated/0/Documents/工作', 24)).toBe('/storage/…/Documents/工作')
  })
})

describe('abbreviateName', () => {
  it('不超长时原样显示，超长时省略中间、保留扩展名', () => {
    expect(abbreviateName('柯基.jpg')).toBe('柯基.jpg')
    expect(abbreviateName('abcdefghijklmnop.png', 10)).toBe('abcdef…png')
    expect(abbreviateName('😀😀😀😀😀😀😀😀😀😀😀😀', 10)).toBe('😀😀😀😀😀😀…😀😀😀')
  })
})

describe('locatorText', () => {
  it('显示标题与行号，格式不对时为空', () => {
    expect(locatorText('{"heading":"安装 > Windows","line_start":3,"line_end":8}')).toBe('安装 > Windows · 第 3–8 行')
    expect(locatorText('{"heading":null,"line_start":5,"line_end":5}')).toBe('第 5 行')
    expect(locatorText('{}')).toBe('')
    expect(locatorText('不是 JSON')).toBe('')
    // 音频段：时间段
    expect(locatorText('{"start_ms":192000,"end_ms":202000}')).toBe('03:12–03:22')
    expect(locatorText('{"start_ms":0,"end_ms":9660}')).toBe('00:00–00:09')
    expect(locatorText('{"start_ms":3725000,"end_ms":3735000}')).toBe('1:02:05–1:02:15')
    // 文档：页码或幻灯片在前，再是标题
    expect(locatorText('{"char_start":0,"char_end":9,"heading":"总结","page_start":3,"page_end":4}')).toBe('第 3–4 页 · 总结')
    expect(locatorText('{"char_start":0,"char_end":9,"heading":"","page_start":2,"page_end":2}')).toBe('第 2 页')
    expect(locatorText('{"char_start":0,"char_end":9,"heading":"","slide_start":5,"slide_end":5}')).toBe('第 5 张幻灯片')
    // 文档的内嵌媒体
    expect(locatorText('{"embed":{"kind":"image","n":1,"pos":2,"page":5}}')).toBe('第 5 页中的图片')
    expect(locatorText('{"embed":{"kind":"video","n":2,"slide":2,"part":"ppt/media/media1.mp4"},"start_ms":3000,"end_ms":5000}')).toBe('第 2 张幻灯片中的视频 00:03–00:05')
    expect(locatorText('{"embed":{"kind":"image","n":1,"sheet":"销售","part":"xl/media/image1.png"}}')).toBe('工作表“销售”中的图片')
    expect(locatorText('{"embed":{"kind":"audio","n":3,"part":"word/media/a.m4a"},"start_ms":0,"end_ms":10000}')).toBe('文档中的音频 00:00–00:10')
  })
})

describe('openAtText', () => {
  it('时间写成时刻，PDF 写页码', () => {
    expect(openAtText({ ms: 192_500 })).toBe('打开到 03:12')
    expect(openAtText({ page: 7 })).toBe('打开到第 7 页')
  })
})

describe('highlight', () => {
  const hits = (text: string, q: string) => highlight(text, q).filter((p) => p.hit).map((p) => p.text)
  it('按空白分词，不分大小写，正文拼回去不变', () => {
    expect(hits('Rust 与 rust 的年终总结', 'RUST 总结')).toEqual(['Rust', 'rust', '总结'])
    const parts = highlight('a+b (c)', 'a+b (c)')
    expect(parts.map((p) => p.text).join('')).toBe('a+b (c)')
    expect(hits('a+b (c)', 'a+b')).toEqual(['a+b'])
  })
  it('多词查询中的单字不高亮，单独一个字时照常高亮', () => {
    expect(hits('猫和狗', '猫 小狗')).toEqual([])
    expect(hits('猫和狗', '猫')).toEqual(['猫'])
    expect(highlight('正文', '  ')).toEqual([{ text: '正文', hit: false }])
  })
})

describe('formatClock', () => {
  it('不到 1 小时写分秒，否则加小时', () => {
    expect(formatClock(0)).toBe('00:00')
    expect(formatClock(59_999)).toBe('00:59')
    expect(formatClock(3_600_000)).toBe('1:00:00')
  })
})

describe('formatSize', () => {
  it('按 1024 进位，小于 10 时保留一位小数', () => {
    expect(formatSize(512)).toBe('512 B')
    expect(formatSize(1536)).toBe('1.5 KB')
    expect(formatSize(20 * 1024 * 1024)).toBe('20 MB')
  })
})

describe('formatDuration', () => {
  it('按分钟、小时、天分档，零头为 0 时省略', () => {
    expect(formatDuration(0)).toBe('不到 1 分钟')
    expect(formatDuration(59)).toBe('不到 1 分钟')
    expect(formatDuration(60)).toBe('约 1 分钟')
    expect(formatDuration(3569)).toBe('约 59 分钟')
    // 取整到 60 分钟时进入小时档
    expect(formatDuration(3570)).toBe('约 1 小时')
    expect(formatDuration(3600)).toBe('约 1 小时')
    expect(formatDuration(3660)).toBe('约 1 小时 1 分钟')
    expect(formatDuration(86369)).toBe('约 23 小时 59 分钟')
    expect(formatDuration(86370)).toBe('约 1 天')
    expect(formatDuration(86400)).toBe('约 1 天')
    expect(formatDuration(86400 + 3540)).toBe('约 1 天')
    expect(formatDuration(86400 + 3600)).toBe('约 1 天 1 小时')
    expect(formatDuration(3 * 86400 + 23 * 3600 + 59 * 60)).toBe('约 3 天 23 小时')
  })
})

describe('indexStateText', () => {
  const base: FileInfo = {
    fileId: 1, path: 'D:\a.md', name: 'a.md', ext: 'md', kind: 'document', cloud: false, offline: false, missing: false,
    size: 1, mtime: 0, width: null, height: null, status: 'stored', job: null, segments: 0, indexedAt: null, spaces: [],
  }

  it('按作业与文件状态说明', () => {
    // 没有作业：按为什么没有识别内容说明
    expect(indexStateText(base)).toBe('未开启文档的内容识别，只按文件名搜索')
    expect(indexStateText({ ...base, ext: 'rs', kind: 'other' })).toBe('不支持识别这类文件的内容，只按文件名搜索')
    expect(indexStateText({ ...base, status: 'cloud' })).toBe('仅在云端，只按文件名搜索')
    expect(indexStateText({ ...base, status: 'too_large' })).toBe('超过大小上限，只按文件名搜索')
    expect(indexStateText({ ...base, kind: 'audio', status: 'too_large' })).toBe('超过时长上限，只按文件名搜索')
    expect(indexStateText({ ...base, segments: 3 })).toBe('已索引')
    expect(indexStateText({ ...base, job: { stage: 'stored', attempts: 0, lastError: null } })).toBe('已索引')
    expect(indexStateText({ ...base, job: { stage: 'failed', attempts: 3, lastError: '坏了。' } })).toBe('索引失败：坏了。')
    expect(indexStateText({ ...base, job: { stage: 'pending', attempts: 1, lastError: '超时。' } })).toBe('等待重试：超时。')
    expect(indexStateText({ ...base, job: { stage: 'embedding', attempts: 0, lastError: null } })).toBe('正在索引')
  })
})

describe('unavailableText', () => {
  it('按原因说明语义检索为什么不可用', () => {
    expect(unavailableText('images_off', '以图搜图')).toBe('尚未选择按内容查找图片，无法以图搜图。可在「设置 → 文件夹」的文件类型中勾选“图片”。')
    expect(unavailableText('no_mmproj', '以图搜图')).toBe('尚未下载图片与音视频模型，无法以图搜图。可在「设置 → 模型」中下载或导入。')
    expect(unavailableText('no_model', null)).toBe('尚未下载文字模型，只显示文件名或正文中含有搜索词的结果。可在「设置 → 模型」中下载或导入。')
    expect(unavailableText('no_model', '以图搜图')).toBe('尚未下载文字模型，无法以图搜图。可在「设置 → 模型」中下载或导入。')
    expect(unavailableText('paused', null)).not.toContain('内容识别无法使用')
    expect(unavailableText('paused', '以图搜图')).toContain('完成后才能以图搜图')
    expect(unavailableText('engine', null)).toBe('内容识别无法使用，只显示文件名或正文中含有搜索词的结果。')
    expect(unavailableText(null, '以图搜图')).toBe('内容识别无法使用，无法以图搜图。请重新启动 Rummage。')
    expect(unavailableText('query_too_long', null)).toBe('搜索内容过长，只显示文件名或正文中含有搜索词的结果。请缩短后再试。')
    expect(unavailableText('query_too_long', '以图搜图')).toBe('搜索内容过长，请缩短后再试。')
    expect(unavailableText('no_mmproj', '找相似')).toBe('尚未下载图片与音视频模型，无法找相似。可在「设置 → 模型」中下载或导入。')
  })
})

describe('matchText', () => {
  it('列出命中的通道', () => {
    const hit: SearchHit = {
      fileId: 1, path: 'D:\a.md', name: 'a.md', kind: 'document', size: 1, mtime: 0, cloud: false, offline: false, missing: false,
      score: 0, cosine: null, z: null, matchPct: null, lowRelevance: false, segments: [], nameHit: false, textSegments: [], width: null, height: null, snippet: null,
    }
    expect(matchText({ ...hit, textSegments: [3] })).toBe('正文含搜索词')
    expect(matchText({ ...hit, cosine: 0.5, nameHit: true, textSegments: [3, 1] })).toBe('内容相近 · 文件名含搜索词 · 正文含搜索词')
  })
})

describe('matchPercent', () => {
  const hit: SearchHit = {
    fileId: 1, path: 'D:\a.md', name: 'a.md', kind: 'document', size: 1, mtime: 0, cloud: false, offline: false, missing: false,
    score: 0, cosine: 0.4, z: 3.4, matchPct: null, lowRelevance: false, segments: [], nameHit: false, textSegments: [], width: null, height: null, snippet: null,
  }

  it('显示核心算好的百分比', () => {
    expect(matchPercent({ ...hit, matchPct: 70 })).toBe('70%')
    expect(matchPercent({ ...hit, matchPct: 0 })).toBe('0%')
  })

  it('没有校准分数时不显示，即使有余弦', () => {
    expect(matchPercent(hit)).toBeNull()
  })
})

describe('lowRelevanceText', () => {
  it('带上折叠的结果数', () => {
    expect(lowRelevanceText(12)).toBe('匹配度较低的结果（12）')
  })
})

describe('trimPeriod', () => {
  it('只有一句时去掉句末句号', () => {
    expect(trimPeriod('已是最新版本。')).toBe('已是最新版本')
    expect(trimPeriod('无法打开这个文件，请稍后再试。')).toBe('无法打开这个文件，请稍后再试')
  })

  it('多句或没有句号时原样', () => {
    expect(trimPeriod('文件不存在。请检查路径。')).toBe('文件不存在。请检查路径。')
    expect(trimPeriod('文件不存在！请检查路径。')).toBe('文件不存在！请检查路径。')
    expect(trimPeriod('已是最新版本')).toBe('已是最新版本')
    expect(trimPeriod('请稍后再试？')).toBe('请稍后再试？')
  })
})
