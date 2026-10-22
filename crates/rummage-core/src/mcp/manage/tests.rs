//! MCP 中查看与操作应用的工具的测试：参数解析与 schema、结果文本
use rummage_proto::{FailedFile, RootInfo};
use serde_json::{Value, json};

use super::*;

fn parsed<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, String> {
    serde_json::from_value(v).map_err(|err| err.to_string())
}

#[test]
fn folder_params() {
    let p: AddFolderParams = parsed(json!({
        "path": r" D:\照片 ",
        "kinds": ["Image", "document", "image"],
        "excludes": ["*.tmp", "/草稿"],
    }))
    .unwrap();
    assert_eq!(p.path, r"D:\照片");
    assert_eq!(p.kinds, Some(vec!["image".into(), "document".into()]));
    assert_eq!(p.excludes, Some(vec!["*.tmp".into(), "/草稿".into()]));
    // 宽松：单个字符串；null、空字符串、空数组视为未给出，但排除规则的空数组表示清空
    let p: UpdateFolderParams = parsed(json!({
        "path": r"D:\照片",
        "kinds": "video",
        "excludes": "*.tmp",
    }))
    .unwrap();
    assert_eq!(p.kinds, Some(vec!["video".into()]));
    assert_eq!(p.excludes, Some(vec!["*.tmp".into()]));
    let p: UpdateFolderParams =
        parsed(json!({"path": r"D:\照片", "kinds": [], "excludes": []})).unwrap();
    assert_eq!((p.kinds, p.excludes), (None, Some(Vec::new())));
    let p: UpdateFolderParams =
        parsed(json!({"path": r"D:\照片", "kinds": null, "excludes": null})).unwrap();
    assert_eq!((p.kinds, p.excludes), (None, None));
    let p: UpdateFolderParams =
        parsed(json!({"path": r"D:\照片", "kinds": "", "excludes": " "})).unwrap();
    assert_eq!((p.kinds, p.excludes), (None, None));
    // 类型不对时附正确写法
    for (v, needle) in [
        (json!({"path": 3}), r#"{"path": "#),
        (
            json!({"path": "D:\\a", "kinds": ["other"]}),
            r#"{"kinds": ["#,
        ),
        (
            json!({"path": "D:\\a", "excludes": [1]}),
            r#"{"excludes": ["#,
        ),
    ] {
        let err = parsed::<AddFolderParams>(v).unwrap_err();
        assert!(err.contains(needle), "{err}");
    }
    assert!(parsed::<RemoveFolderParams>(json!({})).is_err());
    assert!(absolute(r"照片").is_err() && absolute("").is_err());
    assert!(absolute(r"D:\照片").is_ok());
}

#[test]
fn other_params() {
    let p: PauseParams = parsed(json!({})).unwrap();
    assert_eq!(p.minutes, 0);
    let p: PauseParams = parsed(json!({"minutes": "90"})).unwrap();
    assert_eq!(p.minutes, 90);
    let err = parsed::<PauseParams>(json!({"minutes": 10081})).unwrap_err();
    assert!(err.contains(r#"{"minutes": 60}"#), "{err}");
    let p: FailedParams = parsed(json!({"limit": null})).unwrap();
    assert_eq!(p.limit, DEFAULT_FAILED);
    assert!(parsed::<FailedParams>(json!({"limit": 101})).is_err());
    let p: GetSettingsParams = parsed(json!({})).unwrap();
    assert_eq!(p.key, "");
    let p: SetSettingParams = parsed(json!({"key": " chunk_tokens ", "value": "256"})).unwrap();
    assert_eq!((p.key.as_str(), p.value), ("chunk_tokens", json!("256")));
    assert!(parsed::<SetSettingParams>(json!({"key": "chunk_tokens"})).is_err());
}

#[test]
fn values() {
    let kind = |k| params::def(k).unwrap().kind;
    assert_eq!(
        json_value(kind("chunk_tokens"), &json!(256)),
        Some(ParamValue::Int(256))
    );
    assert_eq!(
        json_value(kind("min_relevance"), &json!(1.5)),
        Some(ParamValue::Float(1.5))
    );
    assert_eq!(
        json_value(kind("save_history"), &json!("false")),
        Some(ParamValue::Bool(false))
    );
    assert_eq!(
        json_value(kind("video_preset"), &json!("找说话内容")),
        Some(ParamValue::Int(3))
    );
    assert_eq!(json_value(kind("chunk_tokens"), &json!(null)), None);
    assert_eq!(json_value(kind("chunk_tokens"), &json!([1])), None);
}

#[test]
fn schemas_have_no_null() {
    for schema in [
        rmcp::schemars::schema_for!(AddFolderParams),
        rmcp::schemars::schema_for!(UpdateFolderParams),
        rmcp::schemars::schema_for!(RemoveFolderParams),
        rmcp::schemars::schema_for!(GetSettingsParams),
        rmcp::schemars::schema_for!(SetSettingParams),
        rmcp::schemars::schema_for!(PauseParams),
        rmcp::schemars::schema_for!(FailedParams),
    ] {
        let text = serde_json::to_value(&schema).unwrap().to_string();
        assert!(!text.contains("null"), "{text}");
    }
    let schema = serde_json::to_value(rmcp::schemars::schema_for!(AddFolderParams)).unwrap();
    assert_eq!(schema["required"], json!(["path"]));
    assert_eq!(
        schema["properties"]["kinds"]["items"]["enum"],
        json!(["document", "image", "audio", "video"])
    );
    let schema = serde_json::to_value(rmcp::schemars::schema_for!(SetSettingParams)).unwrap();
    assert_eq!(schema["required"], json!(["key", "value"]));
    // 说明文字是字面量，核对其中的数字与常量一致
    let schema = serde_json::to_value(rmcp::schemars::schema_for!(PauseParams)).unwrap();
    let doc = schema["properties"]["minutes"]["description"]
        .as_str()
        .unwrap();
    assert!(doc.contains(&format!("1-{MAX_PAUSE_MIN}")), "{doc}");
    let schema = serde_json::to_value(rmcp::schemars::schema_for!(FailedParams)).unwrap();
    let doc = schema["properties"]["limit"]["description"]
        .as_str()
        .unwrap();
    assert!(
        doc.contains(&format!("1-{MAX_FAILED}"))
            && doc.contains(&format!("Default {DEFAULT_FAILED}")),
        "{doc}"
    );
}

#[test]
fn folders_output() {
    let root = RootInfo {
        id: 1,
        path: r"D:\照片".into(),
        offline: false,
        missing: false,
        files: 12,
        excludes: vec!["*.tmp".into()],
        skip_nomedia: true,
        scanning: false,
    };
    let disk = RootInfo {
        id: 2,
        path: r"C:\".into(),
        excludes: Vec::new(),
        skip_nomedia: true,
        ..root.clone()
    };
    assert_eq!(
        folders_text(&["document"], &[root, disk], None),
        format!(
            "Content recognition: document (shared by all folders)
Indexed folders:
  D:\\照片 (12 files)
    excludes: *.tmp
  C:\\ (12 files)
    note: {WHOLE_DRIVE_NOTE}"
        )
    );
    assert_eq!(
        folders_text(&[], &[], None),
        "Content recognition: none (all files are searchable by file name only) (shared by all folders)
No folders are indexed yet. Add one with add_folder."
    );
}

#[test]
fn settings_output() {
    let values = params::defaults();
    let all = settings_text(&values, None);
    assert!(
        all.starts_with("Search (applies immediately):\n  "),
        "{all}"
    );
    // 每个在当前平台显示的参数都列出，其余不列出
    for d in params::PARAMS {
        let listed = all.contains(&format!("\n  {} = ", d.key));
        assert_eq!(listed, d.shown(), "{}", d.key);
    }
    let one = settings_text(&values, params::def("image_tokens"));
    assert!(
        one.starts_with(
            "image_tokens = 标准 (280 Token) | default 标准 (280 Token) | one of 最低 (70 Token), 较低 (140 Token), 标准 (280 Token), 较高 (560 Token), 最高 (1120 Token) | changing it rebuilds the image index
    图片识别精度: "
        ),
        "{one}"
    );
    let one = settings_text(&values, params::def("long_image_tokens"));
    assert!(
        one.starts_with("long_image_tokens = 标准 (140 Token) | default 标准 (140 Token) |"),
        "{one}"
    );
    let d = params::def("chunk_tokens").unwrap();
    assert_eq!(
        set_text(d, ParamValue::Int(256), true, &["document"]),
        "Set chunk_tokens to 256. Rummage will rebuild the document index with the new setting in the background; the old index stays searchable meanwhile."
    );
    assert_eq!(
        set_text(d, ParamValue::Int(384), false, &[]),
        "chunk_tokens is already 384; nothing changed."
    );
}

#[test]
fn failed_output() {
    let file = FailedFile {
        file_id: 1,
        name: "b.docx".into(),
        path: r"D:\b.docx".into(),
        kind: "document".into(),
        reason: Some("文档已加密。".into()),
        failed_at: 0,
        retryable: false,
    };
    assert_eq!(failed_text(&[], 0, 0), "No files failed to index.");
    assert_eq!(
        failed_text(&[file], 3, 2),
        "3 files failed to index (2 can be retried):
  D:\\b.docx (encrypted document, cannot be retried)
    reason: 文档已加密。
Showing the 1 most recent."
    );
}
