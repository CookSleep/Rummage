//! 设置页“Agent”的命令：把 Rummage 的 MCP 服务添加到常用 AI 工具，把随安装包附带的 Skill 安装到 AI 工具读取 Skill 的目录。
//! - MCP：每个客户端一个配置文件，客户端的配置目录存在时才算检测到。添加、移除只改名为 `rummage` 的这一项，
//!   JSON 用 CST 编辑、TOML 用 toml_edit，保留文件中其余内容、注释与排版；文件解析不了时不写入，界面只提供复制配置。
//!   写入先写同目录的临时文件再替换，不会留下写了一半的文件。命令写 `rummage-core.exe` 的完整路径，不依赖 PATH。
//! - Claude Desktop 的 MSIX 版把 `%APPDATA%` 的写入重定向到 `%LOCALAPPDATA%\Packages\Claude_…\LocalCache\Roaming`，
//!   有这个目录时写在那里。OpenCode 2 仍读 1 的 `mcp.<名称>` 写法，文件中已有 2 的 `mcp.servers` 时才写到其中。
//! - Skill：复制到 `~\.claude\skills\rummage`（Claude Code）或 `~\.agents\skills\rummage`（Codex 等；Cursor、VS Code、OpenCode 两处都读）。
//!   目标文件夹的 SKILL.md 不是 Rummage 的时不覆盖、不删除。安装包中的 Skill 在资源目录下的 `skills\rummage`；
//!   开发版没有打包资源，直接用仓库中的 `skills/rummage`。
//! - 只在桌面端提供：命令行与 MCP 只在 Windows 上提供
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jsonc_parser::cst::{CstInputValue, CstObject, CstRootNode};
use jsonc_parser::{ParseOptions, json};
use rummage_proto::ApiError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager, Runtime};

use crate::search::error;

/// 写进各客户端配置中的服务名，也是 Skill 的文件夹名
const NAME: &str = "rummage";

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Client {
    ClaudeCode,
    Codex,
    ClaudeDesktop,
    Cursor,
    Vscode,
    Opencode,
}

const CLIENTS: [Client; 6] = [
    Client::ClaudeCode,
    Client::Codex,
    Client::ClaudeDesktop,
    Client::Cursor,
    Client::Vscode,
    Client::Opencode,
];

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SkillTarget {
    /// `~\.claude\skills`，Claude Code
    Claude,
    /// `~\.agents\skills`，Codex 等
    Agents,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
enum State {
    /// 没有检测到客户端（MCP）
    NotFound,
    /// 还没有添加或安装
    Absent,
    /// 已添加或安装，与当前一致
    Added,
    /// 已有 `rummage` 一项，但命令路径或 Skill 内容与当前不同
    Outdated,
    /// 配置文件解析不了，或同名文件夹不是 Rummage 的 Skill
    Invalid,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct McpStatus {
    id: Client,
    path: String,
    state: State,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SkillStatus {
    id: SkillTarget,
    path: String,
    state: State,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    core_exe: String,
    skill_dir: String,
    clients: Vec<McpStatus>,
    skills: Vec<SkillStatus>,
}

fn env_dir(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn home() -> Option<PathBuf> {
    env_dir("USERPROFILE").or_else(|| env_dir("HOME"))
}

/// Claude Desktop 的配置目录：MSIX 版的重定向目录优先
fn claude_desktop_dir() -> Option<PathBuf> {
    let packaged = env_dir("LOCALAPPDATA").and_then(|local| {
        fs::read_dir(local.join("Packages"))
            .ok()?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("Claude_"))
            .map(|e| e.path().join("LocalCache").join("Roaming").join("Claude"))
            .find(|dir| dir.is_dir())
    });
    packaged.or_else(|| env_dir("APPDATA").map(|dir| dir.join("Claude")))
}

/// 客户端的配置目录（存在时算检测到）与配置文件
fn client_paths(client: Client) -> Option<(PathBuf, PathBuf)> {
    match client {
        Client::ClaudeCode => {
            let home = home()?;
            Some((home.join(".claude"), home.join(".claude.json")))
        }
        Client::Codex => {
            let dir = home()?.join(".codex");
            Some((dir.clone(), dir.join("config.toml")))
        }
        Client::ClaudeDesktop => {
            let dir = claude_desktop_dir()?;
            Some((dir.clone(), dir.join("claude_desktop_config.json")))
        }
        Client::Cursor => {
            let dir = home()?.join(".cursor");
            Some((dir.clone(), dir.join("mcp.json")))
        }
        Client::Vscode => {
            let dir = env_dir("APPDATA")?.join("Code").join("User");
            Some((dir.clone(), dir.join("mcp.json")))
        }
        Client::Opencode => {
            let dir = home()?.join(".config").join("opencode");
            let jsonc = dir.join("opencode.jsonc");
            let file = if jsonc.is_file() {
                jsonc
            } else {
                dir.join("opencode.json")
            };
            Some((dir, file))
        }
    }
}

// ---- JSON 配置 ----

fn parse_json(text: &str) -> Option<CstRootNode> {
    let root = CstRootNode::parse(text, &ParseOptions::default()).ok()?;
    // 空文件当作空对象；顶层不是对象的不改
    if root.value().is_some() && root.object_value().is_none() {
        return None;
    }
    Some(root)
}

/// 存放各服务的对象所在的键；OpenCode 2 的写法是 `mcp.servers`
fn servers_path(client: Client, root: &CstRootNode) -> &'static [&'static str] {
    match client {
        Client::Vscode => &["servers"],
        Client::Opencode => {
            let v2 = root
                .object_value()
                .and_then(|o| o.object_value("mcp"))
                .is_some_and(|mcp| mcp.object_value("servers").is_some());
            if v2 { &["mcp", "servers"] } else { &["mcp"] }
        }
        _ => &["mcpServers"],
    }
}

/// 各客户端的一项配置；OpenCode 的命令是数组，1 的写法要带 `enabled`
fn json_entry(client: Client, root: &CstRootNode, exe: &str) -> CstInputValue {
    match client {
        Client::Vscode | Client::ClaudeCode => {
            json!({ "type": "stdio", "command": exe, "args": ["mcp"] })
        }
        Client::Opencode if servers_path(client, root).len() == 2 => {
            json!({ "type": "local", "command": [exe, "mcp"] })
        }
        Client::Opencode => json!({ "type": "local", "command": [exe, "mcp"], "enabled": true }),
        _ => json!({ "command": exe, "args": ["mcp"] }),
    }
}

/// 已有一项是否就是当前要写的命令；路径不区分大小写
fn json_matches(client: Client, entry: &Value, exe: &str) -> bool {
    let same = |v: &Value| v.as_str().is_some_and(|s| s.eq_ignore_ascii_case(exe));
    if client == Client::Opencode {
        let cmd = entry.get("command").and_then(Value::as_array);
        return cmd.is_some_and(|c| c.len() == 2 && same(&c[0]) && c[1] == "mcp");
    }
    entry.get("command").is_some_and(same) && entry.get("args") == Some(&serde_json::json!(["mcp"]))
}

fn json_servers(root: &CstRootNode, path: &[&str]) -> Option<CstObject> {
    path.iter()
        .try_fold(root.object_value()?, |obj, key| obj.object_value(key))
}

fn json_state(client: Client, text: &str, exe: &str) -> State {
    let Some(root) = parse_json(text) else {
        return State::Invalid;
    };
    let path = servers_path(client, &root);
    // 中间的键存在但不是对象时，写入会覆盖它，不改
    let mut obj = root.object_value();
    for key in path {
        let Some(o) = obj else { return State::Absent };
        if o.get(key).is_some() && o.object_value(key).is_none() {
            return State::Invalid;
        }
        obj = o.object_value(key);
    }
    let entry = obj
        .and_then(|o| o.get(NAME))
        .and_then(|p| p.value())
        .and_then(|v| v.to_serde_value());
    match entry {
        None => State::Absent,
        Some(e) if json_matches(client, &e, exe) => State::Added,
        Some(_) => State::Outdated,
    }
}

fn json_add(client: Client, text: &str, exe: &str) -> Option<String> {
    let root = parse_json(text)?;
    let entry = json_entry(client, &root, exe);
    let obj = servers_path(client, &root)
        .iter()
        .fold(root.object_value_or_set(), |obj, key| {
            obj.object_value_or_set(key)
        });
    match obj.get(NAME) {
        Some(prop) => prop.set_value(entry),
        None => {
            obj.append(NAME, entry);
        }
    }
    Some(root.to_string())
}

fn json_remove(client: Client, text: &str) -> Option<String> {
    let root = parse_json(text)?;
    if let Some(prop) = json_servers(&root, servers_path(client, &root)).and_then(|o| o.get(NAME)) {
        prop.remove();
    }
    Some(root.to_string())
}

// ---- TOML 配置（Codex） ----

fn toml_state(text: &str, exe: &str) -> State {
    let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else {
        return State::Invalid;
    };
    let Some(servers) = doc.get("mcp_servers") else {
        return State::Absent;
    };
    let Some(servers) = servers.as_table_like() else {
        return State::Invalid;
    };
    let Some(entry) = servers.get(NAME) else {
        return State::Absent;
    };
    let cmd = entry.get("command").and_then(|v| v.as_str());
    let args: Option<Vec<_>> = entry
        .get("args")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|v| v.as_str()).collect());
    if cmd.is_some_and(|c| c.eq_ignore_ascii_case(exe)) && args == Some(vec![Some("mcp")]) {
        return State::Added;
    }
    State::Outdated
}

fn toml_add(text: &str, exe: &str) -> Option<String> {
    let mut doc = text.parse::<toml_edit::DocumentMut>().ok()?;
    let servers = doc.entry("mcp_servers").or_insert_with(|| {
        let mut t = toml_edit::Table::new();
        // 只写出 `[mcp_servers.rummage]`，不单独写一行 `[mcp_servers]`
        t.set_implicit(true);
        toml_edit::Item::Table(t)
    });
    let servers = servers.as_table_like_mut()?;
    let mut entry = toml_edit::Table::new();
    entry["command"] = toml_edit::value(exe);
    entry["args"] = toml_edit::value(toml_edit::Array::from_iter(["mcp"]));
    servers.insert(NAME, toml_edit::Item::Table(entry));
    Some(doc.to_string())
}

fn toml_remove(text: &str) -> Option<String> {
    let mut doc = text.parse::<toml_edit::DocumentMut>().ok()?;
    if let Some(servers) = doc
        .get_mut("mcp_servers")
        .and_then(|s| s.as_table_like_mut())
    {
        servers.remove(NAME);
    }
    Some(doc.to_string())
}

// ---- 读写文件 ----

fn read_text(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        other => other,
    }
}

/// 先写临时文件再替换，客户端不会读到写了一半的配置
fn write_text(path: &Path, text: &str) -> io::Result<()> {
    let tmp = path.with_extension("rummage-tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

fn mcp_status(client: Client, exe: &str) -> McpStatus {
    let Some((dir, file)) = client_paths(client) else {
        return McpStatus {
            id: client,
            path: String::new(),
            state: State::NotFound,
        };
    };
    let path = file.to_string_lossy().into_owned();
    if !dir.is_dir() {
        return McpStatus {
            id: client,
            path,
            state: State::NotFound,
        };
    }
    let state = match read_text(&file) {
        Ok(text) if client == Client::Codex => toml_state(&text, exe),
        Ok(text) => json_state(client, &text, exe),
        Err(err) => {
            log::warn!("无法读取 MCP 配置 {}：{err}", file.display());
            State::Invalid
        }
    };
    McpStatus {
        id: client,
        path,
        state,
    }
}

fn edit_mcp(client: Client, exe: &str, add: bool) -> Result<(), ApiError> {
    let Some((dir, file)) = client_paths(client).filter(|(dir, _)| dir.is_dir()) else {
        return Err(error(
            ApiError::BAD_REQUEST,
            "没有检测到这个 AI 工具，请复制配置后手动添加。",
        ));
    };
    let text = read_text(&file).map_err(|err| {
        log::warn!("无法读取 MCP 配置 {}：{err}", file.display());
        error(
            ApiError::INTERNAL,
            "无法读取这个 AI 工具的配置文件，请复制配置后手动添加。",
        )
    })?;
    let codex = client == Client::Codex;
    let state = if codex {
        toml_state(&text, exe)
    } else {
        json_state(client, &text, exe)
    };
    if state == State::Invalid {
        return Err(error(
            ApiError::BAD_REQUEST,
            "这个 AI 工具的配置文件格式有误，请复制配置后手动添加。",
        ));
    }
    let next = match (codex, add) {
        (true, true) => toml_add(&text, exe),
        (true, false) => toml_remove(&text),
        (false, true) => json_add(client, &text, exe),
        (false, false) => json_remove(client, &text),
    };
    let next = next.expect("上面已确认能解析");
    if next == text {
        return Ok(());
    }
    fs::create_dir_all(&dir)
        .and_then(|_| write_text(&file, &next))
        .map_err(|err| {
            log::warn!("无法写入 MCP 配置 {}：{err}", file.display());
            error(
                ApiError::INTERNAL,
                "无法写入这个 AI 工具的配置文件，请关闭它后重试，或复制配置后手动添加。",
            )
        })
}

// ---- Skill ----

fn bundled_skill<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, ApiError> {
    if cfg!(debug_assertions) {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri 在仓库根目录下");
        return Ok(repo.join("skills").join(NAME));
    }
    let res = app.path().resource_dir().map_err(|err| {
        log::warn!("无法取得资源目录：{err}");
        error(
            ApiError::INTERNAL,
            "找不到 Skill 所在的目录，请重新安装 Rummage。",
        )
    })?;
    Ok(res.join("skills").join(NAME))
}

fn skill_path(target: SkillTarget) -> Option<PathBuf> {
    let parent = match target {
        SkillTarget::Claude => ".claude",
        SkillTarget::Agents => ".agents",
    };
    Some(home()?.join(parent).join("skills").join(NAME))
}

/// 文件夹中的 SKILL.md 是 Rummage 的，才能覆盖或删除
fn is_ours(dir: &Path) -> bool {
    fs::read_to_string(dir.join("SKILL.md"))
        .is_ok_and(|s| s.lines().any(|l| l.trim() == "name: rummage"))
}

/// 安装包中的每个文件都在目标中且内容相同
fn same_files(src: &Path, dst: &Path) -> io::Result<bool> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        let same = if entry.file_type()?.is_dir() {
            to.is_dir() && same_files(&entry.path(), &to)?
        } else {
            fs::read(entry.path())? == fs::read(&to).unwrap_or_default()
        };
        if !same {
            return Ok(false);
        }
    }
    Ok(true)
}

fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

fn skill_status(target: SkillTarget, src: &Path) -> SkillStatus {
    let Some(dst) = skill_path(target) else {
        return SkillStatus {
            id: target,
            path: String::new(),
            state: State::NotFound,
        };
    };
    let state = if !dst.exists() {
        State::Absent
    } else if !is_ours(&dst) {
        State::Invalid
    } else {
        match same_files(src, &dst) {
            Ok(true) => State::Added,
            Ok(false) => State::Outdated,
            Err(err) => {
                log::warn!("无法比较 Skill {}：{err}", dst.display());
                State::Outdated
            }
        }
    };
    SkillStatus {
        id: target,
        path: dst.to_string_lossy().into_owned(),
        state,
    }
}

fn edit_skill(target: SkillTarget, src: &Path, install: bool) -> Result<(), ApiError> {
    let dst = skill_path(target)
        .ok_or_else(|| error(ApiError::INTERNAL, "找不到用户文件夹，请手动复制 Skill。"))?;
    if dst.exists() && !is_ours(&dst) {
        return Err(error(
            ApiError::BAD_REQUEST,
            "这个位置已有另一个名为 rummage 的 Skill，请先手动处理。",
        ));
    }
    let result = if dst.exists() {
        fs::remove_dir_all(&dst)
    } else {
        Ok(())
    };
    let result = if install {
        result.and_then(|_| copy_dir(src, &dst))
    } else {
        result
    };
    result.map_err(|err| {
        log::warn!(
            "无法{} Skill {}：{err}",
            if install { "安装" } else { "移除" },
            dst.display()
        );
        let msg = if install {
            "无法安装 Skill，请稍后重试，或打开 Skill 文件夹手动复制。"
        } else {
            "无法移除 Skill，请稍后重试。"
        };
        error(ApiError::INTERNAL, msg)
    })
}

// ---- 命令 ----

fn core_exe() -> Result<String, ApiError> {
    let exe = std::env::current_exe().map_err(|err| {
        log::warn!("无法取得程序路径：{err}");
        error(
            ApiError::INTERNAL,
            "无法取得 Rummage 的安装位置，请稍后再试。",
        )
    })?;
    Ok(exe
        .with_file_name(format!("rummage-core{}", std::env::consts::EXE_SUFFIX))
        .to_string_lossy()
        .into_owned())
}

fn info(exe: String, src: PathBuf) -> AgentInfo {
    AgentInfo {
        clients: CLIENTS.iter().map(|&c| mcp_status(c, &exe)).collect(),
        skills: [SkillTarget::Claude, SkillTarget::Agents]
            .iter()
            .map(|&t| skill_status(t, &src))
            .collect(),
        core_exe: exe,
        skill_dir: src.to_string_lossy().into_owned(),
    }
}

/// 在阻塞线程中执行修改（可为空），再读出最新状态
async fn run<R: Runtime>(
    app: AppHandle<R>,
    edit: impl FnOnce(&str, &Path) -> Result<(), ApiError> + Send + 'static,
) -> Result<AgentInfo, ApiError> {
    let exe = core_exe()?;
    let src = bundled_skill(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        edit(&exe, &src)?;
        Ok(info(exe, src))
    })
    .await
    .map_err(|err| {
        error(
            ApiError::INTERNAL,
            format!("读取 AI 工具的配置时出错：{err}"),
        )
    })?
}

#[tauri::command]
pub async fn agent_info<R: Runtime>(app: AppHandle<R>) -> Result<AgentInfo, ApiError> {
    run(app, |_, _| Ok(())).await
}

#[tauri::command]
pub async fn mcp_add<R: Runtime>(app: AppHandle<R>, client: Client) -> Result<AgentInfo, ApiError> {
    run(app, move |exe, _| edit_mcp(client, exe, true)).await
}

#[tauri::command]
pub async fn mcp_remove<R: Runtime>(
    app: AppHandle<R>,
    client: Client,
) -> Result<AgentInfo, ApiError> {
    run(app, move |exe, _| edit_mcp(client, exe, false)).await
}

#[tauri::command]
pub async fn skill_install<R: Runtime>(
    app: AppHandle<R>,
    target: SkillTarget,
) -> Result<AgentInfo, ApiError> {
    run(app, move |_, src| edit_skill(target, src, true)).await
}

#[tauri::command]
pub async fn skill_remove<R: Runtime>(
    app: AppHandle<R>,
    target: SkillTarget,
) -> Result<AgentInfo, ApiError> {
    run(app, move |_, src| edit_skill(target, src, false)).await
}

#[tauri::command]
pub async fn open_skill_dir<R: Runtime>(app: AppHandle<R>) -> Result<(), ApiError> {
    let dir = bundled_skill(&app)?;
    tauri::async_runtime::spawn_blocking(move || tauri_plugin_opener::open_path(&dir, None::<&str>))
        .await
        .map_err(|err| {
            error(
                ApiError::INTERNAL,
                format!("打开 Skill 文件夹时出错：{err}"),
            )
        })?
        .map_err(|err| {
            log::warn!("无法打开 Skill 文件夹：{err}");
            error(
                ApiError::INTERNAL,
                "无法打开 Skill 所在的文件夹，请重新安装 Rummage。",
            )
        })
}

#[cfg(test)]
mod tests;
