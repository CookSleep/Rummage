import { useEffect } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { AgentInfo } from '../types'
import { isAndroid, isTauri } from '../lib/platform'
import { SKILL_LABEL } from '../components/settings/AgentTab'
import { showToast } from '../components/Toast'

let started = false

/**
 * 安装的 Skill 是复制过去的，不会随客户端升级；启动时把内容已过时的更新为当前版本，未安装的不处理。
 * 各处分别更新，一处失败不影响另一处；失败时原有的 Skill 保持不变。开发模式下 StrictMode 会执行两次 effect，用模块级标记保证只更新一次
 */
export function useSkillAutoUpdate() {
  useEffect(() => {
    if (!isTauri || isAndroid || started) return
    started = true
    void invoke<AgentInfo>('agent_info')
      .then(async (info) => {
        const outdated = info.skills.filter((s) => s.state === 'outdated')
        if (!outdated.length) return
        const failed: string[] = []
        for (const s of outdated) {
          try {
            await invoke('skill_install', { target: s.id })
          } catch (err) {
            console.warn(`自动更新 ${SKILL_LABEL[s.id]} 的 Skill 失败：`, err)
            failed.push(SKILL_LABEL[s.id])
          }
        }
        showToast(
          failed.length ? `Skill 自动更新失败（${failed.join('、')}），可在「设置 → Agent」中手动更新` : 'Skill 已更新到当前版本',
          failed.length ? 'error' : 'success',
        )
      })
      .catch((err) => console.warn('读取 Skill 的安装状态失败：', err))
  }, [])
}
