import type { Appearance, FontMode } from '../types'
import { readLocal, writeLocal } from './storage'

export type FontSlot = 'ui' | 'mono'

// 内置字体随应用分发，始终从本地字体文件加载
const FONT_SLOTS = {
  ui: {
    family: 'HarmonyOS Sans SC',
    fallback: "'Noto Sans SC', 'PingFang SC', 'Microsoft YaHei', sans-serif",
    cssVar: '--font-ui-sans',
    loadLocal: () => import('../../fonts/harmonyos-sans-sc.css'),
  },
  mono: {
    family: 'Maple Mono NF CN',
    fallback: "'Cascadia Code', 'SF Mono', Menlo, Consolas, monospace",
    cssVar: '--font-mono',
    loadLocal: () => import('../../fonts/maple-mono-nf-cn.css'),
  },
}

const DEFAULT_APPEARANCE: Appearance = { uiFont: 'builtin', uiFontName: '', monoFont: 'builtin', monoFontName: '' }

/** 外观设置在本地的缓存键：只为首次绘制前就用上字体，以核心中保存的为准 */
const APPEARANCE_KEY = 'rummage.appearance'

const FONT_MODES: FontMode[] = ['builtin', 'system', 'custom']

function quoteFontFamily(name: string) {
  return `"${name.replace(/["\\]/g, '\\$&')}"`
}

/** 内置字体用内置族名，自定义字体用用户填写的名称，未填写或选择系统字体时只用系统字体回退列表 */
export function buildFontStack(slot: FontSlot, mode: FontMode, name = '') {
  const cfg = FONT_SLOTS[slot]
  const family = mode === 'builtin' ? cfg.family : mode === 'custom' ? name.trim() : ''
  return family ? `${quoteFontFamily(family)}, ${cfg.fallback}` : cfg.fallback
}

/** 只有内置字体才加载字体文件；本地样式表加载后无法移除，但切到其他字体后 CSS 变量已不再引用内置字体 */
function applyFont(slot: FontSlot, mode: FontMode, name = '') {
  const cfg = FONT_SLOTS[slot]
  if (mode === 'builtin') cfg.loadLocal().catch((err) => console.error('本地字体加载失败：', err))
  document.documentElement.style.setProperty(cfg.cssVar, buildFontStack(slot, mode, name))
}

export function applyAppearance(a: Appearance) {
  applyFont('ui', a.uiFont, a.uiFontName)
  applyFont('mono', a.monoFont, a.monoFontName)
}

export function getFontLabel(mode: FontMode, name = '') {
  if (mode === 'custom') return name || '自定义'
  return mode === 'builtin' ? '内置字体' : '系统字体'
}

/** 读本地缓存；没有、读不到或内容不对时用内置字体 */
export function loadCachedAppearance(): Appearance {
  const raw = readLocal(APPEARANCE_KEY)
  if (!raw) return DEFAULT_APPEARANCE
  try {
    const v = JSON.parse(raw)
    const mode = (m: unknown) => (FONT_MODES.includes(m as FontMode) ? (m as FontMode) : 'builtin')
    const name = (n: unknown) => (typeof n === 'string' ? n : '')
    return { uiFont: mode(v?.uiFont), uiFontName: name(v?.uiFontName), monoFont: mode(v?.monoFont), monoFontName: name(v?.monoFontName) }
  } catch {
    return DEFAULT_APPEARANCE
  }
}

export function saveCachedAppearance(a: Appearance) {
  // 存储不可用时下次启动先用内置字体，拿到核心中的设置后再切换
  writeLocal(APPEARANCE_KEY, JSON.stringify(a))
}
