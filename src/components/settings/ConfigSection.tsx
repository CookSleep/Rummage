import { useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { CoreStatus, ParamDef } from '../../types'
import { errorMessage } from '../../lib/errors'
import { rebuildConfirmText } from '../../lib/params'
import { useConfigImport } from '../../hooks/useConfigImport'
import ConfirmDialog from '../ConfirmDialog'
import CoreStatusNotice from '../CoreStatusNotice'
import { SlidersIcon } from '../icons'
import { CARD, CARD_ROW, ICON_TILE, ROW_ACTIONS, SMALL_BUTTON } from '../onboarding/styles'
import { showToast } from '../Toast'
import { SECTION_NOTICE, SECTION_TITLE } from './styles'

interface Props {
  defs: ParamDef[]
  offline: boolean
  coreStatus: CoreStatus
}

/**
 * “数据”页的“配置”：导出、导入文件夹、排除规则和参数，只能在同类设备之间导入。
 * 保存位置与导入的文件都由 Rust 侧的对话框选择；导入会改变影响索引的参数时，先确认重建，同“参数”页的修改
 */
export default function ConfigSection({ defs, offline, coreStatus }: Props) {
  const [exporting, setExporting] = useState(false)
  const imp = useConfigImport(defs)

  const exportConfig = () => {
    setExporting(true)
    invoke<boolean>('export_config')
      .then((saved) => {
        if (saved) showToast('配置已导出', 'success')
      })
      .catch((err) => {
        console.warn('无法导出配置：', err)
        showToast(errorMessage(err, '配置未能导出，请重试。'), 'error')
      })
      .finally(() => setExporting(false))
  }

  return (
    <section className="space-y-3">
      <h4 className={SECTION_TITLE}>配置</h4>
      {offline ? <CoreStatusNotice status={coreStatus} className={SECTION_NOTICE} /> : (
        <div className={CARD}>
          <div className={CARD_ROW}>
            <span className={ICON_TILE}>
              <SlidersIcon className="h-4 w-4" />
            </span>
            <span className="min-w-0 flex-1">
              <span className="block text-sm text-gray-800 dark:text-gray-100">配置文件</span>
              <span className="mt-0.5 block text-xs text-gray-500 dark:text-gray-400">导出文件夹、排除规则和参数，可在另一台同类设备上导入。</span>
            </span>
            <span className={`gap-1.5 ${ROW_ACTIONS}`}>
              <button type="button" className={SMALL_BUTTON} disabled={exporting} onClick={exportConfig}>
                {exporting ? '正在导出' : '导出配置'}
              </button>
              <button type="button" className={SMALL_BUTTON} disabled={imp.busy} onClick={imp.start}>
                {imp.busy ? '正在导入' : '导入配置'}
              </button>
            </span>
          </div>
        </div>
      )}
      {imp.pending && (
        <ConfirmDialog
          title="重建索引"
          message={rebuildConfirmText(imp.pending)}
          confirmText="导入并重建"
          onConfirm={imp.confirm}
          onCancel={imp.cancel}
        />
      )}
    </section>
  )
}
