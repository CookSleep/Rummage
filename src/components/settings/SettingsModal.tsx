import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ComponentType, type SVGProps } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { Appearance, CoreStatus, Notice, ParamDef, ParamGroup, ParamValue, ParamValues, ProxySetting, SetupStatus } from '../../types'
import { errorMessage } from '../../lib/errors'
import { setAutoCheck, useReleaseUpdates } from '../../hooks/useReleaseUpdates'
import { isAndroid, isTauri } from '../../lib/platform'
import { dismissAllTooltips } from '../../lib/tooltipDismiss'
import { useBackLayer } from '../../hooks/useBackLayer'
import { useCoreEvent } from '../../hooks/useCoreEvent'
import { renderMessage } from '../ConfirmDialog'
import CoreStatusNotice from '../CoreStatusNotice'
import { CloseIcon, CubeIcon, DatabaseIcon, FolderIcon, GaugeIcon, GlobeIcon, GpuIcon, InfoIcon, PaletteIcon, SearchIcon, SettingsIcon, SlidersIcon, SparklesIcon, StorageIcon, WarningIcon } from '../icons'
import LoadError from '../LoadError'
import ProxyForm from '../onboarding/ProxyForm'
import AboutTab from './AboutTab'
import AgentTab from './AgentTab'
import AppearanceTab from './AppearanceTab'
import DataTab from './DataTab'
import DeviceTab from './DeviceTab'
import FoldersTab from './FoldersTab'
import ModelsTab from './ModelsTab'
import ParamsTab from './ParamsTab'
import Toggle from './Toggle'
import { NOTICE_BOX, ROW_DESC, ROW_LABEL, SECTION_NOTICE, SECTION_TITLE } from './styles'

export type TabId = 'folders' | 'search' | 'index' | 'resource' | 'device' | 'models' | 'network' | 'appearance' | 'data' | 'advanced' | 'agent' | 'about'

const ALL_TABS: { id: TabId; label: string; icon: ComponentType<SVGProps<SVGSVGElement>>; group?: ParamGroup }[] = [
  { id: 'folders', label: '文件夹', icon: FolderIcon },
  { id: 'search', label: '搜索', icon: SearchIcon, group: 'Search' },
  { id: 'index', label: '索引', icon: DatabaseIcon, group: 'Index' },
  { id: 'resource', label: '资源', icon: GaugeIcon, group: 'Resource' },
  { id: 'device', label: '设备', icon: GpuIcon },
  { id: 'models', label: '模型', icon: CubeIcon },
  { id: 'network', label: '网络', icon: GlobeIcon },
  { id: 'appearance', label: '外观', icon: PaletteIcon },
  { id: 'data', label: '数据', icon: StorageIcon },
  { id: 'advanced', label: '高级', icon: SlidersIcon, group: 'Advanced' },
  { id: 'agent', label: 'Agent', icon: SparklesIcon },
  { id: 'about', label: '关于', icon: InfoIcon },
]

// 命令行与 MCP 只在 Windows 上提供，Android 没有“Agent”页
const TABS = ALL_TABS.filter((t) => !isAndroid || t.id !== 'agent')

const NOTICE_TEXT: Record<Notice, string> = {
  db_restored: '上次更新索引数据库时出错，已从备份恢复。',
}

interface Props {
  onClose: () => void
  coreStatus: CoreStatus
  /** 还没取到过引导状态时为空；后台服务断开后保留上次的值（`useSetup`） */
  setup: SetupStatus | null
  /** 已连上但读取引导状态失败的原因 */
  setupError: string
  onSetup: (update: (s: SetupStatus) => SetupStatus) => void
  onSetupRetry: () => void
  appearance: Appearance
  onAppearance: (next: Appearance) => Promise<void>
  /** 打开时显示的页，默认“文件夹” */
  initialTab?: TabId
  /** 打开时滚动到的参数，须在 `initialTab` 页中；切换页后不再滚动 */
  initialParam?: string
}

/**
 * 设置弹窗：左侧分页，窄窗口时分页移到顶部横向滚动；
 * 各页分别记住滚动位置。参数值来自 `get_params`，之后跟随 `ParamsChanged` 事件；参数页由参数表生成（`param_defs`）。
 * 顶部显示核心记下的通知（数据库已从备份恢复），用户点“知道了”后清除。后台服务重新连上后重新读取。Esc 或点遮罩关闭
 */
export default function SettingsModal({ onClose, coreStatus, setup, setupError, onSetup, onSetupRetry, appearance, onAppearance, initialTab = 'folders', initialParam }: Props) {
  const [tab, setTab] = useState<TabId>(initialTab)
  const [focus, setFocus] = useState(initialParam)
  const [defs, setDefs] = useState<ParamDef[]>([])
  const [values, setValues] = useState<ParamValues | null>(null)
  const [loadError, setLoadError] = useState('')
  const [notices, setNotices] = useState<Notice[]>([])
  const [noticeError, setNoticeError] = useState('')
  // 切换页签后仍显示“模型”页任务的结果（ModelsTab 的 `acted`）
  const [modelsActed, setModelsActed] = useState(false)
  const autoCheck = useReleaseUpdates((s) => s.autoCheck)
  const onModelsAct = useCallback(() => setModelsActed(true), [])
  const scrollRef = useRef<HTMLDivElement>(null)
  const navRef = useRef<HTMLElement>(null)
  const scrollTops = useRef<Partial<Record<TabId, number>>>({})

  const load = useCallback(() => {
    setLoadError('')
    Promise.all([invoke<ParamDef[]>('param_defs'), invoke<ParamValues>('get_params')])
      .then(([d, v]) => {
        setDefs(d)
        setValues(v)
      })
      .catch((err) => {
        console.warn('无法读取设置：', err)
        setLoadError(errorMessage(err, '无法读取设置，请稍后再试。'))
      })
    invoke<Notice[]>('get_notices')
      .then(setNotices)
      .catch((err) => console.warn('无法读取通知：', err))
  }, [])

  const running = coreStatus === 'running'
  const offline = isTauri && !running
  useEffect(() => {
    if (isTauri && running) load()
  }, [running, load])

  useCoreEvent((ev) => {
    if (ev.type === 'ParamsChanged') setValues(ev.values)
  })

  // Android 返回键：设置里打开的确认框、下拉菜单等后登记，先关闭它们
  useBackLayer(onClose)

  // 确认框、字体许可、更新日志在捕获阶段处理 Esc 并阻止传播，这里只在它们没有打开时收到
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return
      e.preventDefault()
      onClose()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])

  // 打开时收起所有说明气泡，背后的搜索页也不随滚轮滚动
  useEffect(() => {
    dismissAllTooltips()
    // 页面在文档上滚动（同 SearchPage），锁 html 而不是 body
    const prev = document.documentElement.style.overflow
    document.documentElement.style.overflow = 'hidden'
    return () => {
      document.documentElement.style.overflow = prev
    }
  }, [])

  useLayoutEffect(() => {
    if (scrollRef.current) scrollRef.current.scrollTop = scrollTops.current[tab] ?? 0
  }, [tab])

  // 窄窗口下分页横向滚动，打开时把当前页的按钮滚到中间；靠近两端居中不了时由浏览器限制在滚动范围内，停在最近的位置。
  // 用 offsetLeft（nav 为定位元素）而不用 getBoundingClientRect：打开动画带缩放
  useLayoutEffect(() => {
    const nav = navRef.current
    const el = nav?.querySelector<HTMLElement>('[aria-current="page"]')
    if (!nav || !el) return
    nav.scrollLeft = el.offsetLeft + el.offsetWidth / 2 - nav.clientWidth / 2
  }, [])

  /** 返回参数已保存、但需要告诉用户的提示 */
  const setParam = useCallback(async (key: string, value: ParamValue) => {
    const warning = await invoke<string | null>('set_param', { key, value })
    // 事件随后也会带来全部参数；先更新，避免控件闪回旧值
    setValues((v) => v && { ...v, [key]: value })
    return warning
  }, [])

  const dismiss = (n: Notice) => {
    setNoticeError('')
    invoke('dismiss_notice', { notice: n })
      .then(() => setNotices((list) => list.filter((x) => x !== n)))
      .catch((err) => {
        console.warn('无法清除通知：', err)
        setNoticeError(errorMessage(err, '无法清除这条提示，请重试。'))
      })
  }

  const group = TABS.find((t) => t.id === tab)?.group
  const onProxy = (proxy: ProxySetting) => onSetup((s) => ({ ...s, proxy }))

  const body = () => {
    // 外观、数据、网络页只有部分依赖后台服务，不可用时只在那一分区显示它的状态；其余各页整页显示
    if (tab === 'about') return <AboutTab />
    if (tab === 'agent') return <AgentTab />
    if (tab === 'appearance') return <AppearanceTab appearance={appearance} onChange={onAppearance} offline={offline} coreStatus={coreStatus} />
    if (tab === 'data') return <DataTab defs={defs} offline={offline} coreStatus={coreStatus} />
    if (tab === 'network') {
      return (
        <div className="space-y-8">
          <section className="space-y-4">
            <h4 className={SECTION_TITLE}>网络</h4>
            {offline ? (
              <CoreStatusNotice status={coreStatus} className={SECTION_NOTICE} />
            ) : setup ? (
              <ProxyForm saved={setup.proxy} onSaved={onProxy} label="网络代理" />
            ) : (
              setupError && <LoadError message={setupError} onRetry={onSetupRetry} />
            )}
          </section>
          {/* 开关存在本地，不依赖后台服务 */}
          <section className="space-y-4">
            <h4 className={SECTION_TITLE}>更新</h4>
            <div>
              <div className="mb-1 flex items-center justify-between gap-3">
                <span className={ROW_LABEL}>自动检查更新</span>
                <Toggle checked={autoCheck} onChange={setAutoCheck} label="自动检查更新" />
              </div>
              <div className={ROW_DESC}>{renderMessage('开启后，启动时会从 GitHub 获取版本信息，有新版本时在右上角显示 NEW 提示。关闭后可在「设置 → 关于」中手动检查更新。')}</div>
            </div>
          </section>
        </div>
      )
    }
    if (offline) return <CoreStatusNotice status={coreStatus} className="h-full" />
    if ((tab === 'device' || tab === 'models') && !setup) return setupError ? <LoadError message={setupError} onRetry={onSetupRetry} /> : null
    if (tab === 'device' && setup) return <DeviceTab setup={setup} values={values} />
    if (tab === 'models' && setup) return <ModelsTab setup={setup} acted={modelsActed} onAct={onModelsAct} />
    if (loadError) return <LoadError message={loadError} onRetry={load} />
    if (!values) return null
    if (group) return <ParamsTab group={group} defs={defs} values={values} setParam={setParam} onDevice={() => setTab('device')} focus={focus} />
    return <FoldersTab values={values} setParam={setParam} setup={setup} onModels={() => setTab('models')} />
  }

  return (
    <div className="fixed inset-0 z-[70] flex items-center justify-center modal-pad">
      <div className="absolute inset-0 animate-overlay-in bg-black/30 backdrop-blur-sm" onClick={onClose} />
      <div
        role="dialog"
        aria-modal="true"
        aria-label="设置"
        className="relative z-10 flex h-[85vh] max-h-full w-full max-w-3xl animate-modal-in flex-col overflow-hidden rounded-3xl border border-white/50 bg-white/95 shadow-2xl ring-1 ring-black/5 dark:border-white/[0.08] dark:bg-gray-900/95 dark:ring-white/10 sm:h-[600px]"
      >
        <div className="flex shrink-0 items-center justify-between border-b border-gray-100 p-5 dark:border-white/[0.08]">
          <h3 className="flex items-center gap-2 text-lg font-bold text-gray-800 dark:text-gray-100">
            <SettingsIcon className="h-5 w-5 text-blue-500" />
            设置
          </h3>
          <div className="flex items-center gap-3">
            <span className="select-none font-mono text-sm text-gray-400 dark:text-gray-500">v{__APP_VERSION__}</span>
            <button
              type="button"
              onClick={onClose}
              className="rounded-full p-1 text-gray-400 transition hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-white/[0.06] dark:hover:text-gray-200"
              aria-label="关闭"
            >
              <CloseIcon className="h-5 w-5" />
            </button>
          </div>
        </div>

        {notices.map((n) => (
          <div key={n} className={`mx-5 mt-4 shrink-0 ${NOTICE_BOX}`}>
            <WarningIcon className="mt-px h-4 w-4 flex-shrink-0" />
            <span className="flex-1">
              {NOTICE_TEXT[n]}
              {noticeError && <span data-selectable-text className="mt-1 block text-red-500 dark:text-red-400">{noticeError}</span>}
            </span>
            <button type="button" className="flex-shrink-0 font-medium hover:underline" onClick={() => dismiss(n)}>
              知道了
            </button>
          </div>
        ))}

        <div className="flex min-h-0 flex-1 flex-col sm:flex-row">
          <div className="flex w-full shrink-0 flex-col border-b border-gray-100 bg-gray-50/50 dark:border-white/[0.08] dark:bg-white/[0.02] sm:w-48 sm:border-b-0 sm:border-r">
            <nav ref={navRef} className="relative flex flex-1 space-x-1 overflow-x-auto p-3 sm:flex-col sm:space-x-0 sm:space-y-1 sm:overflow-y-auto">
              {TABS.map((t) => (
                <button
                  key={t.id}
                  type="button"
                  aria-current={tab === t.id ? 'page' : undefined}
                  onClick={() => {
                    setTab(t.id)
                    setFocus(undefined)
                  }}
                  className={`flex flex-shrink-0 items-center gap-2.5 whitespace-nowrap rounded-xl px-3 py-2.5 text-sm transition-colors ${tab === t.id ? 'bg-white font-medium text-blue-600 shadow-sm dark:bg-white/[0.08] dark:text-blue-400' : 'text-gray-600 hover:bg-gray-100/80 dark:text-gray-400 dark:hover:bg-white/[0.04]'}`}
                >
                  <t.icon className="h-4 w-4" />
                  {t.label}
                </button>
              ))}
            </nav>
          </div>

          <div className="relative flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
            <div
              ref={scrollRef}
              onScroll={(e) => {
                scrollTops.current[tab] = e.currentTarget.scrollTop
              }}
              className="flex-1 overflow-y-auto overscroll-contain p-5 [scrollbar-gutter:stable_both-edges] sm:p-6"
            >
              {body()}
            </div>
          </div>
        </div>
      </div>
    </div>
  )
}
