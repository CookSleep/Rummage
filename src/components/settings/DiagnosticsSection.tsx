import { useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { ParamDef, ParamValue, ParamValues } from '../../types'
import { errorMessage } from '../../lib/errors'
import { saveParam } from '../../lib/params'
import { isAndroid } from '../../lib/platform'
import { renderMessage } from '../ConfirmDialog'
import { ResetIcon } from '../icons'
import { SMALL_BUTTON } from '../onboarding/styles'
import { HoverTip } from '../Tip'
import { showToast } from '../Toast'
import ParamField from './ParamField'
import Toggle from './Toggle'
import { ROW_DESC, ROW_LABEL, SECTION_TITLE } from './styles'

/** 日志开关的参数：参数表中小标题为空，不在参数页的列表中，只在本区显示 */
const LOG_KEY = 'write_log_file'

interface Props {
  defs: ParamDef[]
  values: ParamValues
  setParam: (key: string, value: ParamValue) => Promise<string | null>
}

/**
 * “高级”页底部的诊断：写入日志文件、打开日志目录、导出诊断包。
 * 诊断包的保存位置由 Rust 侧的对话框选择，只在本地生成。Android 上日志在应用私有目录中，其他应用打不开，不显示“日志目录”
 */
export default function DiagnosticsSection({ defs, values, setParam }: Props) {
  const [keepPaths, setKeepPaths] = useState(false)
  const [exporting, setExporting] = useState(false)
  const [status, setStatus] = useState('')
  const def = defs.find((d) => d.key === LOG_KEY)

  const openLogs = () => {
    invoke('open_log_dir').catch((err) => {
      console.warn('无法打开日志目录：', err)
      showToast(errorMessage(err, '无法打开日志目录。'), 'error')
    })
  }

  const exportPackage = () => {
    setExporting(true)
    setStatus('')
    invoke<string | null>('export_diagnostics', { keepPaths })
      .then((path) => {
        if (path) setStatus(`已保存到「${path}」`)
      })
      .catch((err) => {
        console.warn('无法导出诊断包：', err)
        showToast(errorMessage(err, '诊断包未能导出，请重试。'), 'error')
      })
      .finally(() => setExporting(false))
  }

  return (
    <section className="space-y-4">
      <h4 className={SECTION_TITLE}>诊断</h4>
      {def && (
        <ParamField
          def={def}
          value={values[LOG_KEY] ?? def.default}
          onChange={(v) => void saveParam(setParam, LOG_KEY, v)}
        />
      )}
      {!isAndroid && (
        <div>
          <div className="mb-1 flex items-center justify-between gap-3">
            <span className={ROW_LABEL}>日志目录</span>
            <button type="button" className={SMALL_BUTTON} onClick={openLogs}>
              打开
            </button>
          </div>
          <div className={ROW_DESC}>日志保存在数据目录中的“logs”文件夹，反馈问题时可以附上。</div>
        </div>
      )}
      <div>
        <div className="mb-1 flex items-center justify-between gap-3">
          <span className={ROW_LABEL}>诊断包中保留文件路径</span>
          {/* 恢复默认按钮与 ParamField 一致：默认关闭 */}
          <div className="flex items-center gap-1.5">
            <Toggle checked={keepPaths} onChange={setKeepPaths} label="诊断包中保留文件路径" />
            <button
              type="button"
              disabled={!keepPaths}
              onClick={() => setKeepPaths(false)}
              aria-label="诊断包中保留文件路径恢复默认"
              className="shrink-0 rounded-full p-1 text-gray-400 transition hover:bg-black/5 hover:text-gray-600 disabled:pointer-events-none disabled:opacity-30 dark:hover:bg-white/10 dark:hover:text-gray-200"
            >
              <ResetIcon className="h-3.5 w-3.5" />
              <HoverTip label="恢复默认" />
            </button>
          </div>
        </div>
        <div className={ROW_DESC}>关闭时，文件路径会替换为占位符。</div>
      </div>
      <div>
        <div className="mb-1 flex items-center justify-between gap-3">
          <span className={ROW_LABEL}>导出诊断包</span>
          <button type="button" className={SMALL_BUTTON} disabled={exporting} onClick={exportPackage}>
            {exporting ? '正在导出' : '导出'}
          </button>
        </div>
        <div className={ROW_DESC}>把日志、设备检测结果、版本与显卡信息和设置打包成 ZIP，保存在本机，不会上传。不含搜索历史和文件内容。</div>
        {status && <div data-selectable-text className={`mt-1 break-all ${ROW_DESC}`}>{renderMessage(status, 'font-semibold')}</div>}
      </div>
    </section>
  )
}
