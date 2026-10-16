import { useEffect, useState, type ReactNode } from 'react'
import { checkForUpdates, setUpdateModalOpen, useReleaseUpdates } from '../../hooks/useReleaseUpdates'
import { errorMessage } from '../../lib/errors'
import { openExternal } from '../../lib/platform'
import { compareVersions, ISSUES_URL, LICENSE_URL, MANUAL_CHECK_MIN_INTERVAL_MS, REPO_URL } from '../../lib/releaseUpdates'
import { ChatIcon, ChevronRightIcon, ClockIcon, DocumentIcon, ExternalLinkIcon, GithubIcon, HeartIcon, ResetIcon, ScaleIcon } from '../icons'
import { CARD_ROW, CARD_ROW_HOVER, ICON_TILE } from '../onboarding/styles'
import { HoverTip } from '../Tip'
import { showToast } from '../Toast'
import FontLicensesModal from './FontLicensesModal'

interface AboutRowProps {
  icon: ReactNode
  title: string
  subtitle?: string
  right?: ReactNode
  onClick: () => void
  highlight?: boolean
}

/** 列表中的一行；外链也是按钮，点击时交给系统浏览器 */
function AboutRow({ icon, title, subtitle, right, onClick, highlight }: AboutRowProps) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`${CARD_ROW} ${highlight ? 'text-left outline-none transition-[background-color] bg-blue-500/[0.08] hover:bg-blue-500/[0.12] focus-visible:bg-blue-500/[0.12] dark:bg-blue-500/[0.12] dark:hover:bg-blue-500/[0.16] dark:focus-visible:bg-blue-500/[0.16]' : CARD_ROW_HOVER}`}
    >
      <span className={highlight ? 'flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-blue-500 text-white' : ICON_TILE}>{icon}</span>
      <span className="min-w-0 flex-1">
        <span className={`block text-sm ${highlight ? 'font-medium text-blue-600 dark:text-blue-400' : 'text-gray-800 dark:text-gray-100'}`}>{title}</span>
        {subtitle && <span className="mt-0.5 block text-xs text-gray-500 dark:text-gray-400">{subtitle}</span>}
      </span>
      {right}
    </button>
  )
}

/** 设置页样式的卡片：标题行下是以细线分隔的若干行 */
function AboutCard({ icon, title, children }: { icon?: ReactNode; title?: string; children: ReactNode }) {
  return (
    <div className="overflow-hidden rounded-2xl border border-transparent bg-black/[0.04] dark:bg-white/[0.06]">
      {title && (
        <div className="flex items-center gap-2 px-4 pb-1 pt-4 text-gray-700 dark:text-gray-300">
          {icon}
          <h4 className="text-sm font-bold text-gray-800 dark:text-gray-100">{title}</h4>
        </div>
      )}
      <div className="divide-y divide-black/[0.05] dark:divide-white/[0.06]">{children}</div>
    </div>
  )
}

const CHEVRON = <ChevronRightIcon className="h-4 w-4 shrink-0 text-gray-400 dark:text-gray-500" />
const EXTERNAL = <ExternalLinkIcon className="h-4 w-4 shrink-0 text-gray-400 dark:text-gray-500" />
const HEADER_LINK = 'rounded outline-none underline-offset-2 hover:underline focus-visible:underline focus-visible:ring-2 focus-visible:ring-blue-500/40'

/**
 * “关于”页（版本号在“检查更新”一行）：应用与作者、版本与更新（检查更新、更新日志）、支持与反馈、
 * 开源协议与字体许可。仓库地址集中在 `src/lib/releaseUpdates.ts`；外部链接一律交给系统浏览器
 */
export default function AboutTab() {
  const latest = useReleaseUpdates((s) => s.releases[0])
  const checkError = useReleaseUpdates((s) => s.checkError)
  const [checking, setChecking] = useState(false)
  const [licenses, setLicenses] = useState(false)

  // 打开时取一次版本信息，一小时内复用缓存；失败的原因显示在“检查更新”一行
  useEffect(() => {
    checkForUpdates().catch((err) => console.warn('检查更新失败：', err))
  }, [])

  const handleCheck = async () => {
    if (checking) return
    setChecking(true)
    try {
      // 手动检查只复用一分钟内的缓存；有新版本时直接打开更新日志
      const next = await checkForUpdates(MANUAL_CHECK_MIN_INTERVAL_MS)
      if (!next) showToast('暂无已发布的版本')
      else if (compareVersions(next.version, __APP_VERSION__) > 0) setUpdateModalOpen(true)
      else showToast('已是最新版本', 'success')
    } catch (err) {
      showToast(`检查更新失败：${errorMessage(err, '请稍后再试')}`, 'error')
    } finally {
      setChecking(false)
    }
  }

  const hasUpdate = Boolean(latest && compareVersions(latest.version, __APP_VERSION__) > 0)
  const checkStatus = checking ? (
    <span className="shrink-0 text-xs text-gray-500 dark:text-gray-400">正在检查</span>
  ) : checkError ? (
    <span className="shrink-0 text-xs text-red-500 dark:text-red-400">检查失败</span>
  ) : latest ? (
    <span className="shrink-0 text-xs text-gray-500 dark:text-gray-400">已是最新版本</span>
  ) : null

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-4 rounded-2xl border border-transparent bg-black/[0.04] p-4 dark:bg-white/[0.06]">
        <div className="min-w-0 flex-1">
          <h4 className="text-base font-bold text-gray-800 dark:text-gray-100">
            <button type="button" onClick={() => openExternal(REPO_URL)} className={HEADER_LINK}>
              Rummage
            </button>
          </h4>
          <p className="mt-0.5 text-[13px] text-gray-500 dark:text-gray-400">
            <button
              type="button"
              onClick={() => openExternal('https://github.com/CookSleep')}
              className={`${HEADER_LINK} transition-colors hover:text-gray-700 focus-visible:text-gray-700 dark:hover:text-gray-200 dark:focus-visible:text-gray-200`}
            >
              @CookSleep
            </button>
          </p>
          <p data-selectable-text className="mt-3 text-xs leading-relaxed text-gray-500 dark:text-gray-400">
            本项目的成长离不开每一位用户的使用、反馈、贡献与支持，感谢一路有你。
          </p>
        </div>
        <button
          type="button"
          onClick={() => openExternal(REPO_URL)}
          aria-label="GitHub 仓库"
          className="shrink-0 rounded-lg p-1 text-gray-400 outline-none transition-colors hover:text-gray-700 focus-visible:text-gray-700 focus-visible:ring-2 focus-visible:ring-blue-500/40 dark:text-gray-500 dark:hover:text-gray-200 dark:focus-visible:text-gray-200"
        >
          <GithubIcon className="h-6 w-6" />
          <HoverTip label="GitHub 仓库" />
        </button>
      </div>

      <AboutCard icon={<ResetIcon className="h-4 w-4" />} title="版本与更新">
        {hasUpdate && !checking ? (
          <AboutRow
            highlight
            icon={<ResetIcon className="h-4 w-4" />}
            title="发现新版本"
            subtitle={`当前版本 v${__APP_VERSION__}`}
            onClick={() => setUpdateModalOpen(true)}
            right={(
              <span className="flex shrink-0 items-center gap-1">
                <span className="rounded-full bg-blue-500 px-2 py-0.5 text-xs font-medium text-white">{latest?.tag}</span>
                <ChevronRightIcon className="h-4 w-4 text-blue-500 dark:text-blue-400" />
              </span>
            )}
          />
        ) : (
          <AboutRow
            icon={<ResetIcon className={`h-4 w-4 ${checking ? 'animate-spin' : ''}`} />}
            title="检查更新"
            subtitle={checkError && !checking ? checkError : `当前版本 v${__APP_VERSION__}`}
            onClick={() => void handleCheck()}
            right={checkStatus}
          />
        )}
        <AboutRow icon={<ClockIcon className="h-4 w-4" />} title="更新日志" onClick={() => setUpdateModalOpen(true)} right={CHEVRON} />
      </AboutCard>

      <AboutCard icon={<HeartIcon className="h-4 w-4" />} title="支持与反馈">
        <AboutRow icon={<ChatIcon className="h-4 w-4" />} title="反馈问题" onClick={() => openExternal(ISSUES_URL)} right={EXTERNAL} />
        <AboutRow icon={<HeartIcon className="h-4 w-4" />} title="赞助作者" onClick={() => openExternal('https://www.ifdian.net/a/cooksleep')} right={EXTERNAL} />
      </AboutCard>

      <AboutCard>
        <AboutRow icon={<ScaleIcon className="h-4 w-4" />} title="开源协议" subtitle="本软件以 MIT 协议开源" onClick={() => openExternal(LICENSE_URL)} right={EXTERNAL} />
        <AboutRow
          icon={<DocumentIcon className="h-4 w-4" />}
          title="第三方许可"
          subtitle="本软件使用的字体与依赖库的许可"
          onClick={() => setLicenses(true)}
          right={CHEVRON}
        />
      </AboutCard>

      {licenses && <FontLicensesModal onClose={() => setLicenses(false)} />}
    </div>
  )
}
