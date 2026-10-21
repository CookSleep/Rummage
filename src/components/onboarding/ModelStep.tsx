import { useEffect, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { ProxySetting, SetupStatus } from '../../types'
import { errorMessage } from '../../lib/errors'
import { DownloadIcon } from '../icons'
import { showToast } from '../Toast'
import { isAndroid } from '../../lib/platform'
import { ImportRow, MODELS, ModelRow, modelInfos, modelLabel, PausedProgress, PauseResumeButton, ProxyStatus, requiredModel, rowFile, type SourceChoice, SourcePicker, TaskProgress, taskMessageClass, toSource } from './ModelDownload'
import { BUTTON, CARD, ERROR_TEXT, PRIMARY_BUTTON } from './styles'

/**
 * 第 3 步：下载模型。下载与导入在核心进程中进行，关闭窗口也会继续，进度经事件推来；
 * 显示当前代理状态，并提供换用镜像或设置代理的入口。
 * 进入时依次完整校验本步需要的、已安装但还没有通过校验的模型（下载、导入成功时已记为通过）；校验不通过的按未安装处理，
 * 需要重新下载或导入。所需模型都通过校验后才能进入下一步（Onboarding 的 `blockerAt`）。
 * Android 上只要一个模型（`requiredModel`）：芯片有 NPU 版时先下 NPU 模型，NPU 检测未通过后回到这一步下载通用模型；
 * 已装好的其他模型照常列出
 */
export default function ModelStep({ status, mmproj, onProxy }: { status: SetupStatus; mmproj: boolean; onProxy: (p: ProxySetting) => void }) {
  const [source, setSource] = useState<SourceChoice>({ type: 'official', url: '' })
  const [error, setError] = useState('')
  // `import_models` 还没返回
  const [picking, setPicking] = useState(false)
  // 进入这一步时已发起过校验；开发模式下 effect 会执行两次
  const autoVerified = useRef(false)
  const task = status.task
  const running = task?.state === 'running'
  const required = requiredModel(status)
  const rows = isAndroid ? modelInfos(status).filter((m) => m.label === required.label || rowFile(status, m).ready) : MODELS.filter((m) => !m.mmproj || mmproj)
  const files = isAndroid ? [rowFile(status, required)] : mmproj ? [status.model, status.mmproj] : [status.model]
  // 都已安装且没有校验出问题，不需要下载或导入
  const ready = files.every((f) => f.ready && f.verified !== false)
  // 已安装但还没有通过校验（校验被取消时由“重新校验”再发起）
  const unverified = files.some((f) => f.ready && f.verified === null)
  // 校验已发起、还没看到进行中的校验任务，不显示“重新校验”；进入时会自动校验的，从第一次渲染起就算
  const [starting, setStarting] = useState(unverified && !running)
  // 校验刚结束、重新读取的状态还没到时文件仍是未校验，不显示“重新校验”
  const verifyDone = task?.kind === 'verify' && task.state === 'done'
  // 有下载了一部分的 .part 时显示暂停的进度，续传
  const paused = files.find((f) => !f.ready && f.partialBytes > 0)

  const call = (cmd: string, args?: Record<string, unknown>) => {
    setError('')
    return invoke(cmd, args).then(
      () => true,
      (err) => {
        console.warn(`${cmd} 失败：`, err)
        setError(errorMessage(err, '操作失败，请重试。'))
        return false
      },
    )
  }

  const download = () => call('download_models', { source: toSource(source), mmproj, npu: isAndroid && required.npu })
  const importModels = () => {
    setPicking(true)
    call('import_models').then(() => setPicking(false))
  }
  // 返回时校验任务已开始（之后由 `running` 控制显示），或没有要校验的文件
  const verify = () => {
    setStarting(true)
    call('verify_models', { mmproj, npu: isAndroid && Boolean(status.npu) }).then(() => setStarting(false))
  }

  // 取消校验、导入只用胶囊通知说明，不留在页面上；只对在这一步看到过进行中的任务提示
  const seenRunning = useRef(false)
  useEffect(() => {
    if (running) {
      seenRunning.current = true
      return
    }
    if (!seenRunning.current || task?.state !== 'cancelled') return
    seenRunning.current = false
    if (task.kind !== 'download' && task.message) showToast(task.message)
  }, [task, running])

  useEffect(() => {
    if (autoVerified.current || running || !unverified) return
    autoVerified.current = true
    verify()
  }, [])

  return (
    <div>
      <h2 className="text-base font-bold text-gray-800 dark:text-gray-100">下载模型</h2>
      <p className="mt-1 text-sm text-gray-500 dark:text-gray-400">识别文件内容需要本地模型，下载一次即可，之后完全离线使用。</p>

      <div className={`mt-4 ${CARD}`}>
        {rows.map((m) => (
          <ModelRow key={m.label} info={m} file={rowFile(status, m)} running={running} />
        ))}
        {!ready && <ImportRow disabled={running} picking={picking} onImport={importModels} tokenizerUrl={status.tokenizerUrl} npu={required.npu} />}
      </div>

      {running && task && (
        <div className="mt-4 flex items-center gap-3">
          <TaskProgress task={task} label={modelLabel(status)} className="min-w-0 flex-1" />
          {task.kind === 'download' ? (
            <PauseResumeButton paused={false} onClick={() => call('cancel_model_task')} />
          ) : (
            task.kind !== 'delete' && (
              <button type="button" className={`flex-shrink-0 ${BUTTON}`} onClick={() => call('cancel_model_task')}>
                取消
              </button>
            )
          )}
        </div>
      )}
      {/* 暂停或出错中断的下载：进度条停在已下载的位置，点图标续传 */}
      {!running && paused && (
        <div className="mt-4 flex items-center gap-3">
          <PausedProgress label={modelLabel(status)(paused.name).trim()} file={paused} interrupted={task?.kind === 'download' && task.state === 'failed'} className="min-w-0 flex-1" />
          <PauseResumeButton paused disabled={picking} onClick={download} />
        </div>
      )}
      {/* 成功时模型行的状态已说明结果；暂停的下载由上面的进度说明；取消校验、导入用胶囊通知说明 */}
      {!running && task?.message && task.state !== 'done' && task.state !== 'cancelled' && (
        <p data-selectable-text className={`mt-3 text-xs ${taskMessageClass(task)}`}>
          {task.message}
        </p>
      )}
      {!running && !starting && !verifyDone && unverified && (
        <div className="mt-3">
          <button type="button" className={BUTTON} onClick={verify}>
            重新校验
          </button>
        </div>
      )}

      {!ready && (
        <div className="mt-5 space-y-3">
          <SourcePicker value={source} onChange={setSource} disabled={running} />
          <ProxyStatus proxy={status.proxy} onProxy={onProxy} />

          {!paused && (
            <div className="flex justify-end pt-1">
              <button type="button" className={`inline-flex items-center gap-1.5 ${PRIMARY_BUTTON}`} disabled={running || picking} onClick={download}>
                <DownloadIcon className="h-4 w-4" />
                {files.some((f) => f.verified === false) ? '重新下载' : '开始下载'}
              </button>
            </div>
          )}
        </div>
      )}
      {error && <p data-selectable-text className={`mt-2 ${ERROR_TEXT}`}>{error}</p>}
    </div>
  )
}
