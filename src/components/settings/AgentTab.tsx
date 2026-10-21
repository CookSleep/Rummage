import { useCallback, useEffect, useState, type ComponentType, type ReactNode, type SVGProps } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { AgentClient, AgentInfo, AgentState, SkillTarget } from '../../types'
import { copyText } from '../../lib/clipboard'
import { errorMessage } from '../../lib/errors'
import { trimPeriod } from '../../lib/format'
import { CheckIcon, ClaudeCodeIcon, ClaudeIcon, CodexIcon, CopyIcon, CursorIcon, FolderIcon, OpenCodeIcon, VscodeIcon } from '../icons'
import LoadError from '../LoadError'
import { CARD, CARD_ROW, ICON_TILE, ROW_ACTIONS, ROW_ICON_BUTTON, ROW_LONG_TEXT, SMALL_BUTTON } from '../onboarding/styles'
import PathText from '../PathText'
import { HoverTip } from '../Tip'
import { showToast } from '../Toast'
import { ROW_DESC, SECTION_TITLE } from './styles'

const CLIENT_LABEL: Record<AgentClient, string> = {
  claudeCode: 'Claude Code',
  codex: 'Codex',
  claudeDesktop: 'Claude Desktop',
  cursor: 'Cursor',
  vscode: 'VS Code',
  opencode: 'OpenCode',
}

const CLIENT_ICON: Record<AgentClient, ComponentType<SVGProps<SVGSVGElement>>> = {
  claudeCode: ClaudeCodeIcon,
  codex: CodexIcon,
  claudeDesktop: ClaudeIcon,
  cursor: CursorIcon,
  vscode: VscodeIcon,
  opencode: OpenCodeIcon,
}

const SKILL_LABEL: Record<SkillTarget, string> = {
  claude: 'Claude Code',
  agents: 'Codex 等',
}

/** 复制给用户手动添加的配置；写 `rummage-core.exe` 的完整路径，不依赖安装时是否加入 PATH */
function mcpConfig(client: AgentClient, exe: string) {
  const json = (v: unknown) => JSON.stringify(v, null, 2)
  if (client === 'claudeCode') return `claude mcp add --scope user rummage -- "${exe}" mcp`
  if (client === 'codex') return `codex mcp add rummage -- "${exe}" mcp`
  if (client === 'vscode') return json({ servers: { rummage: { type: 'stdio', command: exe, args: ['mcp'] } } })
  // OpenCode 2 也能读 1 的这种写法
  if (client === 'opencode') return json({ mcp: { rummage: { type: 'local', command: [exe, 'mcp'], enabled: true } } })
  return json({ mcpServers: { rummage: { command: exe, args: ['mcp'] } } })
}

const SUBTITLE = 'mt-0.5 block text-xs text-gray-500 dark:text-gray-400'
const SUBTITLE_WARN = 'mt-0.5 block text-xs text-amber-600 dark:text-amber-400/90'

/** 卡片中的一行：图标、名称、下方的路径或状态说明，右侧为操作 */
function Row({ icon, title, path, note, warn, children }: { icon: ReactNode; title: string; path?: string; note?: string; warn?: boolean; children: ReactNode }) {
  return (
    <div className={CARD_ROW}>
      <span className={ICON_TILE}>{icon}</span>
      <span className="min-w-0 flex-1">
        <span className="block text-sm text-gray-800 dark:text-gray-100">{title}</span>
        {note ? (
          <span className={warn ? SUBTITLE_WARN : SUBTITLE}>{note}</span>
        ) : (
          path && (
            <span data-selectable-text className={`${SUBTITLE} ${ROW_LONG_TEXT}`}>
              <PathText path={path} />
              <HoverTip label={path} wrap />
            </span>
          )
        )}
      </span>
      <span className={`gap-1.5 ${ROW_ACTIONS}`}>{children}</span>
    </div>
  )
}

const ADDED = (
  <span className="mr-1 inline-flex items-center gap-1 text-xs text-green-600 dark:text-green-400">
    <CheckIcon className="h-3.5 w-3.5" />
    已添加
  </span>
)

/**
 * “Agent”页：把 Rummage 的 MCP 服务添加到常用 AI 工具（检测到的工具可直接添加、更新与移除，都可复制配置手动添加），
 * 以及把随安装包附带的 Skill 安装到 Claude Code 或 `~/.agents/skills`。改动由 Rust 侧完成（`src-tauri/src/agent.rs`），
 * 只改名为 rummage 的那一项；不依赖后台服务
 */
export default function AgentTab() {
  const [info, setInfo] = useState<AgentInfo | null>(null)
  const [loadError, setLoadError] = useState('')
  // 正在处理的行，处理期间该行的按钮不可用
  const [busy, setBusy] = useState('')

  const load = useCallback(() => {
    invoke<AgentInfo>('agent_info')
      .then((i) => {
        setInfo(i)
        setLoadError('')
      })
      .catch((err) => {
        console.warn('无法读取 AI 工具的配置：', err)
        setLoadError(errorMessage(err, '无法读取 AI 工具的配置。'))
      })
  }, [])

  useEffect(load, [load])

  const act = (key: string, cmd: string, args: Record<string, string>, done: string) => {
    setBusy(key)
    invoke<AgentInfo>(cmd, args)
      .then((i) => {
        setInfo(i)
        showToast(done, 'success')
      })
      .catch((err) => {
        console.warn(`${cmd} 失败：`, err)
        showToast(trimPeriod(errorMessage(err, '操作失败，请重试。')), 'error')
      })
      .finally(() => setBusy(''))
  }

  const copy = async (text: string) => {
    try {
      await copyText(text)
      showToast('已复制配置', 'success')
    } catch (err) {
      console.warn(err)
      showToast('复制失败', 'error')
    }
  }

  const openDir = () => {
    invoke('open_skill_dir').catch((err) => {
      console.warn('打开 Skill 文件夹失败：', err)
      showToast(trimPeriod(errorMessage(err, '无法打开 Skill 所在的文件夹。')), 'error')
    })
  }

  if (loadError) return <LoadError message={loadError} onRetry={load} />
  if (!info) return null

  const note = (state: AgentState, invalid: string) => {
    if (state === 'notFound') return { note: '未检测到，可复制配置手动添加' }
    if (state === 'invalid') return { note: invalid, warn: true }
    if (state === 'outdated') return { note: '与当前版本不一致，需要更新', warn: true }
    return {}
  }

  return (
    <div className="space-y-8">
      <section className="space-y-3">
        <div className="space-y-1">
          <h4 className={SECTION_TITLE}>MCP</h4>
          <p className={ROW_DESC}>通过 MCP 让 AI 工具调用 Rummage 的语义搜索，按内容检索已索引的文件，并可将结果导出为 CSV、JSON 或路径列表，用于整理文件。</p>
        </div>
        <div className={CARD}>
          {info.clients.map((c) => {
            const label = CLIENT_LABEL[c.id]
            const Icon = CLIENT_ICON[c.id]
            const key = `mcp-${c.id}`
            return (
              <Row key={c.id} icon={<Icon className="h-4 w-4" />} title={label} path={c.path} {...note(c.state, '配置文件无法解析，可复制配置手动添加')}>
                {c.state === 'added' && ADDED}
                {(c.state === 'absent' || c.state === 'outdated') && (
                  <button type="button" className={SMALL_BUTTON} disabled={busy === key} onClick={() => act(key, 'mcp_add', { client: c.id }, `已添加，重新启动 ${label} 后生效`)}>
                    {c.state === 'absent' ? '添加' : '更新'}
                  </button>
                )}
                {(c.state === 'added' || c.state === 'outdated') && (
                  <button type="button" className={SMALL_BUTTON} disabled={busy === key} onClick={() => act(key, 'mcp_remove', { client: c.id }, `已移除，重新启动 ${label} 后生效`)}>
                    移除
                  </button>
                )}
                <button type="button" className={ROW_ICON_BUTTON} aria-label="复制配置" onClick={() => copy(mcpConfig(c.id, info.coreExe))}>
                  <CopyIcon className="h-4 w-4" />
                  <HoverTip label="复制配置" />
                </button>
              </Row>
            )
          })}
        </div>
      </section>

      <section className="space-y-3">
        <div className="space-y-1">
          <h4 className={SECTION_TITLE}>Skill</h4>
          <p className={ROW_DESC}>Skill 为 AI 工具提供 Rummage 的使用说明，包括何时调用、如何检索与导出结果。Claude Code 读取「~\.claude\skills」，Codex 读取「~\.agents\skills」，Cursor、VS Code 与 OpenCode 两处都会读取，安装其中一处即可。</p>
        </div>
        <div className={CARD}>
          {info.skills.map((s) => {
            const key = `skill-${s.id}`
            return (
              <Row key={s.id} icon={s.id === 'claude' ? <ClaudeCodeIcon className="h-4 w-4" /> : <CodexIcon className="h-4 w-4" />} title={SKILL_LABEL[s.id]} path={s.path} {...note(s.state, '此目录已有另一个名为 rummage 的 Skill')}>
                {s.state === 'added' && ADDED}
                {(s.state === 'absent' || s.state === 'outdated') && (
                  <button type="button" className={SMALL_BUTTON} disabled={busy === key} onClick={() => act(key, 'skill_install', { target: s.id }, s.state === 'absent' ? '已安装' : '已更新')}>
                    {s.state === 'absent' ? '安装' : '更新'}
                  </button>
                )}
                {(s.state === 'added' || s.state === 'outdated') && (
                  <button type="button" className={SMALL_BUTTON} disabled={busy === key} onClick={() => act(key, 'skill_remove', { target: s.id }, '已移除')}>
                    移除
                  </button>
                )}
              </Row>
            )
          })}
          <Row icon={<FolderIcon className="h-4 w-4" />} title="Skill 文件夹" path={info.skillDir}>
            <button type="button" className={SMALL_BUTTON} onClick={openDir}>
              打开
            </button>
          </Row>
        </div>
      </section>
    </div>
  )
}
