import { createPortal } from 'react-dom'
import harmonyLicense from '../../../third-party-licenses/HarmonyOS-Sans-LICENSE.txt?raw'
import mapleLicense from '../../../third-party-licenses/MapleMono-LICENSE.txt?raw'
import { useEscape } from '../../hooks/useEscape'
import { openExternal } from '../../lib/platform'
import { CloseIcon } from '../icons'

const FONT_LICENSES = [
  {
    name: 'HarmonyOS Sans SC',
    author: 'Huawei Device Co., Ltd.',
    license: 'HarmonyOS Sans Fonts License Agreement',
    url: 'https://developer.huawei.com/consumer/cn/design/resource/',
    licenseUrl: 'https://github.com/openharmony/resources/blob/master/LICENSE_Fonts',
    text: harmonyLicense,
  },
  {
    name: 'Maple Mono NF CN',
    author: 'The Maple Mono Project Authors',
    license: 'SIL Open Font License 1.1',
    url: 'https://github.com/subframe7536/maple-font',
    licenseUrl: '',
    text: mapleLicense,
  },
]

/** 客户端中 target="_blank" 打不开系统浏览器，点击时改为交给它 */
function Link({ url }: { url: string }) {
  return (
    <a
      href={url}
      target="_blank"
      rel="noopener noreferrer"
      onClick={(e) => {
        e.preventDefault()
        openExternal(url)
      }}
      className="break-all text-blue-500 hover:underline"
    >
      {url}
    </a>
  )
}

/** 内置字体的许可；链接交给系统浏览器打开。Esc 或点遮罩关闭 */
export default function FontLicensesModal({ onClose }: { onClose: () => void }) {
  useEscape(onClose)

  return createPortal(
    <div className="fixed inset-0 z-[110] flex items-center justify-center p-4" onClick={onClose}>
      <div className="absolute inset-0 animate-overlay-in bg-black/20 backdrop-blur-md dark:bg-black/40" />
      <div
        role="dialog"
        aria-modal="true"
        className="relative z-10 flex max-h-[85vh] w-full max-w-2xl animate-confirm-in flex-col rounded-3xl border border-white/50 bg-white/90 shadow-[0_8px_40px_rgb(0,0,0,0.12)] ring-1 ring-black/5 backdrop-blur-xl dark:border-white/[0.08] dark:bg-gray-900/90 dark:shadow-[0_8px_40px_rgb(0,0,0,0.4)] dark:ring-white/10 sm:max-h-[90vh]"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="shrink-0 p-6 pb-2">
          <div className="flex items-center justify-between gap-4">
            <h3 className="text-base font-bold text-gray-800 dark:text-gray-100">字体与第三方许可</h3>
            <button
              type="button"
              onClick={onClose}
              className="shrink-0 rounded-full p-1 text-gray-400 transition hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-white/[0.06] dark:hover:text-gray-200"
              aria-label="关闭"
            >
              <CloseIcon className="h-5 w-5" />
            </button>
          </div>
        </div>

        <div className="flex-1 overflow-y-auto px-6 pb-6 pt-2 space-y-6 min-h-0">
          {FONT_LICENSES.map((font) => (
            <section key={font.name} className="space-y-2">
              <h4 className="text-sm font-bold text-gray-800 dark:text-gray-100">{font.name}</h4>
              <div data-selectable-text className="space-y-0.5 text-xs leading-relaxed text-gray-500 dark:text-gray-400">
                <div>版权方：{font.author}</div>
                <div>许可：{font.license}</div>
                <div>
                  项目地址：<Link url={font.url} />
                </div>
                {font.licenseUrl && (
                  <div>
                    许可证：<Link url={font.licenseUrl} />
                  </div>
                )}
              </div>
              <pre
                data-selectable-text
                className="max-h-72 overflow-y-auto whitespace-pre-wrap break-words rounded-xl bg-black/[0.04] p-3 font-mono text-[11px] leading-relaxed text-gray-600 dark:bg-white/[0.06] dark:text-gray-300"
              >
                {font.text}
              </pre>
            </section>
          ))}
        </div>
      </div>
    </div>,
    document.body,
  )
}
