//! 查看与操作应用的子命令的测试：参数解析与输出格式
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use rummage_proto::{FailedFile, IndexCounts, IndexState, IndexStatus, PauseReason, RootInfo};

use super::*;

fn args(cmd: &str, list: &[&str]) -> Result<Args, String> {
    super::super::parse(cmd, list.iter().map(OsString::from))
}

fn cmd(c: &str, list: &[&str]) -> Cmd {
    args(c, list).unwrap().cmd
}

#[test]
fn parse_folders() {
    assert_eq!(cmd("folders", &[]), Cmd::Folders);
    let a = args(
        "folders",
        &[
            "add",
            r"照片\2026",
            "--types",
            "Image，document",
            "--types",
            "image",
            "--exclude",
            "*.tmp",
            "--exclude",
            "/草稿",
            "--json",
            "--data-dir",
            r"D:\数据",
        ],
    )
    .unwrap();
    assert!(a.json);
    assert_eq!(a.data_dir, Some(PathBuf::from(r"D:\数据")));
    assert_eq!(
        a.cmd,
        Cmd::AddFolder {
            path: PathBuf::from(r"照片\2026"),
            kinds: Some(vec!["image".into(), "document".into()]),
            excludes: Some(vec!["*.tmp".into(), "/草稿".into()]),
        }
    );
    assert_eq!(
        cmd("folders", &["add", r"D:\a"]),
        Cmd::AddFolder {
            path: PathBuf::from(r"D:\a"),
            kinds: None,
            excludes: None,
        }
    );
    // 清空排除规则；只给一条空规则同样是清空（由核心去掉空行）
    assert_eq!(
        cmd("folders", &["set", r"D:\a", "--no-exclude"]),
        Cmd::SetFolder {
            path: PathBuf::from(r"D:\a"),
            kinds: None,
            excludes: Some(Vec::new()),
        }
    );
    assert_eq!(
        cmd("folders", &["set", r"D:\a", "--exclude", ""]),
        Cmd::SetFolder {
            path: PathBuf::from(r"D:\a"),
            kinds: None,
            excludes: Some(vec![String::new()]),
        }
    );
    assert_eq!(
        cmd("folders", &["remove", r"D:\a"]),
        Cmd::RemoveFolder(PathBuf::from(r"D:\a"))
    );
}

#[test]
fn parse_others() {
    assert_eq!(cmd("settings", &[]), Cmd::Settings(None));
    assert_eq!(
        cmd("settings", &["chunk_tokens"]),
        Cmd::Settings(Some("chunk_tokens".into()))
    );
    // 负数不是参数名
    assert_eq!(
        cmd("settings", &["set", "audio_silence_db", "-60"]),
        Cmd::SetSetting {
            key: "audio_silence_db".into(),
            value: "-60".into(),
        }
    );
    assert_eq!(cmd("pause", &[]), Cmd::Pause(None));
    assert_eq!(cmd("pause", &["--minutes", "30"]), Cmd::Pause(Some(30)));
    assert_eq!(cmd("resume", &[]), Cmd::Resume);
    assert_eq!(cmd("failed", &[]), Cmd::Failed);
    assert_eq!(cmd("failed", &["retry", "--json"]), Cmd::RetryFailed);
}

#[test]
fn parse_errors() {
    for (c, list, needle) in [
        ("folders", &["add"][..], "需要一个文件夹路径"),
        ("folders", &["set", r"D:\a"], "需要 --types 或 --exclude"),
        (
            "folders",
            &["remove", r"D:\a", "--types", "image"],
            "只能用于",
        ),
        ("folders", &["--exclude", "*.tmp"], "只能用于"),
        ("folders", &["--no-exclude"], "只能用于"),
        (
            "folders",
            &["set", r"D:\a", "--exclude", "*.tmp", "--no-exclude"],
            "不能同时使用",
        ),
        (
            "folders",
            &["add", r"D:\a", "--types", "other"],
            "无法识别的类型“other”",
        ),
        (
            "folders",
            &["add", r"D:\a", "--types", " , "],
            "至少给出一种",
        ),
        (
            "folders",
            &["add", r"D:\a", r"D:\b"],
            r"无法识别的参数：D:\b",
        ),
        ("folders", &["list"], "无法识别的参数：list"),
        ("folders", &["--minutes", "5"], "无法识别的参数：--minutes"),
        ("settings", &["set", "chunk_tokens"], "需要参数的键和值"),
        ("pause", &["--minutes", "0"], "--minutes 应为 1–10080"),
        ("pause", &["--minutes", "10081"], "--minutes 应为"),
        ("resume", &["now"], "无法识别的参数：now"),
        ("failed", &["retry", "all"], "无法识别的参数：all"),
    ] {
        let err = args(c, list).unwrap_err();
        assert!(err.contains(needle), "{c} {list:?}: {err}");
    }
}

fn root(path: &str, files: i64, excludes: &[&str]) -> RootInfo {
    RootInfo {
        id: 1,
        path: path.into(),
        offline: false,
        missing: false,
        files,
        excludes: excludes.iter().map(|s| s.to_string()).collect(),
        skip_nomedia: true,
        scanning: false,
    }
}

#[test]
fn folders_output() {
    let index = IndexStatus {
        state: IndexState::Running,
        paused: Some(PauseReason::Battery),
        total: 200,
        done: 150,
        failed: 10,
        pending: 40,
        by_kind: BTreeMap::from([(
            "document".into(),
            IndexCounts {
                total: 200,
                done: 150,
                failed: 10,
                pending: 40,
            },
        )]),
        backend: None,
        error: None,
        eta_s: None,
        rebuild: None,
    };
    let mut offline = root(r"E:\移动硬盘", 0, &[]);
    offline.offline = true;
    let mut missing = root(r"D:\旧资料", 0, &[]);
    missing.offline = true;
    missing.missing = true;
    let roots = [
        root(r"D:\照片", 1234, &["*.tmp", "/草稿"]),
        offline,
        missing,
        root(r"C:\", 9000, &[]),
    ];
    assert_eq!(
        folders_text(&["document", "image"], &roots, Some(&index)),
        format!(
            "识别内容的文件类型：文档、图片（所有文件夹共用）
索引进度：160 / 200（80%），失败 10，未接电源，已暂停
已索引的文件夹：
  D:\\照片（1234 个文件）
    排除：*.tmp、/草稿
  E:\\移动硬盘（设备未连接）
  D:\\旧资料（找不到文件夹）
  C:\\（9000 个文件）
    注意：{WHOLE_DRIVE_NOTE}"
        )
    );
    assert_eq!(
        folders_text(&[], &[], None),
        "识别内容的文件类型：无（只按文件名搜索）（所有文件夹共用）
还没有已索引的文件夹，可用 folders add <文件夹> 添加。"
    );
}

#[test]
fn settings_output() {
    let mut values = params::defaults();
    values.insert("chunk_tokens".into(), ParamValue::Int(256));
    let text = settings_text(&values);
    assert!(text.starts_with("搜索（修改后立即生效）\n  "), "{text}");
    assert!(
        text.contains(
            "\n  chunk_tokens = 256（默认 384，128–512 的整数，修改后重建文档的索引）\n    文本分段长度（Token）："
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "\n  image_tokens = 标准（280 Token）（默认 标准（280 Token），可选 最低（70 Token）、较低（140 Token）"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "\n  video_frame_tokens = 标准（140 Token）（默认 标准（140 Token），可选 低（70 Token）、标准（140 Token），修改后重建视频的索引）"
        ),
        "{text}"
    );
    // 每个在当前平台显示的参数都列出，其余不列出
    for d in params::PARAMS {
        let listed = text.contains(&format!("\n  {} = ", d.key));
        assert_eq!(listed, d.shown(), "{}", d.key);
    }
    assert_eq!(
        range_text(params::def("save_history").unwrap().kind),
        "true 或 false"
    );
    assert_eq!(
        range_text(params::def("min_relevance").unwrap().kind),
        "0–6 的数"
    );
}

#[test]
fn set_output() {
    let d = params::def("chunk_tokens").unwrap();
    assert_eq!(
        set_text(d, ParamValue::Int(256), true, &["document"]),
        "已将 chunk_tokens（文本分段长度（Token））设为 256。将按新设置重建文档的索引，重建期间旧索引仍可搜索。"
    );
    assert_eq!(
        set_text(d, ParamValue::Int(384), false, &[]),
        "chunk_tokens（文本分段长度（Token））已是 384，没有变化。"
    );
    let d = params::def("video_window_s").unwrap();
    assert!(set_text(d, ParamValue::Int(8), true, &[]).ends_with("此项暂不生效。"));
    let d = params::def("result_limit").unwrap();
    assert_eq!(
        set_text(d, ParamValue::Int(100), true, &[]),
        "已将 result_limit（结果数量上限）设为 100。"
    );
}

#[test]
fn failed_output() {
    let file = |path: &str, retryable| FailedFile {
        file_id: 1,
        name: String::new(),
        path: path.into(),
        kind: "document".into(),
        reason: Some("文件已损坏，无法读取。".into()),
        failed_at: 0,
        retryable,
    };
    assert_eq!(failed_text(&[], 0, 0), "没有建立索引失败的文件。");
    assert_eq!(
        failed_text(&[file(r"D:\a.pdf", true), file(r"D:\b.docx", false)], 5, 4),
        "共 5 个文件建立索引失败，其中 4 个可以重试：
  D:\\a.pdf
    文件已损坏，无法读取。
  D:\\b.docx（加密文档，不能重试）
    文件已损坏，无法读取。
只列出了最近的 2 个。"
    );
}

#[test]
fn plain_converts_settings_location() {
    use crate::cli::plain;
    assert_eq!(
        plain("请重新启动 Rummage，或在「设置 → 设备」中选择使用 CPU。"),
        "请重新启动 Rummage，或在 Rummage 的 设置 → 设备 中选择使用 CPU。"
    );
    // 开头、标点前不加空格；设备名的「」不动
    assert_eq!(
        plain("「设置 → 模型」。「RTX 3070」已屏蔽。"),
        "Rummage 的 设置 → 模型。「RTX 3070」已屏蔽。"
    );
}

#[test]
fn usage_lists_commands() {
    let usage = crate::cli::usage();
    for c in crate::cli::COMMANDS {
        assert!(usage.contains(&format!("rummage-core {c}")), "{c}");
    }
    assert!(usage.contains("1–10080"), "{usage}");
}
