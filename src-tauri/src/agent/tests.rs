use super::*;

const EXE: &str = r"C:\Users\me\AppData\Local\Rummage\rummage-core.exe";

#[test]
fn json_add_keeps_comments_and_other_servers() {
    let text = "{\n  // 我的服务\n  \"mcpServers\": {\n    \"other\": { \"command\": \"x\" }\n  },\n  \"theme\": \"dark\"\n}\n";
    let next = json_add(Client::Cursor, text, EXE).unwrap();
    assert!(next.contains("// 我的服务"));
    assert!(next.contains("\"other\": { \"command\": \"x\" }"));
    assert!(next.contains("\"theme\": \"dark\""));
    let v: Value = serde_json::from_str(&next.replace("// 我的服务", "")).unwrap();
    assert_eq!(
        v["mcpServers"]["rummage"],
        serde_json::json!({ "command": EXE, "args": ["mcp"] })
    );
    assert_eq!(json_state(Client::Cursor, &next, EXE), State::Added);
    // 路径大小写不同视为同一个
    assert_eq!(
        json_state(Client::Cursor, &next, &EXE.to_uppercase()),
        State::Added
    );
}

#[test]
fn json_add_to_empty_or_missing_file() {
    assert_eq!(json_state(Client::ClaudeDesktop, "", EXE), State::Absent);
    let next = json_add(Client::ClaudeDesktop, "", EXE).unwrap();
    let v: Value = serde_json::from_str(&next).unwrap();
    assert_eq!(
        v["mcpServers"]["rummage"]["args"],
        serde_json::json!(["mcp"])
    );
}

#[test]
fn json_vscode_and_claude_code_use_stdio_type() {
    let next = json_add(Client::Vscode, "{}", EXE).unwrap();
    let v: Value = serde_json::from_str(&next).unwrap();
    assert_eq!(
        v["servers"]["rummage"],
        serde_json::json!({ "type": "stdio", "command": EXE, "args": ["mcp"] })
    );
    let next = json_add(Client::ClaudeCode, "{\"numStartups\": 3}", EXE).unwrap();
    let v: Value = serde_json::from_str(&next).unwrap();
    assert_eq!(v["mcpServers"]["rummage"]["type"], "stdio");
    assert_eq!(v["numStartups"], 3);
}

#[test]
fn json_outdated_is_replaced_and_remove_keeps_rest() {
    let text = r#"{ "mcpServers": { "rummage": { "command": "D:\\old\\rummage-core.exe", "args": ["mcp"] }, "other": {} } }"#;
    assert_eq!(json_state(Client::Cursor, text, EXE), State::Outdated);
    let next = json_add(Client::Cursor, text, EXE).unwrap();
    assert_eq!(json_state(Client::Cursor, &next, EXE), State::Added);
    let removed = json_remove(Client::Cursor, &next).unwrap();
    assert_eq!(json_state(Client::Cursor, &removed, EXE), State::Absent);
    let v: Value = serde_json::from_str(&removed).unwrap();
    assert!(v["mcpServers"]["other"].is_object());
}

#[test]
fn json_invalid_is_not_touched() {
    assert_eq!(
        json_state(Client::Cursor, "{ \"mcpServers\": ", EXE),
        State::Invalid
    );
    assert_eq!(json_state(Client::Cursor, "[]", EXE), State::Invalid);
    assert_eq!(
        json_state(Client::Cursor, r#"{ "mcpServers": [] }"#, EXE),
        State::Invalid
    );
    assert!(json_add(Client::Cursor, "[]", EXE).is_none());
}

#[test]
fn opencode_v1_and_v2_layouts() {
    // 1 的写法：直接放在 mcp 下，带 enabled
    let next = json_add(
        Client::Opencode,
        "{\n  \"$schema\": \"https://opencode.ai/config.json\",\n}",
        EXE,
    )
    .unwrap();
    let v = parse_json(&next).unwrap().to_serde_value().unwrap();
    assert_eq!(
        v["mcp"]["rummage"],
        serde_json::json!({ "type": "local", "command": [EXE, "mcp"], "enabled": true })
    );
    assert_eq!(json_state(Client::Opencode, &next, EXE), State::Added);

    // 已有 2 的 mcp.servers 时写到其中，不带 enabled
    let text = r#"{ "mcp": { "servers": { "other": { "type": "local", "command": ["x"] } } } }"#;
    let next = json_add(Client::Opencode, text, EXE).unwrap();
    let v: Value = serde_json::from_str(&next).unwrap();
    assert_eq!(
        v["mcp"]["servers"]["rummage"],
        serde_json::json!({ "type": "local", "command": [EXE, "mcp"] })
    );
    assert!(v["mcp"].get("rummage").is_none());
    assert_eq!(json_state(Client::Opencode, &next, EXE), State::Added);
    let removed = json_remove(Client::Opencode, &next).unwrap();
    assert_eq!(json_state(Client::Opencode, &removed, EXE), State::Absent);
}

#[test]
fn toml_add_keeps_rest_and_removes() {
    let text = "model = \"gpt\"\n\n# 注释\n[projects.'D:\\x']\ntrust_level = \"trusted\"\n";
    assert_eq!(toml_state(text, EXE), State::Absent);
    let next = toml_add(text, EXE).unwrap();
    assert!(next.starts_with(text));
    assert!(next.contains("[mcp_servers.rummage]"));
    assert!(!next.contains("[mcp_servers]\n"));
    assert_eq!(toml_state(&next, EXE), State::Added);
    assert_eq!(toml_state(&next, r"D:\other.exe"), State::Outdated);
    let removed = toml_remove(&next).unwrap();
    assert_eq!(toml_state(&removed, EXE), State::Absent);
    assert!(removed.contains("# 注释"));
    assert_eq!(toml_state("model = ", EXE), State::Invalid);
}

#[test]
fn skill_copy_compare_and_ownership() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir_all(src.join("references")).unwrap();
    fs::write(src.join("SKILL.md"), "---\nname: rummage\n---\n").unwrap();
    fs::write(src.join("references").join("cli.md"), "a").unwrap();

    let dst = tmp.path().join("dst");
    copy_dir(&src, &dst).unwrap();
    assert!(is_ours(&dst));
    assert!(same_files(&src, &dst).unwrap());
    fs::write(src.join("references").join("cli.md"), "b").unwrap();
    assert!(!same_files(&src, &dst).unwrap());

    let other = tmp.path().join("other");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("SKILL.md"), "---\nname: something\n---\n").unwrap();
    assert!(!is_ours(&other));
}
