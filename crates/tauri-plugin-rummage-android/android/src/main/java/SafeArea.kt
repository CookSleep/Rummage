package com.cooksleep.rummage.storage

import kotlin.math.roundToLong

/** 安全区四边的宽度（CSS 像素） */
data class Insets(val top: Double, val right: Double, val bottom: Double, val left: Double)

/**
 * 全面屏的安全区：WindowInsets 给出的是设备像素，页面缩放为 1 时 CSS 像素即 dp，按屏幕密度换算。
 * 写进 `<html>` 的 `--safe-area-inset-*`，由 src/index.css 的 `--safe-*` 使用
 */
object SafeArea {
  /** 换算成 CSS 像素，保留两位小数 */
  fun fromPx(top: Int, right: Int, bottom: Int, left: Int, density: Float): Insets {
    fun css(px: Int) = (px / density * 100).roundToLong() / 100.0
    return Insets(css(top), css(right), css(bottom), css(left))
  }

  /** 设置 CSS 变量的脚本 */
  fun script(i: Insets): String {
    val sides = listOf("top" to i.top, "right" to i.right, "bottom" to i.bottom, "left" to i.left)
    return "(function(s){" + sides.joinToString("") { (side, v) -> "s.setProperty('--safe-area-inset-$side','${v}px');" } + "})(document.documentElement.style)"
  }
}
