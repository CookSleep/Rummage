import type { ConfigImport } from '../types'

/** 导入配置后的胶囊通知，按丢弃的情况拼成一句 */
export function importMessage(r: ConfigImport) {
  const parts = [
    r.missing ? `${r.missing} 个文件夹不存在已跳过` : '',
    r.inaccessible ? `${r.inaccessible} 个文件夹无法访问已跳过` : '',
    r.failed ? `${r.failed} 个文件夹未能添加` : '',
    r.droppedParams ? `${r.droppedParams} 项参数无效已跳过` : '',
  ].filter(Boolean)
  return ['配置已导入', ...parts].join('，')
}
