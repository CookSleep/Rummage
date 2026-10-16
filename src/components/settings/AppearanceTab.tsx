import { useState } from 'react'
import type { Appearance, CoreStatus } from '../../types'
import { errorMessage } from '../../lib/errors'
import { CLOSE_ACTION_KEY, isAndroid } from '../../lib/platform'
import { readLocal, writeLocal } from '../../lib/storage'
import CoreStatusNotice from '../CoreStatusNotice'
import { ERROR_TEXT } from '../onboarding/styles'
import Select from '../Select'
import FontSettingField from './FontSettingField'
import { ROW_DESC, ROW_LABEL, SECTION_NOTICE, SECTION_TITLE } from './styles'

interface Props {
  appearance: Appearance
  /** 立即应用并保存；保存失败时抛出带 `message` 的错误 */
  onChange: (next: Appearance) => Promise<void>
  /** 后台服务不可用：字体存在后台服务中，这一分区只显示它的状态 */
  offline: boolean
  coreStatus: CoreStatus
}

/**
 * “外观”页：界面与等宽字体，以及关闭窗口时留在托盘还是退出；字体许可的入口在“关于”页。
 * 关闭窗口的行为只关乎界面进程，存在本地（`CLOSE_ACTION_KEY`），由关闭处理读取（`installCloseWatch`）；Android 上没有这一项
 */
export default function AppearanceTab({ appearance, onChange, offline, coreStatus }: Props) {
  const [error, setError] = useState('')
  const [closeAction, setCloseAction] = useState(() => (readLocal(CLOSE_ACTION_KEY) === 'quit' ? 'quit' : 'tray'))

  const change = (next: Appearance) => {
    setError('')
    onChange(next).catch((err) => {
      console.warn('无法保存外观设置：', err)
      setError(errorMessage(err, '外观设置未能保存，下次启动可能恢复原样。'))
    })
  }

  return (
    <div className="space-y-8">
      <section className="space-y-4">
        <h4 className={SECTION_TITLE}>字体</h4>
        {offline ? <CoreStatusNotice status={coreStatus} className={SECTION_NOTICE} /> : (
          <>
            <FontSettingField
              slot="ui"
              label="界面字体"
              description="内置字体为 HarmonyOS Sans SC；选择“系统字体”时使用系统默认的字体。"
              mode={appearance.uiFont}
              name={appearance.uiFontName}
              onChange={(mode, name) => change({ ...appearance, uiFont: mode, uiFontName: name })}
            />
            <FontSettingField
              slot="mono"
              label="等宽字体"
              description="内置字体为 Maple Mono NF CN，用于详情栏中的文档预览与排除规则等。"
              mode={appearance.monoFont}
              name={appearance.monoFontName}
              onChange={(mode, name) => change({ ...appearance, monoFont: mode, monoFontName: name })}
            />
            {error && <p data-selectable-text className={ERROR_TEXT}>{error}</p>}
          </>
        )}
      </section>
      {!isAndroid && (
        <section className="space-y-4">
          <h4 className={SECTION_TITLE}>窗口</h4>
          <div>
            <div className="mb-1 flex items-center justify-between gap-3">
              <span className={ROW_LABEL}>关闭窗口时</span>
              <div className="min-w-0 max-w-[60%]">
                <Select
                  label="关闭窗口时"
                  value={closeAction}
                  options={[
                    { label: '留在托盘', value: 'tray' },
                    { label: '退出 Rummage', value: 'quit' },
                  ]}
                  onChange={(v) => {
                    setCloseAction(v)
                    writeLocal(CLOSE_ACTION_KEY, v)
                  }}
                />
              </div>
            </div>
            <div className={ROW_DESC}>留在托盘时，后台服务继续建立索引；退出时，后台服务一并退出。</div>
          </div>
        </section>
      )}
    </div>
  )
}
