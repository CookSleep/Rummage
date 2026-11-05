// 下拉面板分两层：圆角外壳只负责裁切，滚动交给内层，滚动条才不会伸出圆角；
// 最大高度设在外壳上，含边框在内
export const DROPDOWN_PANEL_CLASS = 'flex flex-col overflow-hidden rounded-xl border border-transparent bg-white/95 shadow-[0_8px_30px_rgb(0,0,0,0.12)] ring-1 ring-black/5 backdrop-blur-xl dark:bg-gray-800/95 dark:shadow-[0_8px_30px_rgb(0,0,0,0.4)] dark:ring-white/[0.06]'
// 内层按外壳的内圆角（12px 减 1px 边框）裁切
export const DROPDOWN_SCROLL_CLASS = 'min-h-0 overflow-y-auto rounded-[11px]'
