import { useEffect, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { ModelFileStatus, SetupStatus } from '../../types'
import { errorMessage } from '../../lib/errors'
import { formatSize } from '../../lib/format'
import ConfirmDialog from '../ConfirmDialog'
import { ImportRow, MMPROJ_LABEL, MODEL_LABEL, MODELS, ModelRow, modelLabel, type SourceChoice, SourcePicker, TaskProgress, taskMessageClass, toSource } from '../onboarding/ModelDownload'
import { showToast } from '../Toast'
import { BUTTON, CARD, ERROR_TEXT, SMALL_BUTTON, SMALL_DANGER_BUTTON, SMALL_PRIMARY_BUTTON } from '../onboarding/styles'
import { ROW_DESC, SECTION_TITLE } from './styles'

interface Props {
  setup: SetupStatus
  /** 本次打开设置后在这一页开始过任务，或看到过进行中的任务；只显示这些任务的结果，不显示引导中留下的 */
  acted: boolean
  onAct: () => void
}

/**
 * “模型”页：文字模型与图片与音视频模型的安装状态、大小与上次校验的结果；
 * 下载（暂停即取消并保留已下载的部分，继续时续传）、导入、校验，以及删除任一模型（或它下载了一部分的文件）。
 * 设备检测进行中不能下载、导入或删除（检测要打开模型文件），校验不受影响。
 * 任务都在后台服务中进行，同一时间只有一个，关闭设置也会继续；任务结束后引导状态会重新读取。
 * 校验的结果以底部的胶囊通知显示，只显示在这一页看到开始或进行中的校验。
 * 删除文字模型后不能按内容搜索（查询也要它算向量），也不再识别新内容；已建立的索引保留，重新下载或导入后恢复
 */
export default function ModelsTab({ setup, acted, onAct }: Props) {
  const [source, setSource] = useState<SourceChoice>({ type: 'official', url: '' })
  const [error, setError] = useState('')
  // 要删除的模型：`mmproj` 与核心命令的参数一致
  const [confirmDelete, setConfirmDelete] = useState<{ mmproj: boolean } | null>(null)
  // 在这一页看到过进行中的校验，结束时显示结果；没有看到 running 的（如请求失败）不显示
  const verifying = useRef(false)
  const importing = useRef(false)
  const task = setup.task
  const running = task?.state === 'running'

  useEffect(() => {
    if (running) onAct()
  }, [running, onAct])

  useEffect(() => {
    if (!task) return
    if (task.state === 'running') {
      verifying.current = task.kind === 'verify'
      importing.current = task.kind === 'import'
      return
    }
    if (verifying.current && task.message) showToast(task.message, task.state === 'done' ? 'success' : task.state === 'failed' ? 'error' : 'info')
    // 取消导入同样只用胶囊通知说明
    if (importing.current && task.state === 'cancelled' && task.message) showToast(task.message)
    verifying.current = false
    importing.current = false
  }, [task])

  // `import_models` 还没返回
  const [picking, setPicking] = useState(false)
  const blocked = running || setup.selftest.running

  const call = (cmd: string, args?: Record<string, unknown>) => {
    setError('')
    onAct()
    return invoke(cmd, args).catch((err) => {
      console.warn(`${cmd} 失败：`, err)
      setError(errorMessage(err, '操作失败，请重试。'))
    })
  }

  const importModels = () => {
    setPicking(true)
    call('import_models').then(() => setPicking(false))
  }

  // 删除确认中的模型
  const delFile = confirmDelete?.mmproj ? setup.mmproj : setup.model
  const delName = confirmDelete?.mmproj ? MMPROJ_LABEL : MODEL_LABEL
  const delPartial = !delFile.ready && delFile.partialBytes > 0
  const delEffect = confirmDelete?.mmproj
    ? '已识别的图片、音频和视频仍可用文字搜索，但不能再用图片搜索。新增或修改的图片、音频和视频只能按文件名搜索。\n\n重新下载或导入后会自动识别。'
    : '删除后不能按内容搜索，只显示文件名或正文中含有搜索词的结果，也不再识别新的文件内容。\n\n已建立的索引保留，重新下载或导入后恢复。'

  const actions = (file: ModelFileStatus, mmproj: boolean) => {
    const download = (text: string) => (
      <button type="button" className={SMALL_PRIMARY_BUTTON} disabled={blocked || picking} onClick={() => call('download_models', { source: toSource(source), mmproj })}>
        {text}
      </button>
    )
    const remove = (
      <button type="button" className={SMALL_DANGER_BUTTON} disabled={blocked} onClick={() => setConfirmDelete({ mmproj })}>
        删除
      </button>
    )
    if (!file.ready) {
      if (file.partialBytes === 0) return download('下载')
      return (
        <>
          {download('继续下载')}
          {remove}
        </>
      )
    }
    return (
      <>
        {file.verified === false ? (
          download('重新下载')
        ) : (
          <button
            type="button"
            className={SMALL_BUTTON}
            disabled={running}
            onClick={() => call('verify_model', { mmproj })}
          >
            校验
          </button>
        )}
        {remove}
      </>
    )
  }

  return (
    <div className="space-y-8">
      <section className="space-y-3">
        <h4 className={SECTION_TITLE}>模型</h4>
        <p className={ROW_DESC}>识别文件内容需要本地模型，下载一次即可，之后完全离线使用。</p>
        <div className={CARD}>
          {MODELS.map((m) => {
            const file = m.mmproj ? setup.mmproj : setup.model
            return <ModelRow key={m.label} info={m} url={setup.modelUrl} file={file} running={running} actions={actions(file, m.mmproj)} />
          })}
          {/* 两个模型都可用且没有校验出问题时不需要导入 */}
          {[setup.model, setup.mmproj].some((f) => !f.ready || f.verified === false) && (
            <ImportRow disabled={blocked} picking={picking} onImport={importModels} />
          )}
        </div>

        {running && task && (
          <div className="flex items-center gap-3">
            <TaskProgress task={task} label={modelLabel(setup)} className="min-w-0 flex-1" />
            {task.kind !== 'delete' && (
              <button type="button" className={`flex-shrink-0 ${BUTTON}`} onClick={() => call('cancel_model_task')}>
                {task.kind === 'download' ? '暂停' : '取消'}
              </button>
            )}
          </div>
        )}
        {/* 暂停的下载在行中显示已下载的大小；下载、导入成功时行中的状态已说明结果；校验的结果与取消导入用胶囊通知显示 */}
        {acted && !running && task?.message && task.kind !== 'verify' && task.state !== 'cancelled' && !((task.kind === 'download' || task.kind === 'import') && task.state === 'done') && (
          <p data-selectable-text className={`text-xs ${taskMessageClass(task)}`}>
            {task.message}
          </p>
        )}
        {setup.selftest.running && <p className={ROW_DESC}>设备检测正在进行，完成后才能下载、导入或删除模型</p>}
        {error && <p data-selectable-text className={ERROR_TEXT}>{error}</p>}
      </section>

      <section className="space-y-3">
        <h4 className={SECTION_TITLE}>下载</h4>
        <SourcePicker value={source} onChange={setSource} disabled={running} />
      </section>

      {confirmDelete && (
        <ConfirmDialog
          title={delPartial ? '删除已下载的部分' : `删除${delName}`}
          message={delPartial ? `将删除${delName}已下载的 ${formatSize(delFile.partialBytes)}，之后需要从头下载。` : `将删除${delName}，释放约 ${formatSize(delFile.bytes)} 空间。\n\n${delEffect}`}
          confirmText="删除"
          danger
          onConfirm={() => {
            setConfirmDelete(null)
            call('delete_model', { mmproj: confirmDelete.mmproj })
          }}
          onCancel={() => setConfirmDelete(null)}
        />
      )}
    </div>
  )
}
