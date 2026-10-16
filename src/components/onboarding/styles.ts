// 引导各步骤共用的按钮与输入框样式

export const PRIMARY_BUTTON =
  'rounded-xl bg-blue-500 px-4 py-2 text-sm font-medium text-white transition duration-200 hover:bg-blue-600 disabled:cursor-not-allowed disabled:opacity-50'

export const BUTTON =
  'rounded-xl border border-transparent bg-black/[0.04] px-4 py-2 text-sm text-gray-600 transition duration-200 hover:bg-black/[0.07] disabled:cursor-not-allowed disabled:opacity-50 dark:bg-white/[0.06] dark:text-gray-300 dark:hover:bg-white/[0.1]'

export const INPUT =
  'w-full rounded-xl border border-transparent bg-black/[0.04] px-3 py-2 text-sm text-gray-700 outline-none transition duration-200 placeholder:text-gray-400 focus:border-blue-300 dark:bg-white/[0.06] dark:text-gray-200 dark:focus:border-blue-500/50'

/** 分段选择的外框：一行放不下时横向滑动，不换行 */
export const SEGMENTS = 'inline-flex max-w-full overflow-x-auto rounded-xl bg-black/[0.04] p-1 [scrollbar-width:none] dark:bg-white/[0.06]'

/** 分段选择中的一项 */
export function segmentClass(active: boolean) {
  return `shrink-0 whitespace-nowrap rounded-lg px-3 py-1.5 text-xs transition duration-200 ${active ? 'bg-white text-gray-800 shadow-sm dark:bg-white/[0.12] dark:text-gray-100' : 'text-gray-500 hover:text-gray-700 dark:text-gray-400 dark:hover:text-gray-200'}`
}

/** 错误提示 */
export const ERROR_TEXT = 'text-xs text-red-500 dark:text-red-400'

// 列表卡片

/** 卡片容器，行之间以细线分隔 */
export const CARD = 'overflow-hidden rounded-2xl border border-transparent bg-black/[0.04] divide-y divide-black/[0.05] dark:bg-white/[0.06] dark:divide-white/[0.06]'

/** 卡片中的一行；手机宽度下可换行，配合 `ROW_ACTIONS` 把右侧的按钮移到文字下方 */
export const CARD_ROW = 'flex min-h-[56px] w-full items-center gap-3 px-4 py-2.5 max-sm:flex-wrap max-sm:gap-y-2'

/** 卡片行右侧的按钮组（间距由使用处给出）：手机宽度下单独占一行，左缘与文字对齐（行首图标 32px 加间距 12px） */
export const ROW_ACTIONS = 'flex flex-shrink-0 items-center max-sm:basis-full max-sm:pl-11'

/** 卡片行中的长文字（路径、文件名）：宽屏时截断成一行，手机宽度下换行显示完整 */
export const ROW_LONG_TEXT = 'truncate max-sm:whitespace-normal max-sm:[overflow-wrap:anywhere]'

/**
 * 可点的行的悬停底色。只过渡底色：行由第一行变为后续行（如文件夹列表加载完成）时会加上分隔线，
 * 过渡边框颜色会让分隔线从默认的浅色渐变过来，闪一下白线
 */
export const CARD_ROW_HOVER = 'text-left outline-none transition-[background-color] hover:bg-black/[0.03] focus-visible:bg-black/[0.03] disabled:cursor-not-allowed disabled:opacity-50 dark:hover:bg-white/[0.04] dark:focus-visible:bg-white/[0.04]'

/** 行首的图标方块 */
export const ICON_TILE = 'flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-white text-gray-600 shadow-sm dark:bg-white/[0.08] dark:text-gray-300 dark:shadow-none'

/** 卡片里的小按钮 */
export const SMALL_BUTTON =
  'rounded-lg bg-white px-3 py-1.5 text-xs font-medium text-gray-700 shadow-sm transition hover:bg-gray-50 disabled:cursor-not-allowed disabled:opacity-50 dark:bg-white/[0.08] dark:text-gray-200 dark:shadow-none dark:hover:bg-white/[0.12]'

/** 卡片里的危险操作按钮：底色同 SMALL_BUTTON，文字为红色 */
export const SMALL_DANGER_BUTTON =
  'rounded-lg bg-white px-3 py-1.5 text-xs font-medium text-red-600 shadow-sm transition hover:bg-red-50 disabled:cursor-not-allowed disabled:opacity-50 dark:bg-white/[0.08] dark:text-red-400 dark:shadow-none dark:hover:bg-red-500/[0.15]'

/** 小号的 BUTTON（灰色底），用在弹出层等白色底上 */
export const SMALL_PLAIN_BUTTON =
  'rounded-lg border border-transparent bg-black/[0.04] px-3 py-1.5 text-xs text-gray-600 transition hover:bg-black/[0.07] disabled:cursor-not-allowed disabled:opacity-50 dark:bg-white/[0.06] dark:text-gray-300 dark:hover:bg-white/[0.1]'

export const SMALL_PRIMARY_BUTTON =
  'rounded-lg bg-blue-500 px-3 py-1.5 text-xs font-medium text-white shadow-sm transition hover:bg-blue-600 disabled:cursor-not-allowed disabled:opacity-50'

/** 行尾的图标按钮 */
export const ROW_ICON_BUTTON =
  'rounded-lg p-1.5 text-gray-400 transition hover:bg-black/[0.05] hover:text-gray-600 dark:text-gray-500 dark:hover:bg-white/[0.08] dark:hover:text-gray-200'
