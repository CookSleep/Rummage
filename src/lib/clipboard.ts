/**
 * 复制文字。只在点击处理中直接调用：用户激活有效时 navigator.clipboard 不会弹出 WebView2 的剪贴板权限框；
 * 接口不可用或被拒绝时退回 execCommand
 */
export async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text)
    return
  } catch (err) {
    if (copyWithExecCommand(text)) return
    throw err
  }
}

function copyWithExecCommand(text: string) {
  const el = document.createElement('textarea')
  el.value = text
  el.setAttribute('readonly', '')
  el.style.position = 'fixed'
  el.style.left = '-9999px'
  el.style.top = '0'
  document.body.appendChild(el)
  el.select()
  try {
    return document.execCommand('copy')
  } catch {
    return false
  } finally {
    document.body.removeChild(el)
  }
}
