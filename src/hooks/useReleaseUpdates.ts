import { useEffect } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { create } from 'zustand'
import type { ReleaseCache, ReleaseInfo } from '../lib/releaseUpdates'
import { errorMessage } from '../lib/errors'
import { readLocal, writeLocal } from '../lib/storage'
import { isReleaseCacheFresh, mergeReleasePage, parseReleaseCache, parseReleasePage, RELEASE_CACHE_TTL_MS } from '../lib/releaseUpdates'

const CACHE_KEY = 'rummage.releaseCache'
const IGNORED_KEY = 'rummage.ignoredRelease'
const AUTO_CHECK_KEY = 'rummage.autoCheckUpdate'

interface ReleaseUpdatesState {
  releases: ReleaseInfo[]
  pages: number
  hasMore: boolean
  ignoredVersion: string
  /** 启动时自动检查更新（存在本地，默认开启） */
  autoCheck: boolean
  modalOpen: boolean
  // 最近一次检查失败的原因，成功后清空
  checkError: string
}

/** 版本列表与更新日志弹窗的状态 */
export const useReleaseUpdates = create<ReleaseUpdatesState>(() => ({
  releases: [],
  pages: 0,
  hasMore: false,
  ignoredVersion: readLocal(IGNORED_KEY) ?? '',
  autoCheck: readLocal(AUTO_CHECK_KEY) !== '0',
  modalOpen: false,
  checkError: '',
}))

async function fetchReleasePage(page: number) {
  // 由 Rust 侧发出（src-tauri/src/releases.rs），走网络代理设置，CSP 不必放开 GitHub
  const text = await invoke<string>('fetch_releases', { page })
  try {
    return parseReleasePage(JSON.parse(text))
  } catch {
    throw new Error('版本信息的格式不正确。')
  }
}

function applyCache(cache: ReleaseCache) {
  useReleaseUpdates.setState({ releases: cache.releases, pages: cache.pages, hasMore: cache.hasMore, checkError: '' })
}

let checking: Promise<ReleaseInfo | undefined> | null = null

/** 获取最新的版本列表并返回最新版本；缓存在 ttl 内直接复用（默认一小时）。同时发起的检查共用一次请求 */
export function checkForUpdates(ttl = RELEASE_CACHE_TTL_MS) {
  checking ??= (async () => {
    const cache = parseReleaseCache(readLocal(CACHE_KEY))
    if (isReleaseCacheFresh(cache, Date.now(), ttl)) {
      applyCache(cache)
      return cache.releases[0]
    }
    try {
      const next = mergeReleasePage(null, 1, await fetchReleasePage(1), Date.now())
      writeLocal(CACHE_KEY, JSON.stringify(next))
      applyCache(next)
      return next.releases[0]
    } catch (err) {
      // 失败时退回过期缓存，避免弹窗中已显示的内容消失
      if (cache) applyCache(cache)
      useReleaseUpdates.setState({ checkError: errorMessage(err, '检查更新失败，请稍后再试。') })
      throw err
    }
  })().finally(() => {
    checking = null
  })
  return checking
}

let loadingMore: Promise<void> | null = null

/** 加载下一页更早的版本，并写回缓存；某页过滤后没有新增版本时继续加载，直到有新增或没有更多 */
export function loadOlderReleases() {
  loadingMore ??= (async () => {
    // 追加的页沿用第一页的获取时间，缓存整体按第一页过期
    const fetchedAt = parseReleaseCache(readLocal(CACHE_KEY))?.fetchedAt ?? Date.now()
    const count = useReleaseUpdates.getState().releases.length
    for (;;) {
      const state = useReleaseUpdates.getState()
      const next = mergeReleasePage({ fetchedAt, pages: state.pages, hasMore: state.hasMore, releases: state.releases }, state.pages + 1, await fetchReleasePage(state.pages + 1), Date.now())
      writeLocal(CACHE_KEY, JSON.stringify(next))
      applyCache(next)
      // 每轮页码递增，返回不足一页时 hasMore 为假，循环必然结束
      if (next.releases.length > count || !next.hasMore) return
    }
  })().finally(() => {
    loadingMore = null
  })
  return loadingMore
}

export function ignoreReleaseVersion(version: string) {
  writeLocal(IGNORED_KEY, version)
  useReleaseUpdates.setState({ ignoredVersion: version })
}

export function setAutoCheck(autoCheck: boolean) {
  writeLocal(AUTO_CHECK_KEY, autoCheck ? '1' : '0')
  useReleaseUpdates.setState({ autoCheck })
}

export function setUpdateModalOpen(modalOpen: boolean) {
  useReleaseUpdates.setState({ modalOpen })
}

let autoChecked = false

/**
 * 启动时检查一次更新（开发模式下 StrictMode 会执行两次 effect，用模块级标记保证只检查一次）。
 * 请求要从后台服务读代理设置，调用方在后台服务连上后才传入 true
 */
export function useAutoUpdateCheck(enabled: boolean) {
  useEffect(() => {
    if (!enabled || autoChecked) return
    autoChecked = true
    checkForUpdates().catch((err) => console.warn('检查更新失败：', err))
  }, [enabled])
}
