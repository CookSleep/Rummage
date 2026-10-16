import { useEffect, useState, type KeyboardEvent, type RefObject } from 'react'
import type { ImageQuery, SimilarQuery } from '../types'
import { isAndroid } from '../lib/platform'
import { ClockIcon, CloseIcon, ImageIcon, SearchIcon, SimilarIcon, SpinnerIcon } from './icons'
import { HoverTip } from './Tip'

interface Props {
  inputRef: RefObject<HTMLInputElement | null>
  text: string
  onText: (text: string) => void
  /** 以图搜图与图片找相似的时附加的文字描述 */
  note: string
  onNote: (note: string) => void
  /** 以图搜图的图片 */
  image: ImageQuery | null
  /** 找相似的；清空时只清除它，回到原来的文字搜索 */
  similar: SimilarQuery | null
  onPickImage: () => void
  onClear: () => void
  loading: boolean
  /** 正在显示的最近搜索；为空时不显示列表 */
  history: string[]
  /** 点击空的输入框或在其中按向下键时打开列表，失去焦点时收起 */
  onHistoryOpen: (open: boolean) => void
  onPickHistory: (query: string) => void
  onRemoveHistory: (query: string) => void
}

const LIST_ID = 'search-history'

export default function SearchBox({ inputRef, text, onText, note, onNote, image, similar, onPickImage, onClear, loading, history, onHistoryOpen, onPickHistory, onRemoveHistory }: Props) {
  // 键盘选中的历史项，-1 为没有选中
  const [active, setActive] = useState(-1)
  const open = history.length > 0
  const describable = image !== null || similar?.describable === true

  useEffect(() => {
    setActive((a) => Math.min(a, history.length - 1))
  }, [history])

  useEffect(() => {
    if (!open) setActive(-1)
  }, [open])

  // 列表中的按键在这里处理完就停止传播，不再交给搜索页的全局快捷键
  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    const handled = () => {
      e.preventDefault()
      e.stopPropagation()
    }
    if (!open) {
      if (e.key === 'ArrowDown' && !text) {
        handled()
        onHistoryOpen(true)
      }
      return
    }
    if (e.key === 'ArrowDown') {
      handled()
      setActive((a) => (a + 1) % history.length)
    } else if (e.key === 'ArrowUp') {
      handled()
      setActive((a) => (a <= 0 ? history.length - 1 : a - 1))
    } else if (e.key === 'Enter' && active >= 0) {
      handled()
      onPickHistory(history[active])
    } else if ((e.key === 'Delete' || e.key === 'Backspace') && active >= 0) {
      // 列表只在输入框为空时显示，退格不会删到文字
      handled()
      onRemoveHistory(history[active])
    }
  }

  return (
    <div className="relative flex items-center">
      {loading ? (
        <SpinnerIcon className="pointer-events-none absolute left-3.5 h-4 w-4 animate-spin text-blue-500" />
      ) : (
        <SearchIcon className="pointer-events-none absolute left-3.5 h-4 w-4 text-gray-400" />
      )}
      {similar || image ? (
        // 以图搜图与普通图片找相似的时，标记后面可以接着输入文字描述，与图片合成一条查询
        <div className="flex w-full items-center gap-2 rounded-xl border border-blue-400 bg-black/[0.04] py-2 pl-10 pr-20 text-sm transition duration-200 focus-within:ring-2 focus-within:ring-blue-500/30 dark:bg-white/[0.06]">
          {similar ? <SimilarIcon kind={similar.kind} className="h-4 w-4 flex-shrink-0 text-blue-500" /> : <ImageIcon className="h-4 w-4 flex-shrink-0 text-blue-500" />}
          <span className={`truncate text-gray-700 dark:text-gray-200 ${describable ? 'max-w-[50%] flex-shrink-0' : ''}`}>
            {similar ? `相似：${similar.name}` : `以图搜图：${image?.name}`}
            <HoverTip label={similar?.name ?? image?.name ?? ''} wrap />
          </span>
          {describable && (
            <input
              ref={inputRef}
              type="text"
              value={note}
              onChange={(e) => onNote(e.target.value)}
              autoFocus={!isAndroid}
              spellCheck={false}
              maxLength={200}
              enterKeyHint="search"
              placeholder="加一句描述（可选）"
              aria-label="加一句描述"
              className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-gray-400 dark:text-white"
            />
          )}
        </div>
      ) : (
        <input
          ref={inputRef}
          type="text"
          role="combobox"
          aria-expanded={open}
          aria-controls={LIST_ID}
          aria-autocomplete="list"
          aria-activedescendant={active >= 0 ? `${LIST_ID}-${active}` : undefined}
          value={text}
          onChange={(e) => onText(e.target.value)}
          onClick={() => onHistoryOpen(true)}
          onBlur={() => onHistoryOpen(false)}
          onKeyDown={onKeyDown}
          // 手机上不自动弹出键盘
          autoFocus={!isAndroid}
          spellCheck={false}
          enterKeyHint="search"
          // 手机上不能拖入图片
          placeholder={isAndroid ? '搜索文件' : '搜索文件，或拖入图片以图搜图'}
          aria-label="搜索文件"
          className="w-full rounded-xl border border-transparent bg-black/[0.04] py-2.5 pl-10 pr-20 text-sm outline-none transition duration-200 placeholder:text-gray-400 focus:border-blue-400 focus:ring-2 focus:ring-blue-500/30 dark:bg-white/[0.06] dark:text-white"
        />
      )}
      <div className="absolute right-2 flex items-center gap-0.5">
        {(text || image || similar) && (
          <button type="button" onClick={onClear} aria-label={similar ? '取消找相似' : '清空搜索'} className="rounded-full p-1.5 text-gray-400 transition duration-200 hover:bg-black/5 hover:text-gray-600 dark:hover:bg-white/10 dark:hover:text-gray-200">
            <CloseIcon className="h-4 w-4" />
            <HoverTip label={similar ? '清除（Esc）' : '清空（Esc）'} />
          </button>
        )}
        <button type="button" onClick={onPickImage} aria-label="以图搜图" className="rounded-full p-1.5 text-gray-400 transition duration-200 hover:bg-black/5 hover:text-gray-600 dark:hover:bg-white/10 dark:hover:text-gray-200">
          <ImageIcon className="h-4 w-4" />
          <HoverTip label="以图搜图" />
        </button>
      </div>
      {open && (
        // 列表中按下鼠标不让输入框失焦，否则列表在点击生效前就收起了；高度放得下默认的 10 条
        <div
          id={LIST_ID}
          role="listbox"
          aria-label="最近的搜索"
          onMouseDown={(e) => e.preventDefault()}
          className="absolute left-0 top-full z-50 mt-1.5 max-h-[22.5rem] w-full animate-dropdown-down overflow-y-auto rounded-xl border border-transparent bg-white/95 shadow-[0_8px_30px_rgb(0,0,0,0.12)] ring-1 ring-black/5 backdrop-blur-xl dark:bg-gray-800/95 dark:shadow-[0_8px_30px_rgb(0,0,0,0.4)] dark:ring-white/[0.06]"
        >
          {history.map((q, i) => (
            <div
              key={q}
              id={`${LIST_ID}-${i}`}
              role="option"
              aria-selected={i === active}
              onClick={() => onPickHistory(q)}
              onMouseMove={() => setActive(i)}
              className={`group flex h-9 cursor-pointer items-center gap-2.5 px-3 text-sm text-gray-700 transition-colors dark:text-gray-300 ${i === active ? 'bg-black/[0.05] dark:bg-white/[0.08]' : ''}`}
            >
              <ClockIcon className="h-3.5 w-3.5 flex-shrink-0 text-gray-400 dark:text-gray-500" />
              <span className="min-w-0 flex-1 truncate">{q}</span>
              <button
                type="button"
                tabIndex={-1}
                onClick={(e) => {
                  e.stopPropagation()
                  onRemoveHistory(q)
                }}
                aria-label={`删除“${q}”`}
                className={`rounded-full p-1 text-gray-400 transition hover:bg-black/5 hover:text-gray-600 group-hover:opacity-100 dark:hover:bg-white/10 dark:hover:text-gray-200 [@media(hover:none)]:opacity-100 ${i === active ? 'opacity-100' : 'opacity-0'}`}
              >
                <CloseIcon className="h-3.5 w-3.5" />
                <HoverTip label="从历史中删除（Delete / Backspace）" />
              </button>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}
