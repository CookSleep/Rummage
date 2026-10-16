import { useCallback, useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { ParamDef, ParamValue, ParamValues } from '../../types'
import { errorMessage } from '../../lib/errors'
import { rebuildConfirmText } from '../../lib/params'
import { isAndroid } from '../../lib/platform'
import { useParamChanges } from '../../hooks/useParamChanges'
import ConfirmDialog from '../ConfirmDialog'
import LoadError from '../LoadError'
import ParamField from '../settings/ParamField'

/** 影响建立索引速度的参数，按此顺序列出 */
const SPEED_KEYS = ['image_tokens', 'long_image_tokens', 'video_frame_tokens', 'chunk_tokens', 'index_threads']

/**
 * 引导“检测设备”一步的“高级”：只列出影响建立索引速度的参数，修改即保存，与设置页相同（影响索引的先确认）。
 * 确认重建时换成确认框，不叠两层弹窗；只在其他平台显示的参数不列出
 */
export default function SpeedParamsDialog({ values, onClose }: { values: ParamValues; onClose: () => void }) {
  const [defs, setDefs] = useState<ParamDef[] | null>(null)
  const [loadError, setLoadError] = useState('')
  // 保存成功后先显示新值，`ParamsChanged` 事件带来新的参数后以它为准
  const [saved, setSaved] = useState<ParamValues>({})
  const current = { ...values, ...saved }

  useEffect(() => setSaved({}), [values])

  const load = useCallback(() => {
    setLoadError('')
    invoke<ParamDef[]>('param_defs')
      .then(setDefs)
      .catch((err) => {
        console.warn('无法读取设置：', err)
        setLoadError(errorMessage(err, '无法读取设置，请稍后再试。'))
      })
  }, [])

  useEffect(() => {
    load()
  }, [load])

  const setParam = async (key: string, value: ParamValue) => {
    const warning = await invoke<string | null>('set_param', { key, value })
    setSaved((v) => ({ ...v, [key]: value }))
    return warning
  }
  const { errors, epoch, pending, request, confirm, cancel } = useParamChanges(current, setParam)

  if (pending) return <ConfirmDialog title="重建索引" message={rebuildConfirmText(pending)} confirmText="修改并重建" onConfirm={confirm} onCancel={cancel} />

  const platform = isAndroid ? 'android' : 'windows'
  const list = SPEED_KEYS.flatMap((k) => defs?.find((d) => d.key === k && (!d.platform || d.platform === platform)) ?? [])

  return (
    <ConfirmDialog title="高级" message="以下设置影响建立索引的速度，修改后重新检测，测得的用时随之更新。" confirmText="完成" hideCancel onConfirm={onClose} onCancel={onClose}>
      <div className="space-y-4">
        {loadError && <LoadError message={loadError} onRetry={load} />}
        {list.map((d) => (
          <ParamField key={`${d.key}-${epoch}`} def={d} value={current[d.key] ?? d.default} error={errors[d.key]} onChange={(v) => request([[d, v]])} />
        ))}
      </div>
    </ConfirmDialog>
  )
}
