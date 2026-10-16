import { useState } from 'react'
import type { ParamDef, ParamValue, ParamValues } from '../types'
import { useSaveParam } from './useSaveParam'

/**
 * 修改参数，设置页与引导的“高级”共用：影响索引的修改先等确认（`pending`，确认文字见 `rebuildConfirmText`），
 * 其余直接保存。`onSaved` 在每项保存成功后调用；取消确认后 `epoch` 递增，让输入框回到当前值
 */
export function useParamChanges(values: ParamValues, setParam: (key: string, value: ParamValue) => Promise<string | null>, onSaved?: (key: string) => void) {
  const { errors, save } = useSaveParam(setParam)
  const [pending, setPending] = useState<[ParamDef, ParamValue][] | null>(null)
  const [epoch, setEpoch] = useState(0)

  const apply = async (changes: [ParamDef, ParamValue][]) => {
    for (const [d, v] of changes) {
      if (await save(d.key, v)) onSaved?.(d.key)
    }
  }

  const request = (changes: [ParamDef, ParamValue][]) => {
    const real = changes.filter(([d, v]) => values[d.key] !== v)
    if (!real.length) return
    if (real.some(([d]) => d.affectsIndex)) {
      setPending(real)
      return
    }
    void apply(real)
  }

  const confirm = () => {
    if (pending) void apply(pending)
    setPending(null)
  }

  const cancel = () => {
    setPending(null)
    setEpoch((n) => n + 1)
  }

  return { errors, epoch, pending, request, confirm, cancel }
}
