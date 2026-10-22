import { useCallback, useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { CoreStatus, ParamValues, RootInfo, SetupStatus } from '../../types'
import { errorMessage } from '../../lib/errors'
import { KIND_PARAMS, type Kinds, kindsOf, needsMmproj } from '../../lib/kinds'
import { selftestText } from '../../lib/device'
import { isAndroid } from '../../lib/platform'
import { useBackLayer } from '../../hooks/useBackLayer'
import { useConfigImport } from '../../hooks/useConfigImport'
import { useStorageAccess } from '../../hooks/useStorageAccess'
import CoreStatusNotice from '../CoreStatusNotice'
import LoadError from '../LoadError'
import { showToast } from '../Toast'
import BackgroundRunRow from '../settings/BackgroundRunRow'
import EstimateStep from './EstimateStep'
import FoldersStep from './FoldersStep'
import KindsStep from './KindsStep'
import { anyModelReady, requiredModel, rowFile } from './ModelDownload'
import ModelStep from './ModelStep'
import SelftestStep from './SelftestStep'
import { BUTTON, PRIMARY_BUTTON } from './styles'

const STEPS = ['选择文件夹', '文件类型', '下载模型', '检测设备', '预估'] as const

interface Props {
  status: SetupStatus
  coreStatus: CoreStatus
  /** 代理设置保存后更新引导状态中的代理 */
  onStatus: (update: (s: SetupStatus) => SetupStatus) => void
  onDone: () => void
}

/**
 * 首次使用引导：选文件夹 → 选类型 → 下载模型 → 检测设备 → 预估后开始建索引。
 * 每一步的必需项完成后才能进入下一步：至少一个文件夹、至少一种类型、所需的模型都可用且通过完整校验（开着“图片”“音频”或“视频”时还要 mmproj）、
 * 检测完成，或选择只用 CPU（结果过期时可以直接继续，建立索引时后台服务会自动重新检测）。
 * Android 上所需的模型只有一个（`requiredModel`）；只下了 NPU 模型而 NPU 检测未通过时，检测一步请用户回到上一步下载通用模型。
 * 进行到的步骤记在后台服务中（随数据目录迁移），重启后（包括更改数据目录后的重启）从这一步继续；
 * 之前某一步的必需项已不满足（如模型被删除）时退回到那一步。
 * 各步都要用后台服务：不可用时内容区只显示它的状态（步骤条保留），不能前进；重新连上后重新读取。
 * 手机宽度下按钮固定在底部；Android 的返回键回到上一步，最后一步另有“后台运行”（不强制）。
 * 第 1 步可以导入在另一台同类设备上导出的配置：添加文件夹前导入，文件夹步骤直接列出导入的文件夹（`RootsChanged`），
 * 之后各步的文件类型与参数也是导入后的值；还没有建立索引，不确认重建
 */
export default function Onboarding({ status, coreStatus, onStatus, onDone }: Props) {
  // 恢复到上次的步骤之前为空
  const [step, setStep] = useState<number | null>(null)
  const [rootCount, setRootCount] = useState<number | null>(null)
  // 读取过文件夹列表；读取失败时文件夹数未知（为空），不据此退回第 1 步
  const [rootsRead, setRootsRead] = useState(false)
  const [kinds, setKinds] = useState<Kinds | null>(null)
  const [preset, setPreset] = useState<number>()
  const [minFreeGb, setMinFreeGb] = useState(0)
  const [finishing, setFinishing] = useState(false)

  const [paramsError, setParamsError] = useState('')

  const loadParams = useCallback(() => {
    setParamsError('')
    invoke<ParamValues>('get_params')
      .then((v) => {
        setKinds(kindsOf(v))
        setPreset(typeof v.video_preset === 'number' ? v.video_preset : undefined)
        setMinFreeGb(typeof v.min_free_disk_gb === 'number' ? v.min_free_disk_gb : 0)
      })
      .catch((err) => {
        console.warn('无法读取参数：', err)
        setParamsError(errorMessage(err, '无法读取设置，请稍后再试。'))
      })
  }, [])

  const config = useConfigImport(null, loadParams)
  // Android 授权前导入会丢掉配置中的全部文件夹：按钮显示为不可用，点击时提示先授权
  const storage = useStorageAccess()
  const importBlocked = storage.granted === false
  const importConfig = () => {
    if (importBlocked) {
      showToast('请先授予“所有文件访问”权限')
      return
    }
    config.start()
  }

  const running = coreStatus === 'running'
  useEffect(() => {
    if (running) loadParams()
  }, [running, loadParams])

  useEffect(() => {
    if (!running) return
    invoke<RootInfo[]>('list_roots')
      .then((roots) => setRootCount(roots.length))
      .catch((err) => console.warn('无法读取文件夹：', err))
      .finally(() => setRootsRead(true))
  }, [running])

  const setKind = (key: keyof Kinds, on: boolean) => {
    if (!kinds) return
    setKinds((k) => k && { ...k, [key]: on })
    // 已保存但需要告诉用户的提示，如未能安排重建索引
    invoke<string | null>('set_param', { key: KIND_PARAMS[key], value: on })
      .then((note) => note && showToast(note))
      .catch((err) => {
        console.warn('无法保存文件类型：', err)
        // 只回滚这一项：期间可能又切换了别的类型
        setKinds((k) => k && { ...k, [key]: !on })
        showToast(errorMessage(err, '设置未能保存，请重试。'), 'error')
      })
  }

  const choosePreset = (value: number) => {
    const prev = preset
    setPreset(value)
    invoke<string | null>('set_param', { key: 'video_preset', value })
      .then((note) => note && showToast(note))
      .catch((err) => {
        console.warn('无法保存视频识别方式：', err)
        setPreset(prev)
        showToast(errorMessage(err, '设置未能保存，请重试。'), 'error')
      })
  }

  const st = status.selftest
  const mmprojOn = Boolean(kinds && needsMmproj(kinds))
  const required = requiredModel(status)
  const reqFile = rowFile(status, required)
  const verifyPending = status.task?.kind === 'verify' && (status.task.state === 'running' || status.task.state === 'done')
  // Android：只要一个模型
  const androidModel = !reqFile.ready
    ? '请先下载或导入模型'
    : reqFile.verified === false
      ? `${required.label}校验未通过，请重新下载或导入`
      : reqFile.verified !== true
        ? verifyPending
          ? '请等待校验完成'
          : '模型尚未通过校验，请重新校验'
        : ''
  /** 第 `i` 步还缺的必需项；没有时为空串 */
  const blockerAt = (i: number) => [
    rootCount === 0 ? '请先添加至少一个文件夹' : '',
    kinds && !kinds.documents && !mmprojOn ? '请至少选择一种文件类型' : '',
    isAndroid
      ? androidModel
      : !status.model.ready
      ? '请先下载或导入模型'
      : status.model.verified === false
        ? '文字模型校验未通过，请重新下载或导入'
        : mmprojOn && !status.mmproj.ready
          ? '尚未下载图片与音视频模型。不需要识别图片、音频和视频时，可以返回上一步取消勾选。'
          : mmprojOn && status.mmproj.verified === false
            ? '图片与音视频模型校验未通过，请重新下载或导入'
            : status.model.verified !== true || (mmprojOn && status.mmproj.verified !== true)
              ? verifyPending
                ? '请等待校验完成'
                : '模型尚未通过校验，请重新校验'
              : '',
    st.running
      ? '请等待检测完成'
      : st.preferred === 'cpu'
        ? isAndroid && !status.model.ready
          ? 'CPU 要用通用模型，请返回上一步下载'
          : ''
        : !st.devices.length
          ? '请开始检测，或选择只用 CPU'
          : isAndroid && !st.devices.some((d) => d.status === 'passed')
            ? status.model.ready
              ? '没有通过检测的设备，请重新检测'
              : 'NPU 检测未通过，请返回上一步下载通用模型'
            : '',
    '',
  ][i]

  // 恢复到记下的步骤：从头检查之前各步的必需项，第一个不满足的就停在那一步
  useEffect(() => {
    if (step !== null || !kinds || !rootsRead) return
    const saved = Math.min(status.step, STEPS.length - 1)
    let target = 0
    while (target < saved && !blockerAt(target)) target++
    setStep(target)
  })

  const go = (next: number) => {
    setStep(next)
    invoke('set_onboarding_step', { step: next }).catch((err) => console.warn('无法记下引导步骤：', err))
  }

  const finish = () => {
    setFinishing(true)
    invoke('finish_setup')
      .then(onDone)
      .catch((err) => {
        console.warn('无法完成引导：', err)
        showToast(errorMessage(err, '无法完成设置，请重试。'), 'error')
      })
      .finally(() => setFinishing(false))
  }

  const blocker = step === null ? '' : blockerAt(step)
  const last = step === STEPS.length - 1
  useBackLayer(() => step !== null && go(step - 1), step !== null && step > 0 && running)

  return (
    <main className="flex min-h-screen justify-center px-4 pt-[calc(1.5rem+var(--safe-top))] sm:px-6 sm:py-10">
      {/* 状态栏的底色：页面滚动时内容不和状态栏图标叠在一起 */}
      <div aria-hidden className="pointer-events-none fixed inset-x-0 top-0 z-30 h-[var(--safe-top)] bg-gray-50/85 backdrop-blur-xl dark:bg-gray-950/85" />
      <div className="flex w-full max-w-xl flex-col">
        <h1 className="text-xl font-bold text-gray-800 dark:text-gray-100">欢迎使用 Rummage</h1>
        <p className="mt-1 text-sm text-gray-500 dark:text-gray-400">完成几项设置后，就能用文字或图片搜索本机文件。</p>

        <ol className="mt-6 flex items-center gap-1.5 text-xs">
          {STEPS.map((label, i) => (
            <li key={label} className="flex flex-1 flex-col gap-1.5">
              <div className={`h-1 rounded-full transition duration-200 ${step !== null && i <= step ? 'bg-blue-500' : 'bg-black/[0.06] dark:bg-white/[0.08]'}`} />
              <span className={i === step ? 'font-medium text-gray-700 dark:text-gray-200' : 'text-gray-400 dark:text-gray-500'}>{label}</span>
            </li>
          ))}
        </ol>

        <section className="mt-5 rounded-3xl border border-white/50 bg-white/90 p-4 shadow-[0_8px_40px_rgb(0,0,0,0.06)] ring-1 ring-black/5 dark:border-white/[0.08] dark:bg-gray-900/90 dark:ring-white/10 sm:p-6">
          {!running && <CoreStatusNotice status={coreStatus} className="py-16" />}
          {running && step === 0 && <FoldersStep onCount={setRootCount} />}
          {running && step === 1 && kinds && (
            <KindsStep kinds={kinds} mmprojBytes={status.mmproj.bytes} mmprojReady={status.mmproj.ready} onChange={setKind} preset={preset} onPreset={choosePreset} />
          )}
          {running && step === 2 && kinds && <ModelStep status={status} mmproj={mmprojOn} onProxy={(proxy) => onStatus((s) => ({ ...s, proxy }))} />}
          {running && step === 3 && <SelftestStep selftest={st} model={isAndroid ? anyModelReady(status) : status.model.ready} cpu={!isAndroid || status.model.ready} text={selftestText(status)} />}
          {running && step === 4 && kinds && <EstimateStep kinds={kinds} preset={preset} selftest={st} minFreeGb={minFreeGb} />}
          {running && step === 4 && isAndroid && (
            <div className="mt-5">
              <BackgroundRunRow />
            </div>
          )}
        </section>

        {running && paramsError && <LoadError message={paramsError} onRetry={loadParams} className="mt-3 px-1" />}

        {step !== null && (
          <footer className="mt-5 flex items-center gap-3 max-sm:sticky max-sm:bottom-0 max-sm:-mx-4 max-sm:mt-auto max-sm:bg-gray-50/85 max-sm:px-4 max-sm:pb-[calc(0.75rem+var(--safe-bottom))] max-sm:pt-5 max-sm:backdrop-blur-xl dark:max-sm:bg-gray-950/85">
            {step > 0 && (
              <button type="button" className={`shrink-0 ${BUTTON}`} disabled={!running} onClick={() => go(step - 1)}>
                上一步
              </button>
            )}
            {step === 0 && (
              <button type="button" className={`shrink-0 ${BUTTON} ${importBlocked ? 'cursor-not-allowed opacity-50' : ''}`} aria-disabled={importBlocked || undefined} disabled={!running || config.busy} onClick={importConfig}>
                {config.busy ? '正在导入' : '导入配置'}
              </button>
            )}
            <span className="ml-auto min-w-0 text-right text-xs text-gray-400 dark:text-gray-500">{blocker}</span>
            {last ? (
              <button type="button" className={`shrink-0 ${PRIMARY_BUTTON}`} disabled={finishing || !running} onClick={finish}>
                开始使用
              </button>
            ) : (
              <button
                type="button"
                className={`shrink-0 ${PRIMARY_BUTTON}`}
                disabled={Boolean(blocker) || !kinds || !running}
                onClick={() => go(step + 1)}
              >
                下一步
              </button>
            )}
          </footer>
        )}
      </div>
    </main>
  )
}
