import { useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { ConfigImport, ConfigPreview, ParamDef, ParamValue } from '../types'
import { errorMessage } from '../lib/errors'
import { importMessage } from '../lib/config'
import { showToast } from '../components/Toast'

/**
 * 导入配置：Rust 侧选择并校验文件（`import_config`），再导入（`apply_config`）。
 * 传入 `defs` 时（设置页），会改变影响索引的参数就先等确认（`pending`，确认文字见 `rebuildConfirmText`），同参数页的修改；
 * 引导中还没有建立索引，传空，不确认。成功后显示胶囊通知并调用 `onDone`；参数已保存、但需要告诉用户的情况放在 `error` 中
 */
export function useConfigImport(defs: ParamDef[] | null, onDone?: () => void) {
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [pending, setPending] = useState<[ParamDef, ParamValue][] | null>(null)

  const apply = () => {
    invoke<ConfigImport>('apply_config')
      .then((r) => {
        showToast(importMessage(r), 'success')
        if (r.warning) setError(r.warning)
        onDone?.()
      })
      .catch((err) => {
        console.warn('无法导入配置：', err)
        setError(errorMessage(err, '配置未能导入，请重试。'))
      })
      .finally(() => setBusy(false))
  }

  const start = () => {
    setBusy(true)
    setError('')
    invoke<ConfigPreview | null>('import_config')
      .then((p) => {
        if (!p) {
          setBusy(false)
          return
        }
        const changes = (defs ?? []).filter((d) => d.key in p.params).map((d): [ParamDef, ParamValue] => [d, p.params[d.key]])
        if (changes.some(([d]) => d.affectsIndex)) {
          setPending(changes)
          return
        }
        apply()
      })
      .catch((err) => {
        console.warn('无法读取配置：', err)
        setError(errorMessage(err, '无法读取配置文件，请重新选择。'))
        setBusy(false)
      })
  }

  const confirm = () => {
    setPending(null)
    apply()
  }

  const cancel = () => {
    setPending(null)
    setBusy(false)
  }

  return { busy, error, pending, start, confirm, cancel }
}
