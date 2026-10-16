import { useState } from 'react'
import { useAppFonts } from './hooks/useAppFonts'
import { useCoreStatus } from './hooks/useCoreStatus'
import { useAutoUpdateCheck, useReleaseUpdates } from './hooks/useReleaseUpdates'
import { useSetup } from './hooks/useSetup'
import { isTauri } from './lib/platform'
import CoreStatusNotice from './components/CoreStatusNotice'
import DataDirNotices from './components/DataDirNotices'
import DbErrorDialog from './components/DbErrorDialog'
import Onboarding from './components/onboarding/Onboarding'
import SearchPage from './components/SearchPage'
import Toaster from './components/Toast'
import TrayHint from './components/TrayHint'
import UpdateModal from './components/UpdateModal'

/**
 * 未完成引导时显示引导，否则显示搜索页；
 * 无论哪个页面，都显示启动时的数据目录提示、“索引数据库无法打开”和第一次关闭窗口时的托盘提示。
 * 后台服务连上后自动检查一次更新（可在设置中关闭；检查要用后台服务中的代理设置）
 */
export default function App() {
  const coreStatus = useCoreStatus()
  const fonts = useAppFonts(coreStatus === 'running')
  const setup = useSetup(coreStatus === 'running')
  const autoCheck = useReleaseUpdates((s) => s.autoCheck)
  // 显示过搜索页后不再退回整页提示：在设置里点“重试”时不关掉设置、不清空搜索
  const [searchShown, setSearchShown] = useState(false)
  useAutoUpdateCheck(isTauri && autoCheck && coreStatus === 'running')

  const page = () => {
    if (isTauri && setup.status && !setup.status.onboarded) {
      return (
        <Onboarding
          status={setup.status}
          coreStatus={coreStatus}
          onStatus={setup.update}
          onDone={() => setup.update((s) => ({ ...s, onboarded: true }))}
        />
      )
    }
    // 还不知道是否要引导时只显示状态，避免搜索页一闪而过；核心无法启动或读取失败时显示搜索页，
    // 状态在结果区显示，读取失败的原因在设置的设备、模型、网络页显示
    if (isTauri && !searchShown && !setup.status && !setup.error && coreStatus !== 'failed') return <CoreStatusNotice status={coreStatus} className="min-h-screen px-6" />
    if (!searchShown) setSearchShown(true)
    return (
      <SearchPage
        coreStatus={coreStatus}
        setup={setup.status}
        setupError={setup.error}
        onSetup={setup.update}
        onSetupRetry={setup.refresh}
        appearance={fonts.appearance}
        onAppearance={fonts.update}
      />
    )
  }

  return (
    <>
      {page()}
      <DataDirNotices />
      <DbErrorDialog connected={coreStatus === 'running'} />
      <TrayHint />
      <UpdateModal />
      <Toaster />
    </>
  )
}
