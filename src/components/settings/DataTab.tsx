import { useCallback, useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { CoreStatus, StorageInfo } from '../../types'
import { errorMessage } from '../../lib/errors'
import { formatSize } from '../../lib/format'
import ChangeDataDirButton from '../ChangeDataDirButton'
import ConfirmDialog from '../ConfirmDialog'
import CoreStatusNotice from '../CoreStatusNotice'
import { FolderIcon, TrashIcon } from '../icons'
import LoadError from '../LoadError'
import { HoverTip } from '../Tip'
import { CARD, CARD_ROW, ERROR_TEXT, ICON_TILE, SMALL_BUTTON, SMALL_DANGER_BUTTON } from '../onboarding/styles'
import { ROW_DESC, SECTION_NOTICE, SECTION_TITLE } from './styles'

/** 占用的各部分，固定顺序；颜色在浅色与深色模式下都取柔和的中间色 */
const PARTS: { key: keyof Omit<StorageInfo, 'dataDir'>; label: string; color: string }[] = [
  { key: 'models', label: '模型', color: 'bg-blue-400 dark:bg-blue-400/80' },
  { key: 'db', label: '索引数据库', color: 'bg-violet-400 dark:bg-violet-400/80' },
  { key: 'thumbs', label: '缩略图', color: 'bg-teal-400 dark:bg-teal-400/80' },
  { key: 'logs', label: '日志', color: 'bg-amber-400 dark:bg-amber-400/80' },
  { key: 'other', label: '其他', color: 'bg-gray-300 dark:bg-gray-500' },
]

const PLACEHOLDER = 'animate-pulse rounded bg-black/[0.06] dark:bg-white/[0.08]'

/**
 * “数据”页：数据目录（打开、更改位置）、各部分的空间占用与清空索引。
 * 更改位置的目录由 Rust 侧的对话框选择，确认后重启，下次启动时迁移并显示进度。
 * 只有清空索引要用后台服务，不可用（`offline`）时那一分区只显示它的状态
 */
export default function DataTab({ offline, coreStatus }: { offline: boolean; coreStatus: CoreStatus }) {
  const [info, setInfo] = useState<StorageInfo | null>(null)
  const [loadError, setLoadError] = useState('')
  const [confirmClear, setConfirmClear] = useState(false)
  const [dirError, setDirError] = useState('')
  const [clearing, setClearing] = useState(false)
  const [result, setResult] = useState('')
  const [clearError, setClearError] = useState('')

  const load = useCallback(() => {
    invoke<StorageInfo>('storage_info')
      .then((i) => {
        setInfo(i)
        setLoadError('')
      })
      .catch((err) => {
        console.warn('无法统计数据目录占用：', err)
        setLoadError(errorMessage(err, '无法统计占用空间。'))
      })
  }, [])

  useEffect(load, [load])

  const open = () => {
    setDirError('')
    invoke('open_data_dir').catch((err) => {
      console.warn('打开数据目录失败：', err)
      setDirError(errorMessage(err, '无法打开数据目录。'))
    })
  }

  const clearIndex = () => {
    setConfirmClear(false)
    setClearing(true)
    setResult('')
    setClearError('')
    invoke('clear_index')
      .then(() => {
        setResult('已清空索引，正在按现有文件夹重新建立')
        load()
      })
      .catch((err) => {
        console.warn('清空索引失败：', err)
        setClearError(errorMessage(err, '清空索引失败，请重试。'))
      })
      .finally(() => setClearing(false))
  }

  const total = info ? PARTS.reduce((sum, p) => sum + info[p.key], 0) : 0

  return (
    <div className="space-y-8">
      <section className="space-y-3">
        <h4 className={SECTION_TITLE}>数据目录</h4>
        <div className={CARD}>
          <div className={CARD_ROW}>
            <span className={ICON_TILE}>
              <FolderIcon className="h-4 w-4" />
            </span>
            <span className="min-w-0 flex-1">
              {info ? (
                <span data-selectable-text className="block truncate text-sm text-gray-800 dark:text-gray-100">
                  {info.dataDir}
                  <HoverTip label={info.dataDir} wrap />
                </span>
              ) : (
                // 外层与文字行高相同，加载前后高度不变
                <span className="flex h-5 items-center">
                  <span className={`h-4 w-48 max-w-full ${PLACEHOLDER}`} />
                </span>
              )}
              {info ? (
                <span className="mt-0.5 block text-xs text-gray-500 dark:text-gray-400">共 {formatSize(total)}</span>
              ) : (
                <span className="mt-0.5 flex h-4 items-center">
                  <span className={`h-3 w-16 ${PLACEHOLDER}`} />
                </span>
              )}
            </span>
            <span className="flex flex-shrink-0 gap-1.5">
              <button type="button" className={SMALL_BUTTON} disabled={!info} onClick={open}>
                打开
              </button>
              <ChangeDataDirButton label="更改位置" className={SMALL_BUTTON} disabled={!info} onError={setDirError} />
            </span>
          </div>
        </div>
        {loadError && <LoadError message={loadError} onRetry={load} />}
        {dirError && <p data-selectable-text className={ERROR_TEXT}>{dirError}</p>}
      </section>

      <section className="space-y-3">
        <h4 className={SECTION_TITLE}>空间占用</h4>
        <div className={CARD}>
          <div className="px-4 py-3.5">
            <div className="flex h-2 w-full gap-0.5 overflow-hidden rounded-full bg-black/[0.06] dark:bg-white/[0.08]">
              {info &&
                total > 0 &&
                PARTS.filter((p) => info[p.key] > 0).map((p) => (
                  <span key={p.key} className={`h-full min-w-[2px] ${p.color}`} style={{ width: `${(info[p.key] / total) * 100}%` }} />
                ))}
            </div>
          </div>
          {PARTS.map((p) => (
            <div key={p.key} className="flex items-center gap-2.5 px-4 py-2.5">
              <span className={`h-2 w-2 flex-shrink-0 rounded-full ${p.color}`} />
              <span className="flex-1 text-sm text-gray-600 dark:text-gray-300">{p.label}</span>
              {info ? (
                <span className="text-xs tabular-nums text-gray-500 dark:text-gray-400">{formatSize(info[p.key])}</span>
              ) : (
                <span className={`h-3 w-12 ${PLACEHOLDER}`} />
              )}
            </div>
          ))}
        </div>
      </section>

      <section className="space-y-3">
        <h4 className={SECTION_TITLE}>索引</h4>
        {offline ? <CoreStatusNotice status={coreStatus} className={SECTION_NOTICE} /> : (
          <div className={CARD}>
            <div className={CARD_ROW}>
              <span className={ICON_TILE}>
                <TrashIcon className="h-4 w-4" />
              </span>
              <span className="min-w-0 flex-1">
                <span className="block text-sm text-gray-800 dark:text-gray-100">清空索引</span>
                <span className="mt-0.5 block text-xs text-gray-500 dark:text-gray-400">删除全部索引后按现有文件夹重新建立，不会删除你的文件。</span>
              </span>
              <button type="button" className={`flex-shrink-0 ${SMALL_DANGER_BUTTON}`} disabled={clearing} onClick={() => setConfirmClear(true)}>
                {clearing ? '正在清空' : '清空'}
              </button>
            </div>
          </div>
        )}
        {result && <p data-selectable-text className={ROW_DESC}>{result}</p>}
        {clearError && <p data-selectable-text className={ERROR_TEXT}>{clearError}</p>}
      </section>

      {confirmClear && (
        <ConfirmDialog
          title="清空索引"
          message={'将删除全部索引数据，并按现有文件夹重新建立。\n\n不会删除你的文件，文件夹列表与设置会保留。'}
          confirmText="清空"
          danger
          onConfirm={clearIndex}
          onCancel={() => setConfirmClear(false)}
        />
      )}
    </div>
  )
}
