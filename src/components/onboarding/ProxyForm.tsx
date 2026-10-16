import { useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { ProxyMode, ProxySetting } from '../../types'
import { errorMessage } from '../../lib/errors'
import { EyeIcon, EyeOffIcon } from '../icons'
import Select from '../Select'
import { ERROR_TEXT, INPUT, segmentClass, SEGMENTS } from './styles'

export const PROXY_LABEL: Record<ProxyMode, string> = {
  system: '跟随系统',
  none: '不使用代理',
  custom: '自定义',
}

const MODES = (Object.keys(PROXY_LABEL) as ProxyMode[]).map((value) => ({ value, label: PROXY_LABEL[value] }))

/**
 * 网络代理的简易设置：
 * - 选择“自定义”后先显示地址输入框，填写有效地址后才保存并生效；
 * - 自定义地址保存失败时，分段控件回到当前生效的模式，地址输入框仍保留以便修改；
 * - 地址中带用户名和密码时默认遮挡，可切换显示；只在保存失败时提示。
 * 引导中用分段控件；设置页传入 `label`，显示为“名称 + 下拉选择”的一行
 */
export default function ProxyForm({ saved, onSaved, label }: { saved: ProxySetting; onSaved: (s: ProxySetting) => void; label?: string }) {
  const [mode, setMode] = useState(saved.mode)
  const [url, setUrl] = useState(saved.url)
  const [error, setError] = useState('')
  const [showUrl, setShowUrl] = useState(false)
  // 自定义地址保存失败时分段控件回到生效中的模式，地址输入框仍保留
  const [editing, setEditing] = useState(false)
  const showInput = mode === 'custom' || editing
  const hasAuth = url.includes('@')

  const apply = (next: ProxySetting) => {
    if (next.mode === saved.mode && next.url.trim() === saved.url) return
    invoke<ProxySetting>('set_proxy', { setting: next })
      .then((s) => {
        onSaved(s)
        setUrl(s.url)
        setError('')
        setEditing(false)
      })
      .catch((err) => {
        console.warn('保存代理设置失败：', err)
        setError(errorMessage(err, '保存代理设置失败。'))
        if (next.mode !== 'custom') return
        setMode(saved.mode)
        setEditing(true)
      })
  }

  const pick = (m: ProxyMode) => {
    setMode(m)
    setError('')
    setEditing(false)
    if (m === 'custom' && !url.trim()) return
    apply({ mode: m, url })
  }

  return (
    <div className="space-y-2">
      {label ? (
        <div className="flex items-center justify-between gap-3">
          <span className="block text-sm text-gray-600 dark:text-gray-300">{label}</span>
          <div className="min-w-0 max-w-[60%]">
            <Select value={mode} onChange={pick} options={MODES} />
          </div>
        </div>
      ) : (
        <div className={SEGMENTS}>
          {MODES.map((m) => (
            <button key={m.value} type="button" className={segmentClass(mode === m.value)} onClick={() => pick(m.value)}>
              {m.label}
            </button>
          ))}
        </div>
      )}
      {showInput && (
        <div className="relative">
          <input
            value={url}
            onChange={(e) => {
              setUrl(e.target.value)
              setError('')
            }}
            onBlur={() => apply({ mode: 'custom', url })}
            onKeyDown={(e) => {
              if (e.key === 'Enter') apply({ mode: 'custom', url })
            }}
            type={hasAuth && !showUrl ? 'password' : 'text'}
            spellCheck={false}
            placeholder="http://127.0.0.1:7890"
            aria-label="代理地址"
            className={`${INPUT} ${hasAuth ? 'pr-10' : ''}`}
          />
          {hasAuth && (
            <button
              type="button"
              onClick={() => setShowUrl((v) => !v)}
              className="absolute right-2 top-1/2 -translate-y-1/2 p-1 text-gray-400 transition-colors hover:text-gray-600"
              tabIndex={-1}
              aria-label={showUrl ? '隐藏代理地址' : '显示代理地址'}
            >
              {showUrl ? <EyeIcon className="h-4 w-4" /> : <EyeOffIcon className="h-4 w-4" />}
            </button>
          )}
        </div>
      )}
      {error && <p data-selectable-text className={ERROR_TEXT}>{error}</p>}
      <p className="text-xs text-gray-500 dark:text-gray-500">
        {showInput
          ? '只支持 HTTP 代理，可带用户名和密码。本机地址始终直连。'
          : mode === 'none'
            ? '下载模型和检查更新不经过代理。'
            : '下载模型和检查更新使用的代理，修改后立即生效。系统使用自动配置脚本（PAC）时，请改为自定义并填写代理地址。'}
      </p>
    </div>
  )
}
