//! 切片的测试：快照、长度上限与重叠、Markdown 标题、合并短片、偏移与多字节字符、参数读取
use std::convert::Infallible;
use std::fmt::Write;

use super::*;

/// 假的计数：每两个字符一个 token，前缀与特殊 token 固定 12 个。
fn fake(s: &str) -> Result<usize, Infallible> {
    Ok(s.chars().count() / 2 + 12)
}

fn opts() -> ChunkOpts {
    ChunkOpts::from_params(&BTreeMap::new())
}

fn run(text: &str, kind: TextKind, opts: &ChunkOpts) -> Vec<Chunk> {
    let chunks = chunk(text, kind, opts, &mut fake).unwrap();
    check(text, &chunks);
    chunks
}

/// 通用检查：不超过上限、与原文一致、位置单调。
fn check(text: &str, chunks: &[Chunk]) {
    let chars: Vec<char> = text.chars().collect();
    let line_of = |i: usize| chars[..i].iter().filter(|&&c| c == '\n').count() + 1;
    for (i, c) in chunks.iter().enumerate() {
        assert!(fake(&c.text).unwrap() <= MAX_TOKENS, "第 {i} 片超过上限");
        let got: String = chars[c.char_start..c.char_end].iter().collect();
        assert_eq!(got, c.text, "第 {i} 片与原文不一致");
        assert_eq!(c.line_start, line_of(c.char_start));
        assert_eq!(c.line_end, line_of(c.char_end - 1));
        assert_eq!(c.text, c.text.trim());
        if i > 0 {
            assert!(c.char_start > chunks[i - 1].char_start);
            assert!(c.char_end > chunks[i - 1].char_end);
        }
    }
}

fn render(chunks: &[Chunk]) -> String {
    let mut out = String::new();
    for (i, c) in chunks.iter().enumerate() {
        writeln!(
            out,
            "=== #{i} 字符 {}..{} 行 {}..{} token {} 标题“{}”",
            c.char_start,
            c.char_end,
            c.line_start,
            c.line_end,
            fake(&c.text).unwrap(),
            c.heading
        )
        .unwrap();
        writeln!(out, "{}", c.text).unwrap();
    }
    out
}

fn chinese_paragraph() -> String {
    let topics = [
        "扫描文件夹",
        "读取正文",
        "切分片段",
        "计算向量",
        "写入索引",
        "响应查询",
    ];
    (1..=40)
        .map(|i| {
            let t = topics[i % topics.len()];
            format!("第{i}句说明{t}的做法，它决定了本地语义搜索在大量文件下的速度与准确度。")
        })
        .collect()
}

fn english_text() -> String {
    let mut out = String::new();
    for p in 1..=6 {
        for s in 1..=5 {
            write!(
                out,
                "Paragraph {p} sentence {s} explains how the indexer handles files on a local disk! "
            )
            .unwrap();
        }
        out.push_str("Is that clear?\n\n");
    }
    out
}

const MARKDOWN: &str = "# 用户手册

本手册介绍软件的安装与使用。

## 安装

### Windows

下载安装包后双击运行，按提示完成安装。安装程序不会修改系统设置。

```sh
# 这一行在代码块中，不是标题
rummage --version
```

### Android

在应用商店搜索并安装。首次启动时需要授予存储权限。

## 使用

在搜索框中输入描述，例如“去年夏天在海边拍的照片”。结果按相关度排序。

#### 深层标题 ###

最后一段。
";

fn long_sentence() -> String {
    "这是一个没有任何标点的超长句子".repeat(120)
}

#[test]
fn snapshot_chinese_paragraph() {
    insta::assert_snapshot!(render(&run(&chinese_paragraph(), TextKind::Text, &opts())));
}

#[test]
fn snapshot_english_paragraphs() {
    insta::assert_snapshot!(render(&run(&english_text(), TextKind::Text, &opts())));
}

#[test]
fn snapshot_markdown_headings() {
    // 目标长度调小，使各节分别成片。
    let o = ChunkOpts {
        target: 60,
        overlap_pct: 15,
        min: 0,
    };
    insta::assert_snapshot!(render(&run(MARKDOWN, TextKind::Markdown, &o)));
}

#[test]
fn snapshot_long_sentence() {
    insta::assert_snapshot!(render(&run(&long_sentence(), TextKind::Text, &opts())));
}

#[test]
fn snapshot_single_line() {
    insta::assert_snapshot!(render(&run("只有一行的文件。", TextKind::Text, &opts())));
}

#[test]
fn empty_and_blank() {
    for text in ["", "  \n\n\t \n"] {
        for kind in [TextKind::Text, TextKind::Markdown] {
            let mut called = false;
            let mut count = |s: &str| {
                called = true;
                fake(s)
            };
            assert!(chunk(text, kind, &opts(), &mut count).unwrap().is_empty());
            assert!(!called);
        }
    }
}

#[test]
fn within_limit_for_all_targets() {
    let texts = [
        (chinese_paragraph(), TextKind::Text),
        (english_text(), TextKind::Text),
        (MARKDOWN.to_string(), TextKind::Markdown),
        (long_sentence(), TextKind::Text),
    ];
    for target in [128, 384, 512] {
        for pct in [0, 15, 30] {
            for min in [0, 64, 256] {
                let o = ChunkOpts {
                    target,
                    overlap_pct: pct,
                    min,
                };
                for (text, kind) in &texts {
                    let chunks = run(text, *kind, &o);
                    assert!(!chunks.is_empty());
                }
            }
        }
    }
}

#[test]
fn overlap_exists() {
    for (text, kind) in [
        (chinese_paragraph(), TextKind::Text),
        (english_text(), TextKind::Text),
    ] {
        let chunks = run(&text, kind, &opts());
        assert!(chunks.len() >= 2, "{kind:?} 应切成多片");
        for w in chunks.windows(2) {
            assert!(w[1].char_start < w[0].char_end, "{kind:?} 相邻片段没有重叠");
            let shared: String = text
                .chars()
                .skip(w[1].char_start)
                .take(w[0].char_end - w[1].char_start)
                .collect();
            // 重叠的正文 token 数约为目标的 15%（57），不超过两倍。
            let n = fake(&shared).unwrap() - 12;
            assert!((1..=114).contains(&n), "{kind:?} 重叠 {n} 个 token");
        }
    }
    // 比例为 0 时不重叠。
    let o = ChunkOpts {
        target: 384,
        overlap_pct: 0,
        min: 0,
    };
    let chunks = run(&chinese_paragraph(), TextKind::Text, &o);
    for w in chunks.windows(2) {
        assert!(w[1].char_start >= w[0].char_end);
    }
}

#[test]
fn markdown_headings() {
    let o = ChunkOpts {
        target: 60,
        overlap_pct: 15,
        min: 0,
    };
    let chunks = run(MARKDOWN, TextKind::Markdown, &o);
    let headings: Vec<&str> = chunks.iter().map(|c| c.heading.as_str()).collect();
    assert!(
        headings.contains(&"用户手册 > 安装 > Windows"),
        "{headings:?}"
    );
    assert!(
        headings.contains(&"用户手册 > 安装 > Android"),
        "{headings:?}"
    );
    assert!(
        headings.contains(&"用户手册 > 使用 > 深层标题"),
        "{headings:?}"
    );
    // 代码块中的 `#` 行不是标题；只有标题的段并入下一段。
    assert!(!headings.iter().any(|h| h.contains("这一行")));
    assert!(
        chunks
            .iter()
            .all(|c| c.text.lines().any(|l| md_heading(l).is_none()))
    );
    // 标题处断开，不跨节重叠。
    for w in chunks.windows(2) {
        if w[0].heading != w[1].heading {
            assert!(w[1].char_start >= w[0].char_end);
        }
    }
}

#[test]
fn small_paragraphs_merged() {
    let text = "第一段。\n\n第二段。\n\n第三段。";
    let chunks = run(text, TextKind::Text, &opts());
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].text, text);
    assert_eq!((chunks[0].line_start, chunks[0].line_end), (1, 5));
}

/// 首个标题前只有页码，正文另起一章。
fn leading_page_number() -> String {
    format!("12\n\n# 第一章\n\n{}", "第一章的正文内容。".repeat(30))
}

#[test]
fn snapshot_short_merged() {
    insta::assert_snapshot!(render(&run(
        &leading_page_number(),
        TextKind::Markdown,
        &opts()
    )));
}

#[test]
fn short_chunks_merged() {
    // 页码并入下一片，标题路径取正文较长的一方。
    let text = leading_page_number();
    let chunks = run(&text, TextKind::Markdown, &opts());
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].text.starts_with("12\n\n# 第一章"));
    assert_eq!((chunks[0].char_start, chunks[0].line_start), (0, 1));
    assert_eq!(chunks[0].heading, "第一章");
    // 0 表示不合并。
    let o = ChunkOpts { min: 0, ..opts() };
    let chunks = run(&text, TextKind::Markdown, &o);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].text, "12");
    // 最后一片并入上一片，重叠部分不重复。
    let text = format!("{}\n\n# 附注\n\n注一。", chinese_paragraph());
    let before = run(&text, TextKind::Markdown, &ChunkOpts { min: 0, ..opts() });
    let chunks = run(&text, TextKind::Markdown, &opts());
    assert_eq!(chunks.len(), before.len() - 1);
    let last = chunks.last().unwrap();
    assert!(last.text.ends_with("# 附注\n\n注一。"));
    assert_eq!(last.char_start, before[before.len() - 2].char_start);
    assert_eq!(last.heading, "");
    // 合并后超过上限时保持原样。
    let text = format!("短。\n\n{}", "长".repeat(1000));
    let chunks = run(&text, TextKind::Text, &opts());
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].text, "短。");
}

#[test]
fn counter_error_propagates() {
    let mut count = |_: &str| Err::<usize, _>("推理进程不可用");
    let err = chunk("正文", TextKind::Text, &opts(), &mut count).unwrap_err();
    assert_eq!(err, ChunkError::Count("推理进程不可用"));
}

#[test]
fn large_prefix() {
    // 前缀占 200 个 token：每片仍不超过上限。
    let mut count = |s: &str| Ok::<_, Infallible>(s.chars().count() / 2 + 200);
    let text = chinese_paragraph();
    let chunks = chunk(&text, TextKind::Text, &opts(), &mut count).unwrap();
    assert!(chunks.len() >= 2);
    for c in &chunks {
        assert!(c.text.chars().count() / 2 + 200 <= MAX_TOKENS);
    }
    // 前缀占一半以上时报错。
    let mut count = |s: &str| Ok::<_, Infallible>(s.chars().count() / 2 + MAX_TOKENS / 2);
    let err = chunk(&text, TextKind::Text, &opts(), &mut count).unwrap_err();
    assert_eq!(err, ChunkError::PrefixTooLong);
    assert_eq!(err.to_string(), "文件名过长，无法识别内容。");
}

#[test]
fn heading_with_long_paragraph() {
    // 正文 482 token：标题加正文超过目标 384、不超过上限，合在一片。
    let body = "长".repeat(940);
    let text = format!("# 第一章\n\n{body}\n");
    let chunks = run(&text, TextKind::Markdown, &opts());
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].text.starts_with("# 第一章"));
    assert_eq!(chunks[0].heading, "第一章");
    // 正文正好 512 token：标题放不下，不单独成片，标题路径带到正文片。
    let body = "长".repeat(1000);
    for text in [
        format!("# 第一章\n\n{body}\n"),
        format!("# 第一章\n## 第一节\n\n{body}\n"),
    ] {
        let chunks = run(&text, TextKind::Markdown, &opts());
        assert_eq!(chunks.len(), 1, "{chunks:?}");
        assert_eq!(chunks[0].text, body);
        assert!(chunks[0].heading.starts_with("第一章"));
    }
    // 超长段落按句切开，标题与第一句同片。
    let text = format!("# 第一章\n\n{}", chinese_paragraph());
    let chunks = run(&text, TextKind::Markdown, &opts());
    assert!(chunks.len() >= 2);
    assert!(chunks[0].text.starts_with("# 第一章\n\n第1句"));
    assert!(chunks.iter().all(|c| c.heading == "第一章"));
    // 全文只有标题时保留。
    let chunks = run("# 只有标题", TextKind::Markdown, &opts());
    assert_eq!(chunks.len(), 1);
    // 结尾的标题后面没有正文，不单独成片。
    let chunks = run("正文。\n\n# 结尾标题\n", TextKind::Markdown, &opts());
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].text, "正文。");
}

#[test]
fn markdown_closing_hashes_and_fences() {
    assert_eq!(md_heading("## C#").unwrap().1, "C#");
    assert_eq!(md_heading("## C# ##").unwrap().1, "C#");
    assert_eq!(md_heading("# 标题 #").unwrap().1, "标题");
    assert_eq!(md_heading("# ###").unwrap().1, "");
    assert!(md_heading("#标题").is_none());
    assert!(md_heading("    # 缩进代码").is_none());
    // `~~~` 代码块中的 ``` 不关闭它，其中的 `#` 行不是标题；长度不足的围栏也不关闭。
    let text = "# 外\n\n~~~\n```\n# 不是标题\n~~\n# 也不是\n~~~~\n\n## 内\n\n正文\n";
    let o = ChunkOpts {
        target: 10,
        overlap_pct: 0,
        min: 0,
    };
    let chunks = run(text, TextKind::Markdown, &o);
    let headings: Vec<&str> = chunks.iter().map(|c| c.heading.as_str()).collect();
    assert_eq!(headings, ["外", "外 > 内"]);
}

#[test]
fn oversized_segment_not_counted() {
    // 一段超过 8 KiB、没有任何分隔的英文：计数请求都不超过 8 KiB，结果仍不超过上限。
    let text = "abcdefghij".repeat(1000);
    let mut longest = 0;
    let mut count = |s: &str| {
        longest = longest.max(s.len());
        fake(s)
    };
    let chunks = chunk(&text, TextKind::Text, &opts(), &mut count).unwrap();
    check(&text, &chunks);
    assert!(longest <= COUNT_MAX_BYTES, "计数请求有 {longest} 字节");
    assert_eq!(
        chunks.iter().map(|c| c.text.len()).sum::<usize>(),
        text.len()
    );
}

#[test]
fn crlf_file_offsets() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    let raw = format!(
        "{}\r\n\r\n{}\r\n",
        chinese_paragraph(),
        english_text().replace('\n', "\r\n")
    );
    std::fs::write(&file, raw).unwrap();
    let text = crate::read_text(&file, 1 << 20).unwrap().text;
    assert!(!text.contains('\r'));
    // `check` 按规范化后的文本核对偏移与行号。
    let chunks = run(&text, TextKind::Text, &opts());
    assert!(chunks.len() >= 3);
    assert!(chunks.iter().all(|c| !c.text.contains('\r')));
}

#[test]
fn four_byte_chars() {
    // emoji 与扩展区汉字（UTF-8 四字节）：偏移按 Unicode 标量计，二分不会切在字符中间。
    let text = format!("开头😀。\n\n{}\n\n𠀀𠀁结尾🎉", "😀𠀀".repeat(700));
    let chunks = run(&text, TextKind::Text, &opts());
    assert!(chunks.len() >= 2);
    assert!(chunks[0].text.starts_with("开头😀"));
    assert!(chunks.last().unwrap().text.ends_with("𠀀𠀁结尾🎉"));
}

#[test]
fn opts_from_params() {
    assert_eq!(
        opts(),
        ChunkOpts {
            target: 384,
            overlap_pct: 15,
            min: 64
        }
    );
    let values = BTreeMap::from([
        ("chunk_tokens".to_string(), ParamValue::Int(256)),
        // 越界的取值回到默认值。
        ("chunk_overlap_pct".to_string(), ParamValue::Int(90)),
        ("chunk_min_tokens".to_string(), ParamValue::Int(0)),
    ]);
    assert_eq!(
        ChunkOpts::from_params(&values),
        ChunkOpts {
            target: 256,
            overlap_pct: 15,
            min: 0
        }
    );
}
