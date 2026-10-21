import type { ReactNode } from 'react'
import { formatSize } from '../../lib/format'
import { type Kinds, VIDEO_PRESETS } from '../../lib/kinds'
import { isAndroid } from '../../lib/platform'
import { renderMessage } from '../ConfirmDialog'
import { AudioIcon, CheckIcon, DocumentIcon, ImageIcon, VideoIcon } from '../icons'

interface CardProps {
  icon: ReactNode
  title: string
  desc: string
  checked: boolean
  onToggle: () => void
}

/** 选项卡片：未选中时与设置里的卡片同底色，选中时用蓝色底与蓝色图标方块，右侧是复选框 */
function KindCard({ icon, title, desc, checked, onToggle }: CardProps) {
  return (
    <button
      type="button"
      aria-pressed={checked}
      onClick={onToggle}
      className={`flex items-center gap-3 rounded-2xl border border-transparent px-4 py-3 text-left outline-none transition-colors duration-200 ${checked ? 'bg-blue-500/[0.08] hover:bg-blue-500/[0.12] focus-visible:bg-blue-500/[0.12] dark:bg-blue-500/[0.12] dark:hover:bg-blue-500/[0.16]' : 'bg-black/[0.04] hover:bg-black/[0.06] focus-visible:bg-black/[0.06] dark:bg-white/[0.06] dark:hover:bg-white/[0.08]'}`}
    >
      <span className={`flex h-8 w-8 shrink-0 items-center justify-center rounded-lg ${checked ? 'bg-blue-500 text-white' : 'bg-white text-gray-600 shadow-sm dark:bg-white/[0.08] dark:text-gray-300 dark:shadow-none'}`}>{icon}</span>
      <span className="min-w-0 flex-1">
        <span className={`block text-sm ${checked ? 'font-medium text-blue-600 dark:text-blue-400' : 'text-gray-800 dark:text-gray-100'}`}>{title}</span>
        <span className="mt-0.5 block text-xs text-gray-500 dark:text-gray-400">{desc}</span>
      </span>
      <span className={`flex h-4 w-4 flex-shrink-0 items-center justify-center rounded-[4px] border transition-all duration-200 ${checked ? 'border-transparent bg-blue-500 text-white' : 'border-gray-300 bg-white dark:border-white/15 dark:bg-white/5'}`}>
        {checked && <CheckIcon className="h-2.5 w-2.5" />}
      </span>
    </button>
  )
}

/** 视频识别方式的单选卡片，样式同 `KindCard` 的选中态，右侧是圆形单选框 */
function PresetCard({ title, desc, checked, onSelect }: { title: string; desc: string; checked: boolean; onSelect: () => void }) {
  return (
    <button
      type="button"
      role="radio"
      aria-checked={checked}
      onClick={onSelect}
      className={`flex items-start gap-2 rounded-2xl border border-transparent px-3 py-2.5 text-left outline-none transition-colors duration-200 ${checked ? 'bg-blue-500/[0.08] hover:bg-blue-500/[0.12] focus-visible:bg-blue-500/[0.12] dark:bg-blue-500/[0.12] dark:hover:bg-blue-500/[0.16]' : 'bg-black/[0.04] hover:bg-black/[0.06] focus-visible:bg-black/[0.06] dark:bg-white/[0.06] dark:hover:bg-white/[0.08]'}`}
    >
      <span className="min-w-0 flex-1">
        <span className={`block text-sm ${checked ? 'font-medium text-blue-600 dark:text-blue-400' : 'text-gray-800 dark:text-gray-100'}`}>{title}</span>
        <span className="mt-0.5 block text-xs text-gray-500 dark:text-gray-400">{desc}</span>
      </span>
      <span className={`mt-0.5 flex h-4 w-4 flex-shrink-0 items-center justify-center rounded-full border transition-all duration-200 ${checked ? 'border-blue-500 bg-white dark:bg-white/5' : 'border-gray-300 bg-white dark:border-white/15 dark:bg-white/5'}`}>
        {checked && <span className="h-2 w-2 rounded-full bg-blue-500" />}
      </span>
    </button>
  )
}

interface Props {
  kinds: Kinds
  mmprojBytes: number
  mmprojReady: boolean
  /** 设置页中用分区标题，引导中用步骤标题 */
  section?: boolean
  onChange: (key: keyof Kinds, on: boolean) => void
  /** 当前的视频预设（参数 `video_preset`）；给出 `onPreset` 时显示识别方式 */
  preset?: number
  onPreset?: (value: number) => void
}

/**
 * 第 2 步：希望查找哪些类型的文件。选了图片、音频或视频才需要多模态模型。
 * 引导中选了视频时接着选识别方式（视频预设）；设置中的识别方式在“索引”页
 */
export default function KindsStep({ kinds, mmprojBytes, mmprojReady, section, onChange, preset, onPreset }: Props) {
  const icon = 'h-4 w-4'
  const desc = '选中的类型会识别内容，其余文件仍可按文件名搜索。'
  // Android 上一个模型包含全部模态，不另外下载
  const extra = mmprojReady || isAndroid ? '' : `；需另外下载约 ${formatSize(mmprojBytes)} 的图片与音视频模型`
  const cards: { key: keyof Kinds; icon: ReactNode; title: string; desc: string }[] = [
    { key: 'documents', icon: <DocumentIcon className={icon} />, title: '文档', desc: 'PDF、Office 与文本文件' },
    { key: 'images', icon: <ImageIcon className={icon} />, title: '图片', desc: `照片、截图、图表等${extra}` },
    { key: 'audio', icon: <AudioIcon className={icon} />, title: '音频', desc: `录音、音乐、语音消息等${extra}` },
    { key: 'video', icon: <VideoIcon className={icon} />, title: '视频', desc: `找到视频中的某个片段${extra}` },
  ]
  return (
    <div>
      {section ? (
        <>
          <h4 className="text-sm font-bold text-gray-800 dark:text-gray-100">文件类型</h4>
          <p className="mt-1 text-xs text-gray-500 dark:text-gray-500">{desc}</p>
        </>
      ) : (
        <>
          <h2 className="text-base font-bold text-gray-800 dark:text-gray-100">希望按内容查找哪些文件</h2>
          <p className="mt-1 text-sm text-gray-500 dark:text-gray-400">{desc}</p>
        </>
      )}
      <div className="mt-4 grid gap-3 sm:grid-cols-2">
        {cards.map((c) => (
          <KindCard key={c.key} icon={c.icon} title={c.title} desc={c.desc} checked={kinds[c.key]} onToggle={() => onChange(c.key, !kinds[c.key])} />
        ))}
      </div>
      {kinds.video && onPreset && (
        <div className="mt-5">
          <p className="text-sm text-gray-700 dark:text-gray-200">找视频时，你最常想找什么</p>
          <p className="mt-1 text-xs text-gray-500 dark:text-gray-400">{renderMessage('之后可在「设置 → 索引」中修改')}</p>
          <div role="radiogroup" aria-label="视频识别方式" className="mt-3 grid gap-2 sm:grid-cols-3">
            {VIDEO_PRESETS.map((p) => (
              <PresetCard key={p.value} title={p.title} desc={p.desc} checked={preset === p.value} onSelect={() => onPreset(p.value)} />
            ))}
          </div>
        </div>
      )}
    </div>
  )
}
