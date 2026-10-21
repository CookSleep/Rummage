import { invoke } from '@tauri-apps/api/core'
import { copyText } from './clipboard'
import { errorMessage } from './errors'
import { showToast } from '../components/Toast'

/** 在文件管理器中打开文件所在的文件夹；打不开（如 Android 上没有能接收的文件管理器）时改为复制路径 */
export async function revealFile(fileId: number, path: string) {
  try {
    await invoke('reveal_file', { fileId })
  } catch (err) {
    console.warn('无法打开文件所在的文件夹：', err)
    const msg = errorMessage(err, '无法打开文件所在的文件夹')
    try {
      await copyText(path)
      showToast(`${msg.replace(/。$/, '')}，已复制路径`, 'info')
    } catch (copyErr) {
      console.warn('复制路径失败：', copyErr)
      showToast(msg, 'error')
    }
  }
}
