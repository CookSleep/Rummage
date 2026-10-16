import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import type { Appearance, CoreStatus, ImageQuery, SearchHit, SetupStatus, SimilarQuery } from '../types'
import { errorMessage } from '../lib/errors'
import { useIndexStatus } from '../hooks/useIndexStatus'
import { useParamValues } from '../hooks/useParamValues'
import { setUpdateModalOpen, useReleaseUpdates } from '../hooks/useReleaseUpdates'
import { useSearch } from '../hooks/useSearch'
import { indexDevice } from '../lib/device'
import { DEFAULT_FILTERS } from '../lib/filter'
import { lowRelevanceText, trimPeriod, unavailableText } from '../lib/format'
import { moveSelection, type Direction, type Masonry } from '../lib/masonry'
import { hitPosition, hitRegion } from '../lib/media'
import { isTauri } from '../lib/platform'
import { shouldShowUpdate } from '../lib/releaseUpdates'
import { readLocal, writeLocal } from '../lib/storage'
import { renderMessage } from './ConfirmDialog'
import CoreStatusNotice from './CoreStatusNotice'
import FailedFilesDialog from './FailedFilesDialog'
import IndexHint, { indexHint, nextBase } from './IndexHint'
import FilterBar from './FilterBar'
import DetailPanel, { PANEL_OVERLAY_BELOW, PANEL_WIDTH } from './DetailPanel'
import ResultGrid from './ResultGrid'
import SearchBox from './SearchBox'
import SettingsModal from './settings/SettingsModal'
import { ChevronRightIcon, PanelExpandIcon, SettingsIcon, WarningIcon } from './icons'
import { NOTICE_BOX } from './settings/styles'
import { HoverTip } from './Tip'
import { showToast } from './Toast'
import Wordmark from './Wordmark'

/** 详情栏收起状态的本地存储键，只是每个用户自己的显示偏好 */
const COLLAPSED_KEY = 'rummage.detailCollapsed'

const ARROWS: Record<string, Direction> = {
  ArrowUp: 'up',
  ArrowDown: 'down',
  ArrowLeft: 'left',
  ArrowRight: 'right',
}

interface Props {
  coreStatus: CoreStatus
  /** 引导状态（设置页的设备、模型与代理用）：还没取到过时为空，后台服务断开后保留上次的值 */
  setup: SetupStatus | null
  /** 已连上但读取引导状态失败的原因（`useSetup`） */
  setupError: string
  onSetup: (update: (s: SetupStatus) => SetupStatus) => void
  onSetupRetry: () => void
  appearance: Appearance
  onAppearance: (next: Appearance) => Promise<void>
}

/** 搜索页；右上角进入设置 */
export default function SearchPage({ coreStatus, setup, setupError, onSetup, onSetupRetry, appearance, onAppearance }: Props) {
  const indexStatus = useIndexStatus(coreStatus === 'running')
  // 索引提示有查询时在页头、没有时在窗口底部，是两个组件；本轮基数和失败文件弹窗放在这里，切换查询时不丢。
  // 本轮开始（有待处理的文件）时记下已处理数，处理完清掉；界面中途打开时从打开那一刻算起
  const baseRef = useRef<number | null>(null)
  const failedRef = useRef<number | null>(null)
  if (indexStatus) {
    baseRef.current = nextBase(baseRef.current, failedRef.current, indexStatus)
    failedRef.current = indexStatus.failed
  }
  const [failedOpen, setFailedOpen] = useState(false)
  const [text, setText] = useState('')
  const [image, setImage] = useState<ImageQuery | null>(null)
  // 找相似的：显示为搜索框中的标记，清除后回到原来的文字搜索
  const [similar, setSimilar] = useState<SimilarQuery | null>(null)
  // 以图搜图与图片找相似的时附加的文字描述，换图片或清除时清空
  const [note, setNote] = useState('')
  // 筛选条件在本次运行内保持
  const [filters, setFilters] = useState(DEFAULT_FILTERS)
  // 匹配度 50% 处与折叠用同一个门槛
  const minRelevance = useParamValues(coreStatus === 'running')?.min_relevance as number | undefined
  const { results, error, loading } = useSearch(text, note, image, similar, filters, minRelevance, coreStatus === 'running')
  const allHits = useMemo(() => results?.hits ?? [], [results])
  // 匹配度较低的结果由核心排在最后，默认折叠
  const [showLow, setShowLow] = useState(false)
  const lowCount = useMemo(() => allHits.filter((h) => h.lowRelevance).length, [allHits])
  const mainCount = allHits.length - lowCount
  const hits = useMemo(() => (showLow ? allHits : allHits.slice(0, mainCount)), [allHits, showLow, mainCount])
  const [selected, setSelected] = useState<number | null>(null)
  // 详情栏：宽窗口时有结果就常驻右侧，用户可收起（记住偏好）；窄窗口时选中结果后覆盖整个窗口
  const [collapsed, setCollapsed] = useState(() => readLocal(COLLAPSED_KEY) === '1')
  const [overlayOpen, setOverlayOpen] = useState(false)
  const [wide, setWide] = useState(() => window.innerWidth >= PANEL_OVERLAY_BELOW)
  const [viewportHeight, setViewportHeight] = useState(() => window.innerHeight)
  const [dragging, setDragging] = useState(false)
  const [settingsOpen, setSettingsOpen] = useState(false)
  const latestRelease = useReleaseUpdates((s) => s.releases[0])
  const hasUpdate = useReleaseUpdates((s) => shouldShowUpdate(s.releases[0], __APP_VERSION__, s.ignoredVersion))
  const inputRef = useRef<HTMLInputElement>(null)
  const gridRef = useRef<HTMLDivElement>(null)
  const headerRef = useRef<HTMLElement>(null)
  const layoutRef = useRef<Masonry | null>(null)
  const usedDevice = indexDevice(setup?.selftest)
  const query = similar ? similar.name : image ? image.name : text.trim()
  // 搜索历史：点击空的输入框或在其中按向下键时显示；用户确认了一次文字搜索（回车、选中或打开结果、点选历史）才记录
  const [history, setHistory] = useState<string[]>([])
  const [historyOpen, setHistoryOpen] = useState(false)
  // 文字查询：以图搜图与找相似时为空
  const textQuery = image || similar ? '' : text.trim()
  const textQueryRef = useRef('')
  textQueryRef.current = textQuery
  const rememberedRef = useRef('')
  // 换了查询或清空后，再次搜索同一查询也要更新为最近
  useEffect(() => {
    rememberedRef.current = ''
  }, [text, image, similar])

  const remember = useCallback((q = textQueryRef.current) => {
    if (!q || q === rememberedRef.current) return
    rememberedRef.current = q
    invoke('add_history', { query: q }).catch((err) => console.warn('无法记录搜索历史：', err))
  }, [])

  // 每次显示前重新读取，刚记下的搜索和设置中的清空都能反映出来
  const historyShown = historyOpen && !text && !image && !similar
  useEffect(() => {
    if (!isTauri || !historyShown) return
    invoke<string[]>('get_history')
      .then(setHistory)
      .catch((err) => console.warn('无法读取搜索历史：', err))
  }, [historyShown])

  const pickHistory = (q: string) => {
    setText(q)
    setHistoryOpen(false)
    remember(q)
  }

  const removeHistory = (q: string) => {
    invoke('remove_history', { query: q })
      .then(() => setHistory((h) => h.filter((x) => x !== q)))
      .catch((err) => console.warn('无法删除搜索历史：', err))
  }

  const selectedRef = useRef<number | null>(null)
  selectedRef.current = selected
  const hitsRef = useRef(hits)
  hitsRef.current = hits
  // 上一次布局对应的结果列表
  const layoutHitsRef = useRef<SearchHit[] | null>(null)

  // 把卡片滚动到可视区域内（留出顶部搜索栏）
  const scrollIntoView = useCallback((idx: number) => {
    const box = layoutRef.current?.boxes[idx]
    const grid = gridRef.current
    if (!box || !grid) return
    const top = grid.getBoundingClientRect().top + box.y
    const header = (headerRef.current?.offsetHeight ?? 0) + 8
    const bottom = window.innerHeight - 8
    if (top < header) window.scrollBy({ top: top - header })
    else if (top + box.height > bottom) window.scrollBy({ top: top + box.height - bottom })
  }, [])

  // 同一组结果的布局变化（详情栏收起或展开、窗口缩放）后，选中的卡片可能移出视野，滚回来；新结果的布局不滚动
  const onLayout = useCallback((layout: Masonry) => {
    layoutRef.current = layout
    const same = layoutHitsRef.current === hitsRef.current
    layoutHitsRef.current = hitsRef.current
    if (same && selectedRef.current !== null) scrollIntoView(selectedRef.current)
  }, [scrollIntoView])

  // 单击选中并在详情栏显示，不改变布局，双击就是指针下那张卡片的原生双击；窄窗口时打开盖在网格上的详情栏
  const onSelect = useCallback((idx: number) => {
    setSelected(idx)
    setOverlayOpen(true)
    gridRef.current?.focus({ preventScroll: true })
    remember()
  }, [remember])

  // 宽窄切换时关掉浮层：宽窗口里点过结果后把窗口拖窄，不该直接盖住网格
  useEffect(() => setOverlayOpen(false), [wide])

  const toggleCollapsed = useCallback(() => {
    setCollapsed((c) => {
      writeLocal(COLLAPSED_KEY, c ? '0' : '1')
      return !c
    })
  }, [])

  // 窗口窄时详情栏盖在网格上，宽时常驻、网格让出位置
  useEffect(() => {
    const onResize = () => {
      setWide(window.innerWidth >= PANEL_OVERLAY_BELOW)
      setViewportHeight(window.innerHeight)
    }
    window.addEventListener('resize', onResize)
    return () => window.removeEventListener('resize', onResize)
  }, [])

  // 新结果到达时清除选择并关闭窄窗口的详情栏，旧下标对应的已是别的文件；
  // 在渲染中立即处理，不让旧下标配上新结果渲染一次（会为错的文件请求详情）。
  // 结果的文件与顺序都没变（如后台服务重连后重搜）时保留选择
  const [lastResults, setLastResults] = useState(results)
  if (results !== lastResults) {
    setLastResults(results)
    const a = results?.hits ?? []
    const b = lastResults?.hits ?? []
    const same = a.length === b.length && a.every((h, idx) => h.fileId === b[idx].fileId && h.lowRelevance === b[idx].lowRelevance)
    if (!same) {
      setSelected(null)
      setOverlayOpen(false)
      setShowLow(false)
    }
  }

  // 收起时选中的若是折叠区中的结果，取消选择
  const toggleLow = () => {
    if (showLow && selected !== null && selected >= mainCount) {
      setSelected(null)
      setOverlayOpen(false)
    }
    setShowLow(!showLow)
  }

  // 找相似的只清除标记，回到原来的文字搜索；输入框随之重新出现并自动聚焦
  const clear = () => {
    setOverlayOpen(false)
    setNote('')
    if (similar) {
      setSimilar(null)
      return
    }
    setText('')
    setImage(null)
    inputRef.current?.focus()
  }

  // 用结果最匹配的片段（视频、长图为命中的那一段，文档为命中的片段）找相似的；普通图片（不是长图切片）可以加文字描述，
  // 加描述要重新读图，占位文件和离线设备上的图片读不到，不提供
  const findSimilar = useCallback((idx: number) => {
    const hit = hitsRef.current[idx]
    if (!hit) return
    const seg = hit.segments[0]
    const describable = hit.kind === 'image' && !hit.cloud && !hit.offline && !hitRegion(hit)
    setSimilar({ fileId: hit.fileId, segmentId: seg?.segmentId ?? null, name: hit.name, kind: hit.kind, describable })
    setImage(null)
    setNote('')
    setOverlayOpen(false)
    window.scrollTo({ top: 0 })
  }, [])

  const pickImage = () => {
    invoke<ImageQuery | null>('pick_image')
      .then((img) => {
        if (!img) return
        setSimilar(null)
        setImage(img)
        setNote('')
      })
      .catch((err) => console.warn('无法打开图片选择框：', err))
  }

  // 打开时带上命中位置（音视频的起点、PDF 的页码），默认程序支持时打开到那里
  const run = useCallback((cmd: 'open_file' | 'reveal_file', hit: SearchHit) => {
    remember()
    const args = cmd === 'open_file' ? { fileId: hit.fileId, at: hitPosition(hit) } : { fileId: hit.fileId }
    invoke(cmd, args).catch((err) => {
      console.warn('打开文件失败：', err)
      showToast(errorMessage(err, '无法打开这个文件'), 'error')
    })
  }, [remember])

  const onOpen = useCallback((idx: number) => run('open_file', hits[idx]), [hits, run])

  // 把图片拖进窗口即以图搜图：拖动状态只用于显示提示层；松开后由 Rust 侧把图片换成令牌，以 `image-drop` 推来，
  // 没有图片时为 null（src-tauri/src/image_query.rs）
  useEffect(() => {
    if (!isTauri) return
    // 有弹窗（设置、失败文件等）打开时拖入不搜索，也不显示提示层
    const modalOpen = () => !!document.querySelector('[aria-modal="true"]')
    const unDrag = getCurrentWebview().onDragDropEvent((e) => {
      setDragging(!modalOpen() && (e.payload.type === 'enter' || e.payload.type === 'over'))
    })
    const unDrop = listen<ImageQuery | null>('image-drop', (e) => {
      if (modalOpen()) return
      if (!e.payload) {
        showToast('请拖入图片文件')
        return
      }
      setOverlayOpen(false)
      setSimilar(null)
      setImage(e.payload)
      setNote('')
    })
    return () => {
      void unDrag.then((fn) => fn())
      void unDrop.then((fn) => fn())
    }
  }, [])

  // 窄窗口时覆盖整个窗口的详情页打开着；后面的页面不滚动（文档的滚动条也藏起来）
  const overlay = !wide && overlayOpen && selected !== null
  useEffect(() => {
    if (!overlay) return
    document.documentElement.style.overflow = 'hidden'
    return () => {
      document.documentElement.style.overflow = ''
    }
  }, [overlay])

  // 键盘操作：方向键按卡片位置移动选择，Enter 打开，Ctrl+Enter 在资源管理器中显示，
  // 空格收起或展开详情栏（窄窗口时为关闭或打开），Esc 先关窄窗口盖着的详情栏、再清空搜索；全屏播放时都不处理
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // 全屏播放时按键都交给播放器，不收起详情栏（视频随之卸载会退出全屏）。全屏时空格由这里切换播放：
      // 焦点不在视频上时原生控件不响应，焦点在控件的按钮（如刚点过的全屏按钮）上时空格会按下那个按钮。
      // Esc 只退出全屏（WebView 已自行退出时调用会失败，忽略），不关详情栏、不清空搜索
      const fs = document.fullscreenElement
      if (fs) {
        if (e.key === ' ' && fs instanceof HTMLVideoElement) {
          e.preventDefault()
          if (fs.paused) void fs.play().catch(() => {})
          else fs.pause()
        }
        if (e.key === 'Escape') document.exitFullscreen().catch(() => {})
        return
      }
      // 焦点在播放器上时空格与方向键交给它
      if (e.target instanceof HTMLMediaElement && e.key !== 'Escape') return
      // 有弹窗（设置、失败文件、更新日志、确认框等）打开时按键交给它
      if (document.querySelector('[aria-modal="true"]')) return
      const el = document.activeElement
      const inInput = el === inputRef.current && inputRef.current !== null
      // 焦点在搜索框以外的输入框（如“自定义日期”）中时按键交给它
      if (!inInput && (el instanceof HTMLTextAreaElement || (el instanceof HTMLInputElement && el.type !== 'checkbox'))) return
      // 焦点在按钮、筛选条的复选框和下拉框上时 Enter 和空格交给它们自己
      const onButton = el instanceof HTMLButtonElement || (el instanceof HTMLInputElement && !inInput) || el?.getAttribute('role') === 'button'
      if (e.key === 'Escape') {
        e.preventDefault()
        if (historyShown && history.length) setHistoryOpen(false)
        else if (overlay) setOverlayOpen(false)
        else clear()
        return
      }
      // 搜索框里按 Enter 只记下这次搜索，不打开选中的结果
      if (e.key === 'Enter' && inInput) {
        remember()
        return
      }
      if (!hits.length) return
      if (e.key === 'Enter' && selected !== null && !onButton) {
        e.preventDefault()
        run(e.ctrlKey ? 'reveal_file' : 'open_file', hits[selected])
        return
      }
      const dir = ARROWS[e.key]
      if (dir) {
        // 输入框中左右键移动光标，上键不处理
        if (inInput && dir !== 'down') return
        e.preventDefault()
        const boxes = layoutRef.current?.boxes ?? []
        const next = selected === null ? 0 : moveSelection(boxes, selected, dir)
        // 在一列的最上面再按上键回到输入框；窄窗口的详情页盖住了输入框，不回去
        if (dir === 'up' && next === selected) {
          if (!overlay) inputRef.current?.focus()
          return
        }
        setSelected(next)
        scrollIntoView(next)
        if (inInput) gridRef.current?.focus({ preventScroll: true })
        return
      }
      if (inInput || e.ctrlKey || e.altKey || e.metaKey) return
      if (e.key === ' ' && onButton) return
      if (e.key === ' ') {
        e.preventDefault()
        if (wide) toggleCollapsed()
        else if (selected !== null) setOverlayOpen((o) => !o)
        return
      }
      // 在结果中直接打字时回到输入框继续输入；窄窗口的详情页盖住了输入框，不跳过去
      if (e.key.length === 1 && !overlay) inputRef.current?.focus()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  })

  const offline = isTauri && coreStatus !== 'running'
  const selectedHit = selected !== null ? hits[selected] : undefined
  const hasResults = hits.length > 0 && !(error && query)
  const panelShown = wide ? hasResults && !collapsed : overlayOpen && selectedHit !== undefined
  const unavailable = results?.semanticUnavailable ? unavailableText(results.unavailable, similar !== null ? '找相似' : image !== null ? '以图搜图' : null) : ''

  return (
    <main className="min-h-screen" style={panelShown && wide ? { paddingRight: PANEL_WIDTH } : undefined}>
      {/* 首页像搜索引擎：项目名在上、搜索框居中；开始搜索后搜索框移到顶部 */}
      <header
        ref={headerRef}
        style={{ paddingTop: query ? undefined : '28vh' }}
        className="sticky top-0 z-10 bg-gray-50/85 px-14 pb-3 pt-5 backdrop-blur-xl transition-[padding] duration-300 dark:bg-gray-950/85"
      >
        <div className={`absolute top-5 ${wide && hasResults && collapsed ? 'right-14' : 'right-3'}`}>
          {/* 有新版本时在设置按钮左侧显示 NEW，点击打开更新日志 */}
          {hasUpdate && latestRelease && (
            <button
              type="button"
              onClick={() => setUpdateModalOpen(true)}
              className="absolute right-full top-1 mr-0.5 animate-overlay-in rounded-[4px] border border-red-500/30 bg-red-500 px-1 py-0.5 text-[9px] font-black leading-none text-white shadow-sm transition-all hover:bg-red-600 before:absolute before:-inset-[9px] before:content-['']"
            >
              NEW
              <HoverTip label={`新版本 ${latestRelease.tag}`} />
            </button>
          )}
          <button
            type="button"
            onClick={() => setSettingsOpen(true)}
            aria-label="设置"
            className="rounded-full p-2 text-gray-500 transition duration-200 hover:bg-black/5 hover:text-gray-700 dark:text-gray-400 dark:hover:bg-white/10 dark:hover:text-gray-200"
          >
            <SettingsIcon className="h-5 w-5" />
            <HoverTip label="设置" />
          </button>
        </div>
        <div className="mx-auto w-full max-w-2xl">
          {!query && (
            <h1 className="mb-9 flex justify-center text-gray-800 dark:text-gray-100">
              <Wordmark className="h-14 w-auto" />
            </h1>
          )}
          {/* 提示放在搜索框上方，下方留给筛选条；首页浮在标志与搜索框之间，出现时不推动搜索框 */}
          <div className="relative">
            {((query && indexStatus && (indexHint(indexStatus) || indexStatus.failed > 0)) || (results?.semanticUnavailable && query && allHits.length > 0)) && (
              <div className={`flex flex-col gap-1.5 ${query ? 'mb-2' : 'absolute inset-x-0 bottom-full mb-2'}`}>
                {query && <IndexHint status={indexStatus} base={baseRef.current ?? 0} onFailed={() => setFailedOpen(true)} device={usedDevice} />}
                {results?.semanticUnavailable && query && allHits.length > 0 && (
                  <div className={NOTICE_BOX}>
                    <WarningIcon className="mt-px h-4 w-4 flex-shrink-0" />
                    <span data-selectable-text>{renderMessage(unavailable, 'font-semibold')}</span>
                  </div>
                )}
              </div>
            )}
            <SearchBox
              inputRef={inputRef}
              text={text}
              onText={setText}
              note={note}
              onNote={setNote}
              image={image}
              similar={similar}
              onPickImage={pickImage}
              onClear={clear}
              loading={loading}
              history={historyShown ? history : []}
              onHistoryOpen={setHistoryOpen}
              onPickHistory={pickHistory}
              onRemoveHistory={removeHistory}
            />
          </div>
          <FilterBar filters={filters} onChange={setFilters} refresh={settingsOpen} connected={coreStatus === 'running'} />
        </div>
      </header>

      {/* 首页的索引进度放在窗口底部，不挤占搜索框下方 */}
      {!query && (
        <div className="fixed inset-x-0 bottom-0 z-10 px-14 pb-5 pt-2">
          <div className="mx-auto w-full max-w-2xl">
            <IndexHint status={indexStatus} base={baseRef.current ?? 0} onFailed={() => setFailedOpen(true)} up device={usedDevice} />
          </div>
        </div>
      )}

      <section className="mx-auto w-full max-w-[1600px] px-6 pb-10 pt-2">
        {/* 后台服务不可用时在结果区居中说明，有结果时放在结果上方；断开前的结果保留，断开时的搜索错误不显示 */}
        {offline && <CoreStatusNotice status={coreStatus} className={allHits.length ? 'pb-6 pt-4' : 'py-20'} />}
        {offline && !allHits.length ? null : error && query ? (
          <p className="py-20 text-center text-sm text-red-500">{renderMessage(error, 'font-semibold')}</p>
        ) : !query || (loading && !allHits.length) ? null : results && !allHits.length ? (
          // 没有结果时，语义检索不可用的原因比“没有找到”更有用，放在中间
          <p className="py-20 text-center text-sm text-gray-400 dark:text-gray-500">
            {unavailable ? renderMessage(trimPeriod(unavailable), 'font-semibold') : '没有找到相关内容'}
          </p>
        ) : (
          <>
            {/* 结果都不够相关时照样说没有找到，折叠的结果在下方可以展开 */}
            {mainCount === 0 && <p className="pb-6 pt-14 text-center text-sm text-gray-400 dark:text-gray-500">没有找到相关内容</p>}
            <ResultGrid
              hits={hits}
              selected={selected}
              onSelect={onSelect}
              onOpen={onOpen}
              onSimilar={findSimilar}
              gridRef={gridRef}
              onLayout={onLayout}
              connected={coreStatus === 'running'}
              band={lowCount ? {
                at: mainCount,
                node: (
                  <div className="flex w-full items-center gap-3">
                    <button
                      type="button"
                      onClick={toggleLow}
                      aria-expanded={showLow}
                      className="flex flex-shrink-0 items-center gap-1.5 rounded-lg px-2 py-1 text-sm text-gray-500 transition duration-200 hover:bg-black/5 hover:text-gray-700 dark:text-gray-400 dark:hover:bg-white/10 dark:hover:text-gray-200"
                    >
                      <ChevronRightIcon className={`h-4 w-4 transition-transform duration-200 ${showLow ? 'rotate-90' : ''}`} />
                      {lowRelevanceText(lowCount)}
                    </button>
                    <div className="h-px flex-1 bg-gray-200 dark:bg-white/[0.08]" />
                  </div>
                ),
              } : undefined}
            />
          </>
        )}
      </section>

      {panelShown && (
        <DetailPanel
          hit={selectedHit}
          query={textQuery}
          overlay={!wide}
          viewportHeight={viewportHeight}
          connected={coreStatus === 'running'}
          onHide={wide ? toggleCollapsed : () => setOverlayOpen(false)}
          onOpen={() => selectedHit && run('open_file', selectedHit)}
          onReveal={() => selectedHit && run('reveal_file', selectedHit)}
          onSimilar={() => selected !== null && findSimilar(selected)}
        />
      )}

      {wide && hasResults && collapsed && (
        <button
          type="button"
          onClick={toggleCollapsed}
          aria-label="展开详情栏"
          className="fixed right-3 top-5 z-20 rounded-full border border-gray-200/60 bg-white/90 p-2 text-gray-500 shadow-sm backdrop-blur-xl transition duration-200 hover:text-gray-700 dark:border-white/[0.08] dark:bg-gray-900/90 dark:text-gray-400 dark:hover:text-gray-200"
        >
          <PanelExpandIcon className="h-4 w-4" />
          <HoverTip label="展开详情栏（空格）" />
        </button>
      )}

      {dragging && (
        <div className="pointer-events-none fixed inset-3 z-40 flex items-center justify-center rounded-3xl border-2 border-dashed border-blue-400 bg-blue-500/5 backdrop-blur-sm">
          <p className="text-sm font-medium text-blue-500">松开即可用这张图片搜索</p>
        </div>
      )}

      {failedOpen && <FailedFilesDialog onClose={() => setFailedOpen(false)} />}

      {settingsOpen && (
        <SettingsModal
          onClose={() => setSettingsOpen(false)}
          coreStatus={coreStatus}
          setup={setup}
          setupError={setupError}
          onSetup={onSetup}
          onSetupRetry={onSetupRetry}
          appearance={appearance}
          onAppearance={onAppearance}
        />
      )}

    </main>
  )
}
