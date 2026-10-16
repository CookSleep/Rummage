import { useEffect, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { ImageQuery, SearchResults, SimilarQuery } from '../types'
import { errorMessage } from '../lib/errors'
import { toSearchFilter, type Filters } from '../lib/filter'
import { isTauri } from '../lib/platform'

/** 输入停止后多久自动搜索 */
const DEBOUNCE_MS = 250

/**
 * 文字查询与图片的文字描述（`note`）防抖后自动搜索，“找相似的”、以图搜图和改了筛选条件时立即搜索；
 * `similar` 优先，其次 `image`，都为空时按文字。以图搜图与可描述的“找相似的”附带 `note`，与图片合成一条查询。
 * 新结果到达前保留上一次的结果，避免输入时页面闪烁；过时的回复直接丢弃。
 * 门槛 `minRelevance` 改了也立即重新搜索：哪些结果折叠、匹配度多少都由核心按门槛判定。
 * 后台服务断开期间不搜索、保留现有结果，重新连上后立即重新搜索
 */
export function useSearch(text: string, note: string, image: ImageQuery | null, similar: SimilarQuery | null, filters: Filters, minRelevance: number | undefined, connected: boolean) {
  const [results, setResults] = useState<SearchResults | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const seq = useRef(0)
  // 只是筛选条件、图片变了时不必等防抖
  const lastText = useRef(text)
  const lastNote = useRef(note)

  useEffect(() => {
    const id = ++seq.current
    const q = text.trim()
    const typed = lastText.current !== text || lastNote.current !== note
    lastText.current = text
    lastNote.current = note
    if (!image && !similar && !q) {
      setResults(null)
      setError(null)
      setLoading(false)
      return
    }
    if (!isTauri) return
    if (!connected) {
      setLoading(false)
      return
    }
    const run = () => {
      setLoading(true)
      const query = similar
        ? { similar: { fileId: similar.fileId, segmentId: similar.segmentId }, text: similar.describable ? note.trim() : '' }
        : image
          ? { imageToken: image.token, text: note.trim() }
          : { text: q }
      invoke<SearchResults>('search', { ...query, filter: toSearchFilter(filters) })
        .then((r) => {
          if (id !== seq.current) return
          setResults(r)
          setError(null)
        })
        .catch((err) => {
          if (id !== seq.current) return
          console.warn('搜索失败：', err)
          setResults(null)
          setError(errorMessage(err, '搜索失败，请稍后再试。'))
        })
        .finally(() => {
          if (id === seq.current) setLoading(false)
        })
    }
    if (!typed) {
      run()
      return
    }
    const timer = setTimeout(run, DEBOUNCE_MS)
    return () => clearTimeout(timer)
    // 以图搜图与找相似不折叠，匹配度也不跟随门槛，门槛变化时不必重新搜索
  }, [text, note, image, similar, filters, similar || image ? null : minRelevance, connected])

  return { results, error, loading }
}
