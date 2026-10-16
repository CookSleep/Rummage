import { useState } from 'react'
import type { ParamValue } from '../types'
import { errorMessage } from '../lib/errors'

/** 保存参数，并按键记下显示在该项下方的文字：已保存但需要告诉用户的提示，或保存失败的原因。`save` 返回是否保存成功 */
export function useSaveParam(setParam: (key: string, value: ParamValue) => Promise<string | null>) {
  const [errors, setErrors] = useState<Record<string, string>>({})

  const save = async (key: string, value: ParamValue) => {
    try {
      const warning = await setParam(key, value)
      setErrors((e) => ({ ...e, [key]: warning ?? '' }))
      return true
    } catch (err) {
      setErrors((e) => ({ ...e, [key]: errorMessage(err, '设置未能保存，请重试。') }))
      return false
    }
  }

  return { errors, save }
}
