# 配置 Rummage 的 MCP 服务与本 Skill

Rummage 的 MCP 服务是 `rummage-core mcp`，走 stdio，不监听网络端口。

最省事的是在 Rummage 的“设置 → Agent”里操作：检测到的客户端可以直接添加 MCP，Skill 一键安装；其他客户端可复制写好完整路径的配置。下面是手动配置的写法。

- 只有安装时勾选了“添加到 PATH”，配置中才能直接写 `rummage-core`；否则写安装目录中 `rummage-core.exe` 的完整路径，默认为 `C:\Users\<用户名>\AppData\Local\Rummage\rummage-core.exe`。
- 安装或卸载后 PATH 才会变化，已经打开的客户端（终端、Claude Code、Claude Desktop、VS Code 等）读不到新的 PATH，要重新启动后才能找到 `rummage-core`。

## Claude Code

```
claude mcp add --scope user rummage -- rummage-core mcp
```

- `--scope user` 对本机所有项目生效；只想在当前项目中用时改为 `--scope local`（默认）或 `--scope project`（写入项目的 `.mcp.json`）。
- 移除：`claude mcp remove rummage --scope user`。
- 工具名形如 `mcp__rummage__search_files`，其余工具同理。

## Claude Desktop

配置文件为 `%APPDATA%\Claude\claude_desktop_config.json`（也可以在 Claude Desktop 的“设置 › 开发者 › 编辑配置”中打开），在 `mcpServers` 中加入：

```json
{
  "mcpServers": {
    "rummage": {
      "command": "rummage-core",
      "args": ["mcp"]
    }
  }
}
```

保存后完全退出 Claude Desktop（包括托盘图标）再打开。

## VS Code

工作区的 `.vscode/mcp.json`（或用户配置中的 MCP 设置）用的键是 `servers`，不是 `mcpServers`：

```json
{
  "servers": {
    "rummage": {
      "type": "stdio",
      "command": "rummage-core",
      "args": ["mcp"]
    }
  }
}
```

没有加入 PATH 时，`command` 写完整路径，JSON 中反斜杠要写两个，例如 `C:\\Users\\<用户名>\\AppData\\Local\\Rummage\\rummage-core.exe`。

## 安装本 Skill

Skill 随安装包放在安装目录的 `skills\rummage\` 中。Claude Code 读取 `~\.claude\skills`，Codex 读取 `~\.agents\skills`，Cursor、VS Code、OpenCode 两处都读。手动安装时复制过去即可，例如：

```powershell
Copy-Item -Recurse "$env:LOCALAPPDATA\Rummage\skills\rummage" "$HOME\.claude\skills\rummage"
```

Rummage 更新后，“设置 → Agent”会提示更新 Skill。

## 工具

| 工具 | 作用 |
|---|---|
| `search_files` | 按内容或图片搜索，可按类型、文件夹、修改日期筛选；默认 10 条、最多 50 条，用 `offset` 翻页 |
| `save_search_results` | 同样的搜索，把全部结果（最多 500 条）存成新的 CSV、JSON 或路径列表文件 |
| `get_hit_context` | 按 `hit_id` 取文档命中处已索引的正文与前后段落 |
| `index_status` | 已索引的文件夹、进度、失败数、推理设备与暂停原因 |
| `list_folders` | 已索引的文件夹（文件数、排除规则）、识别内容的文件类型、索引进度 |
| `add_folder` | 添加文件夹，可带排除规则与文件类型 |
| `update_folder` | 修改文件夹的排除规则（整体替换）或文件类型 |
| `remove_folder` | 移除文件夹的索引，不删除文件 |
| `get_settings` | 参数的当前值、默认值、范围、是否影响索引与说明 |
| `set_setting` | 修改一个参数，按范围校验；影响索引的会在后台重建 |
| `pause_indexing` / `resume_indexing` | 暂停（可指定分钟数）与恢复建立索引 |
| `list_failed_files` / `retry_failed_files` | 建立索引失败的文件与原因；全部重试 |

管理类工具只改 Rummage 自己的索引与设置；设备、模型、清空索引和数据目录在 Rummage 里改。`save_search_results` 只新建文件，不覆盖已有文件。片段与正文放在 `<snippet>`、`<file_content>` 块中。

## 常见问题

- **第一次调用较慢**：后台服务没在运行时由 MCP 服务拉起，要几秒。
- **找不到 `rummage-core`**：没有勾选“添加到 PATH”，或客户端是在安装之前打开的，见上文。
- **“这不是 Rummage 的数据目录”或“找不到数据目录”**：先打开一次 Rummage，完成设置。
- **结果为空**：用 `index_status` 确认文件夹已添加、索引已完成、模型已下载。
