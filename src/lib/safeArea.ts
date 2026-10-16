import { invoke } from '@tauri-apps/api/core'

/** 安全区四边的宽度（CSS 像素），对应 Rust `src-tauri/src/android/ui.rs` 的 `Insets` */
interface Insets {
  top: number
  right: number
  bottom: number
  left: number
}

/**
 * Android 全面屏（targetSdk 35 起强制）的安全区：Android WebView 140 以前 `env(safe-area-inset-*)` 拿不到正确的值，
 * 插件按 WindowInsets 算出状态栏、导航栏与挖孔的宽度，变化时直接写进 `<html>` 的 `--safe-area-inset-*`（index.css 的 `--safe-*`）。
 * 页面加载完成前算出的值写不进来，启动时在这里取一次
 */
export function installSafeArea() {
  invoke<Insets>('safe_area')
    .then((s) => {
      const style = document.documentElement.style
      for (const side of ['top', 'right', 'bottom', 'left'] as const) style.setProperty(`--safe-area-inset-${side}`, `${s[side]}px`)
    })
    .catch((err) => console.warn('无法读取安全区：', err))
}
