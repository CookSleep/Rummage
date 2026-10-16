// 本地存储（localStorage）的读写：只存每个用户自己的显示偏好与缓存，存储不可用时不影响使用

/** 读一项；没有这一项时为 null，存储不可用时为 `fallback` */
export function readLocal(key: string, fallback: string | null = null) {
  try {
    return localStorage.getItem(key)
  } catch {
    return fallback
  }
}

/** 写一项；存储不可用时只在本次运行中生效 */
export function writeLocal(key: string, value: string) {
  try {
    localStorage.setItem(key, value)
  } catch (err) {
    console.warn('无法保存到本地存储：', err)
  }
}
