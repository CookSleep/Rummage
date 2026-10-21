//! 命令行的测试：参数解析、日期区间、退出码、JSON 转义与各子命令的输出格式
use std::collections::BTreeMap;

use chrono::{FixedOffset, Utc};
use rummage_proto::{
    FileInfo, IndexCounts, IndexState, IndexStatus, PROTOCOL_VERSION, PauseReason, RebuildStatus,
    RootInfo, SearchHit, SegmentHit, SegmentPreview, SemanticUnavailable, Snippet,
};

use serde_json::json;

use super::output::*;
use super::*;

fn args(cmd: &str, list: &[&str]) -> Result<Args, String> {
    parse(cmd, list.iter().map(OsString::from))
}

fn search(list: &[&str]) -> SearchArgs {
    match args("search", list).unwrap().cmd {
        Cmd::Search(s) => s,
        cmd => panic!("{cmd:?}"),
    }
}

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

#[test]
fn search_args() {
    let s = search(&["年终", "总结"]);
    assert_eq!(s.text.as_deref(), Some("年终 总结"));
    assert_eq!(s.limit, 20);
    assert!(!s.include_weak && s.kinds.is_empty());
    let a = args(
        "search",
        &[
            "柯基",
            "--image",
            r"图\a.png",
            "--type",
            "image，Video",
            "--type",
            "image",
            "--folder",
            r"D:\照片",
            "--since",
            "2026-01-05",
            "--until",
            "2026-02-01",
            "--limit",
            "5",
            "--include-weak",
            "--output",
            r"结果\猫.CSV",
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
        Cmd::Search(SearchArgs {
            text: Some("柯基".into()),
            image: Some(PathBuf::from(r"图\a.png")),
            kinds: vec!["image".into(), "video".into()],
            folder: Some(PathBuf::from(r"D:\照片")),
            since: Some(date(2026, 1, 5)),
            until: Some(date(2026, 2, 1)),
            limit: 5,
            include_weak: true,
            output: Some(PathBuf::from(r"结果\猫.CSV")),
        })
    );
    // 只有图片也可以
    assert_eq!(search(&["--image", "a.png"]).text, None);
}

#[test]
fn search_args_errors() {
    for (list, needle) in [
        (&[][..], "查询文字或 --image"),
        (&["  "][..], "查询文字或 --image"),
        (&["x", "--type", "pdf"][..], "无法识别的类型“pdf”"),
        (&["x", "--limit", "0"][..], "1–500"),
        (&["x", "--limit", "501"][..], "1–500"),
        (&["x", "--limit", "abc"][..], "1–500"),
        (&["x", "--since", "2026/01/05"][..], "YYYY-MM-DD"),
        (&["x", "--since", "2026-02-30"][..], "YYYY-MM-DD"),
        (
            &["x", "--since", "2026-02-02", "--until", "2026-02-01"][..],
            "不能晚于",
        ),
        (&["x", "--image"][..], "缺少值"),
        (&["x", "--verbose"][..], "无法识别的参数"),
    ] {
        let err = args("search", list).unwrap_err();
        assert!(err.contains(needle), "{list:?}: {err}");
    }
}

#[test]
fn status_and_context_args() {
    let a = args("status", &["--json"]).unwrap();
    assert_eq!((a.cmd, a.json), (Cmd::Status, true));
    assert!(args("status", &["extra"]).is_err());
    // search 专有的参数在别的命令中不认
    assert!(args("status", &["--include-weak"]).is_err());
    assert!(args("status", &["--limit", "3"]).is_err());
    assert_eq!(
        args("context", &["18342"]).unwrap().cmd,
        Cmd::Context(18342)
    );
    for list in [
        &[][..],
        &["1", "2"][..],
        &["abc"][..],
        &["0"][..],
        &["-3"][..],
    ] {
        assert!(args("context", list).is_err(), "{list:?}");
    }
}

#[test]
fn day_range_includes_whole_days() {
    let tz = FixedOffset::east_opt(8 * 3600).unwrap();
    let (since, until) = day_range(&tz, Some(date(2026, 1, 5)), Some(date(2026, 1, 5)));
    // 北京时间 1 月 5 日 0 点到 23:59:59.999，即 UTC 1 月 4 日 16:00 起
    let start = Utc
        .with_ymd_and_hms(2026, 1, 4, 16, 0, 0)
        .unwrap()
        .timestamp_millis();
    assert_eq!(since, Some(start));
    assert_eq!(until, Some(start + 24 * 3600 * 1000 - 1));
    assert_eq!(day_range(&Utc, None, None), (None, None));
    let (_, until) = day_range(&Utc, None, Some(date(2026, 12, 31)));
    assert_eq!(
        until,
        Some(
            Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0)
                .unwrap()
                .timestamp_millis()
                - 1
        )
    );
}

#[test]
fn exit_codes() {
    assert_eq!(ExitCode::from(Outcome::Found), ExitCode::from(0));
    assert_eq!(ExitCode::from(Outcome::Empty), ExitCode::from(1));
    assert_eq!(ExitCode::from(Outcome::Failed), ExitCode::from(2));
}

#[test]
fn folder_comparison() {
    assert_eq!(folded(r"D:\照片\"), folded("d:/照片"));
    assert_eq!(folded(r"D:\"), "d:");
    assert_ne!(folded(r"D:\照片\2025"), folded(r"D:\照片"));
}

#[test]
fn json_is_ascii() {
    let v = json!({"name": "柯基 a\"b", "emoji": "🐕", "n": 1});
    let out = ascii_json(&v);
    assert!(out.is_ascii(), "{out}");
    assert!(out.contains(r#"\u67ef\u57fa a\"b"#), "{out}");
    // 超出基本平面的字符写成代理对
    assert!(out.contains(r"\ud83d\udc15"), "{out}");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&out).unwrap(), v);
}

/// UTC 当天中午，各时区的本地日期都是同一天
fn noon(y: i32, m: u32, d: u32) -> i64 {
    Utc.with_ymd_and_hms(y, m, d, 12, 0, 0)
        .unwrap()
        .timestamp_millis()
}

fn hit(name: &str, kind: &str) -> SearchHit {
    SearchHit {
        file_id: 1,
        path: format!(r"D:\资料\{name}"),
        name: name.into(),
        kind: kind.into(),
        size: 1_258_291,
        mtime: noon(2026, 1, 5),
        cloud: false,
        offline: false,
        missing: false,
        score: 0.03,
        cosine: Some(0.5),
        z: Some(3.9),
        match_pct: Some(92),
        low_relevance: false,
        segments: Vec::new(),
        name_hit: false,
        text_segments: Vec::new(),
        width: None,
        height: None,
        snippet: None,
    }
}

fn sample_hits() -> Vec<SearchHit> {
    let mut doc = hit("2025年终总结.docx", "document");
    doc.name_hit = true;
    doc.segments = vec![SegmentHit {
        segment_id: 18342,
        locator_json:
            r#"{"char_start":0,"char_end":9,"heading":"成果","page_start":3,"page_end":3}"#.into(),
        cosine: 0.5,
    }];
    doc.snippet = Some(Snippet {
        segment_id: 18342,
        text: "今年完成了三个项目……".into(),
    });
    let mut video = hit("年会.mp4", "video");
    video.size = 500;
    video.cloud = true;
    video.match_pct = None;
    video.cosine = None;
    video.segments = vec![SegmentHit {
        segment_id: 7,
        locator_json: r#"{"start_ms":62000,"end_ms":75000}"#.into(),
        cosine: 0.3,
    }];
    vec![doc, video]
}

fn results(hits: &[SearchHit]) -> Results<'_> {
    Results {
        hits,
        took_ms: 12,
        partial: false,
        semantic_unavailable: false,
        unavailable: None,
        weak_omitted: 0,
        image: false,
    }
}

#[test]
fn search_text_output() {
    let hits = sample_hits();
    let mut r = results(&hits);
    r.partial = true;
    r.weak_omitted = 5;
    assert_eq!(
        search_text(&r),
        r"索引尚未完成，结果可能不完整。
找到 2 个结果：

1. 2025年终总结.docx  文档  匹配度 92%
   D:\资料\2025年终总结.docx
   修改于 2026-01-05 · 1.2 MB · 内容相近 · 文件名含搜索词 · 命中 ID 18342
   位置：第 3 页 · 成果
   “今年完成了三个项目……”

2. 年会.mp4  视频
   D:\资料\年会.mp4
   修改于 2026-01-05 · 500 B · 仅在云端
   位置：01:02–01:15

另有至少 5 个匹配度较低的结果，加 --include-weak 显示。"
    );
    let mut empty = results(&[]);
    empty.semantic_unavailable = true;
    empty.unavailable = Some(SemanticUnavailable::NoMmproj);
    empty.image = true;
    assert_eq!(
        search_text(&empty),
        "尚未下载图片与音视频模型，无法以图搜图。可在 Rummage 的 设置 → 模型 中下载或导入。\n没有找到相关的文件。"
    );
    empty.unavailable = Some(SemanticUnavailable::NoModel);
    empty.image = false;
    assert_eq!(
        search_text(&empty),
        "尚未下载文字模型，只显示文件名或正文中含有搜索词的结果。可在 Rummage 的 设置 → 模型 中下载或导入。\n没有找到相关的文件。"
    );
    empty.unavailable = Some(SemanticUnavailable::QueryTooLong);
    assert!(
        search_text(&empty)
            .starts_with("搜索内容过长，只显示文件名或正文中含有搜索词的结果。请缩短后再试。"),
        "{}",
        search_text(&empty)
    );
}

#[test]
fn search_json_output() {
    let hits = sample_hits();
    let out = search_json(&results(&hits[..1]));
    assert!(out.is_ascii());
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["protocol"], PROTOCOL_VERSION);
    assert_eq!(v["tookMs"], 12);
    assert_eq!(v["weakOmitted"], 0);
    assert_eq!(v["unavailable"], serde_json::Value::Null);
    let h = &v["hits"][0];
    assert_eq!(h["name"], "2025年终总结.docx");
    assert_eq!(h["matchPct"], 92);
    assert_eq!(h["snippet"]["segmentId"], 18342);
    assert_eq!(h["lowRelevance"], false);
    assert_eq!(
        out,
        format!(
            r#"{{"protocol":{PROTOCOL_VERSION},"hits":[{}],"tookMs":12,"partial":false,"semanticUnavailable":false,"unavailable":null,"weakOmitted":0}}"#,
            ascii_json(&hits[0])
        )
    );
}

#[test]
fn locators() {
    for (json, text) in [
        (r#"{"page_start":3,"page_end":4}"#, "第 3–4 页"),
        (
            r#"{"slide_start":2,"slide_end":2,"heading":""}"#,
            "第 2 张幻灯片",
        ),
        (r#"{"heading":"安装指南"}"#, "安装指南"),
        (
            r#"{"heading":"京都","line_start":1,"line_end":5}"#,
            "京都 · 第 1–5 行",
        ),
        (
            r#"{"start_ms":3725000,"end_ms":3730000}"#,
            "1:02:05–1:02:10",
        ),
        (
            r#"{"region":{"x":0,"y":0,"width":10,"height":10}}"#,
            "长图中的一段",
        ),
        (
            r#"{"embed":{"kind":"image","n":1,"page":5}}"#,
            "第 5 页中的图片",
        ),
        (
            r#"{"embed":{"kind":"video","n":1,"sheet":"销售"},"start_ms":0,"end_ms":8000}"#,
            "工作表“销售”中的视频 00:00–00:08",
        ),
        (r#"{"embed":{"kind":"audio","n":1}}"#, "文档中的音频"),
        ("{}", ""),
        ("not json", ""),
    ] {
        assert_eq!(locator_text(json), text, "{json}");
    }
}

#[test]
fn sizes() {
    assert_eq!(format_size(0), "0 B");
    assert_eq!(format_size(1536), "1.5 KB");
    assert_eq!(format_size(5 * 1024 * 1024 * 1024), "5.0 GB");
}

fn status_sample() -> IndexStatus {
    IndexStatus {
        state: IndexState::Running,
        paused: Some(PauseReason::Battery),
        total: 400,
        done: 97,
        failed: 3,
        pending: 300,
        by_kind: BTreeMap::from([
            (
                "document".to_string(),
                IndexCounts {
                    total: 100,
                    done: 90,
                    failed: 3,
                    pending: 7,
                },
            ),
            (
                "image".to_string(),
                IndexCounts {
                    total: 300,
                    done: 7,
                    failed: 0,
                    pending: 293,
                },
            ),
        ]),
        backend: Some("NVIDIA GeForce RTX 4080 SUPER".into()),
        error: None,
        eta_s: Some(750),
        rebuild: Some(RebuildStatus {
            done: 2,
            total: 5,
            by_kind: BTreeMap::new(),
        }),
    }
}

fn roots() -> Vec<RootInfo> {
    vec![
        RootInfo {
            id: 1,
            path: r"D:\资料".into(),
            offline: false,
            missing: false,
            files: 1234,
            excludes: Vec::new(),
            scanning: false,
        },
        RootInfo {
            id: 2,
            path: r"E:\照片".into(),
            offline: true,
            missing: false,
            files: 50,
            excludes: Vec::new(),
            scanning: false,
        },
    ]
}

#[test]
fn status_output() {
    let index = status_sample();
    let roots = roots();
    let s = Status {
        core_version: "0.1.0",
        onboarded: true,
        index: &index,
        roots: &roots,
    };
    assert_eq!(
        status_text(&s),
        r"Rummage 0.1.0
状态：正在建立索引（未接电源，已暂停）
进度：100 / 400（25%），失败 3，剩余约 13 分钟
  文档 93 / 100
  图片 7 / 300
按新设置重建：2 / 5
内容识别设备：NVIDIA GeForce RTX 4080 SUPER
已索引的文件夹：
  D:\资料（1234 个文件）
  E:\照片（设备未连接）"
    );
    let idle = IndexStatus {
        state: IndexState::Idle,
        paused: None,
        total: 0,
        done: 0,
        failed: 0,
        pending: 0,
        by_kind: BTreeMap::new(),
        backend: None,
        error: None,
        eta_s: None,
        rebuild: None,
    };
    assert_eq!(
        status_text(&Status {
            core_version: "0.1.0",
            onboarded: false,
            index: &idle,
            roots: &[],
        }),
        "Rummage 0.1.0\n尚未完成首次设置，请先打开 Rummage。\n状态：索引已完成\n进度：0 / 0（100%），失败 0\n内容识别设备：未启动\n还没有已索引的文件夹，请在 Rummage 中添加。"
    );
    let v: serde_json::Value = serde_json::from_str(&status_json(&s)).unwrap();
    assert_eq!(v["coreVersion"], "0.1.0");
    assert_eq!(v["index"]["paused"]["type"], "battery");
    assert_eq!(v["index"]["byKind"]["image"]["total"], 300);
    assert_eq!(v["roots"][1]["path"], r"E:\照片");
}

#[test]
fn context_output() {
    let seg = SegmentPreview {
        file_id: 1,
        locator_json: r#"{"page_start":3,"page_end":3}"#.into(),
        text: Some("今年完成了三个项目。\n".into()),
        before: Some("…上一段的结尾。".into()),
        after: None,
    };
    let file = FileInfo {
        file_id: 1,
        path: r"D:\资料\2025年终总结.docx".into(),
        name: "2025年终总结.docx".into(),
        ext: "docx".into(),
        kind: "document".into(),
        cloud: false,
        offline: false,
        missing: false,
        size: 10,
        mtime: 1,
        width: None,
        height: None,
        status: "stored".into(),
        job: None,
        segments: 3,
        indexed_at: Some(5),
        spaces: Vec::new(),
    };
    assert_eq!(
        context_text(Some(&file), &seg),
        r"2025年终总结.docx（第 3 页）
D:\资料\2025年终总结.docx

[前文]
…上一段的结尾。

[命中]
今年完成了三个项目。"
    );
    assert_eq!(
        context_text(None, &seg),
        "第 3 页\n\n[前文]\n…上一段的结尾。\n\n[命中]\n今年完成了三个项目。"
    );
    let v: serde_json::Value = serde_json::from_str(&context_json(Some(&file), &seg)).unwrap();
    assert_eq!(v["file"]["path"], r"D:\资料\2025年终总结.docx");
    assert_eq!(v["segment"]["fileId"], 1);
    assert_eq!(v["segment"]["after"], serde_json::Value::Null);
}

#[test]
fn not_a_root_lists_folders() {
    assert_eq!(
        not_a_root(Path::new(r"D:\其他"), &roots()),
        "\"D:\\其他\" 不是已索引的文件夹。已索引的文件夹：\n  D:\\资料\n  E:\\照片"
    );
    assert!(not_a_root(Path::new("x"), &[]).contains("还没有已索引的文件夹"));
}

#[test]
fn export_formats_and_no_overwrite() {
    let mut a = hit("账单, \"三月\".pdf", "document");
    a.match_pct = None;
    let mut b = hit("猫.jpg", "image");
    b.cloud = true;
    let mut c = hit("=1+2.txt", "other");
    c.path = "=1+2.txt".into();
    let hits = vec![a, b];
    let csv = export::render(&hits, "csv");
    let lines: Vec<_> = csv.split("\r\n").collect();
    assert_eq!(
        lines[0],
        "\u{feff}path,name,kind,size,modified,match,status"
    );
    assert!(
        lines[1].starts_with(
            r#""D:\资料\账单, ""三月"".pdf","账单, ""三月"".pdf",document,1258291,2026-01-05 "#
        ),
        "{}",
        lines[1]
    );
    assert!(lines[1].ends_with(",,local"), "{}", lines[1]);
    assert!(lines[2].ends_with(",92,cloud"), "{}", lines[2]);
    let formula = export::render(&[c], "csv");
    assert!(
        formula.contains("\r\n'=1+2.txt,'=1+2.txt,other,"),
        "{formula}"
    );
    let v: serde_json::Value = serde_json::from_str(&export::render(&hits, "json")).unwrap();
    assert_eq!(v[0]["match"], json!(null));
    assert_eq!(v[1]["kind"], "image");
    assert_eq!(v[1]["size"], 1_258_291);
    assert_eq!(v[1]["status"], "cloud");
    assert_eq!(
        export::render(&hits, "txt"),
        "D:\\资料\\账单, \"三月\".pdf\r\nD:\\资料\\猫.jpg\r\n"
    );
    assert_eq!(export::ext(Path::new("a.JSON")), Some("json"));
    assert_eq!(export::ext(Path::new("a.xlsx")), None);

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("结果.TXT");
    export::save(&path, &hits).unwrap();
    assert!(matches!(
        export::save(&path, &hits),
        Err(export::ExportError::Exists)
    ));
    assert!(matches!(
        export::save(&dir.path().join("x.md"), &hits),
        Err(export::ExportError::Format)
    ));
    assert!(args("search", &["猫", "--output", "a.xlsx"]).is_err());
}
