import { useEffect, useRef, useState } from 'react'
import { isAndroid, isTauri, setBeforeClose } from '../lib/platform'
import { readLocal, writeLocal } from '../lib/storage'
import ConfirmDialog from './ConfirmDialog'

/** 已提示过“仍在后台运行”的本地存储键 */
const SHOWN_KEY = 'rummage.trayHintShown'

/** 第一次关闭窗口时提示 Rummage 仍在后台运行（关闭窗口后界面留在托盘，核心继续建索引），确认后才关闭；Android 上没有托盘，不提示 */
export default function TrayHint() {
  const [open, setOpen] = useState(false)
  const pending = useRef<{ promise: Promise<void>; resolve: () => void } | null>(null)

  useEffect(() => {
    if (!isTauri || isAndroid) return
    setBeforeClose(() => {
      if (pending.current) return pending.current.promise
      // 存储不可用时不提示，免得每次关闭都弹出
      if (readLocal(SHOWN_KEY, '1') === '1') return Promise.resolve()
      let resolve = () => {}
      const promise = new Promise<void>((r) => {
        resolve = r
      })
      pending.current = { promise, resolve }
      setOpen(true)
      return promise
    })
    return () => {
      setBeforeClose(null)
      pending.current?.resolve()
      pending.current = null
    }
  }, [])

  if (!open) return null
  const close = () => {
    writeLocal(SHOWN_KEY, '1')
    setOpen(false)
    pending.current?.resolve()
    pending.current = null
  }
  return (
    <ConfirmDialog
      title="Rummage 仍在后台运行"
      message={'关闭窗口后，Rummage 会在后台继续建立索引。\n\n可从任务栏通知区域的托盘图标重新打开或退出。'}
      confirmText="知道了"
      hideCancel
      onConfirm={close}
      onCancel={close}
    />
  )
}
