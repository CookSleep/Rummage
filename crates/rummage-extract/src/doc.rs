//! 经 Xberg 提取正文的文档：PDF、Office、OpenDocument、RTF、HTML、XML、CSV、JSON。
//!
//! - 字节由 `read_bytes` 读出（先做占位文件检查、套文档大小上限），Xberg 只解析内存中的字节，不自己打开文件。
//! - 输出 Markdown：标题成为 `#` 行，表格成为 Markdown 表格，交给 `chunk` 按 Markdown 切片，片段记录标题路径。
//!   关闭 Markdown 转义，正文与原文一致；关闭结果缓存（不往数据目录之外写）与 OCR；HTML 不输出元数据头。
//! - 页码：开启 `pages.extract_pages` 后 Xberg 在 `pages` 中逐页给出 Markdown 正文，逐页拼接并记下每页的起点。
//!   `metadata.pages.boundaries` 的字节范围对应纯文本而不是 Markdown 正文（1.3.9 实测），不能用。
//!   只采用 PDF 的页与演示文稿的幻灯片；Word 等流式文档只能按显式分页符分页，与实际页码不符，不记页码。
//! - 加密或受密码保护：Xberg 报错信息含 encrypt / password（PDF、XLS、ODT），或本应是 ZIP 的 OOXML 实为 CFB 容器
//!   （加密包，MS-OFFCRYPTO），或 Word 97–2003 文件头的 `fEncrypted` 位为 1。加密的 PPT 由 Xberg 返回空正文，按无内容处理。
//! - Xberg 只有异步接口，在调用线程上建一个单线程运行时执行。超时与 panic 由调用方的任务线程处理。

use std::io::{Cursor, Read};
use std::path::Path;

use xberg::{
    ConversionOptions, ExtractInput, ExtractionConfig, OutputFormat, PageConfig, PageUnitType,
};

use crate::read::{ReadError, read_bytes};

/// 经 Xberg 提取的扩展名（小写、不含点）；`.txt`、`.md` 直接读取。
pub const XBERG_EXTS: &[&str] = &[
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "rtf", "html", "htm", "xml",
    "csv", "json",
];

/// CFB（OLE 复合文档）的文件头
const CFB_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doc {
    /// Markdown 正文，换行为 `\n`
    pub text: String,
    /// 每页的（起始字符偏移，页码），按偏移升序；没有页码时为空
    pub pages: Vec<(usize, u32)>,
    /// 页码的单位：`page`（PDF）或 `slide`（演示文稿）
    pub page_unit: &'static str,
}

impl Doc {
    /// 字符范围 `[start, end)` 所在的首页与末页
    pub fn page_range(&self, start: usize, end: usize) -> Option<(u32, u32)> {
        let at = |i: usize| self.pages.iter().rev().find(|p| p.0 <= i).map(|p| p.1);
        Some((at(start)?, at(end.saturating_sub(1).max(start))?))
    }
}

/// 消息都是能直接展示给用户的中文整句。
#[derive(Debug, thiserror::Error)]
pub enum DocError {
    #[error(transparent)]
    Read(#[from] ReadError),
    #[error("\"{0}\" 已加密或受密码保护，只能按文件名搜索。")]
    Encrypted(String),
    #[error("无法提取 \"{0}\" 的正文，文件可能已损坏或格式不受支持。")]
    Extract(String),
}

pub fn is_xberg(ext: &str) -> bool {
    XBERG_EXTS.contains(&ext)
}

pub fn extract_doc(path: &Path, max_bytes: u64) -> Result<Doc, DocError> {
    let bytes = read_bytes(path, max_bytes)?;
    let shown = path.display().to_string();
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    // Xberg 拿走字节之前先看容器
    let sealed = sealed(&ext, &bytes);
    let fail = |msg: String| {
        let lower = msg.to_lowercase();
        if sealed || lower.contains("encrypt") || lower.contains("password") {
            return DocError::Encrypted(shown.clone());
        }
        // Xberg 的英文报错只写日志
        log::warn!("无法提取“{shown}”的正文：{msg}");
        DocError::Extract(shown.clone())
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| fail(err.to_string()))?;
    let cfg = ExtractionConfig {
        use_cache: false,
        disable_ocr: true,
        escape_markdown: false,
        output_format: OutputFormat::Markdown,
        pages: Some(PageConfig {
            extract_pages: true,
            ..Default::default()
        }),
        html_options: Some(ConversionOptions {
            extract_metadata: false,
            ..Default::default()
        }),
        ..Default::default()
    };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    // 类型按内容判断，文件名只作参考
    let input = ExtractInput::from_bytes(bytes, "application/octet-stream", name);
    let out = rt
        .block_on(xberg::extract(input, &cfg))
        .map_err(|err| fail(err.to_string()))?;
    let Some(d) = out.results.into_iter().next() else {
        return Err(fail("Xberg 没有返回提取结果".to_string()));
    };
    let unit = match d.metadata.pages.as_ref().map(|p| p.unit_type) {
        Some(PageUnitType::Slide) => Some("slide"),
        Some(PageUnitType::Page) if d.mime_type == "application/pdf" => Some("page"),
        _ => None,
    };
    let normalize = |s: &str| s.replace("\r\n", "\n").replace('\r', "\n");
    let per_page = d.pages.filter(|p| unit.is_some() && !p.is_empty());
    let Some(per_page) = per_page else {
        return Ok(Doc {
            text: normalize(&d.content),
            pages: Vec::new(),
            page_unit: unit.unwrap_or("page"),
        });
    };
    // 逐页拼接，页与页之间空一行（与整篇的 Markdown 相同），同时记下每页的起始字符偏移
    let (mut text, mut pages, mut chars) = (String::new(), Vec::new(), 0);
    for p in per_page {
        if !text.is_empty() {
            text.push_str("\n\n");
            chars += 2;
        }
        let part = normalize(p.content.trim_end());
        pages.push((chars, p.page_number));
        chars += part.chars().count();
        text.push_str(&part);
    }
    Ok(Doc {
        text,
        pages,
        page_unit: unit.unwrap_or("page"),
    })
}

/// 加密的 Office 文件：本应是 ZIP 的 OOXML 实为 CFB 容器，或 Word 97–2003 的 `fEncrypted` 位为 1（模块文档）
fn sealed(ext: &str, bytes: &[u8]) -> bool {
    if !bytes.starts_with(&CFB_MAGIC) {
        return false;
    }
    match ext {
        "docx" | "xlsx" | "pptx" => true,
        "doc" => {
            // FIB 偏移 10 处的标志字，0x0100 为 fEncrypted（MS-DOC 2.5.2）
            let mut head = [0u8; 12];
            cfb::CompoundFile::open(Cursor::new(bytes))
                .and_then(|mut f| f.open_stream("WordDocument")?.read_exact(&mut head))
                .is_ok_and(|_| u16::from_le_bytes([head[10], head[11]]) & 0x0100 != 0)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::fmt::Write;
    use std::path::PathBuf;

    use super::*;
    use crate::{ChunkOpts, TextKind, chunk};

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name)
    }

    /// 提取结果与切片（目标长度调小，使各节分别成片）的定位信息
    fn render(name: &str) -> String {
        let doc = extract_doc(&fixture(name), 1 << 20).unwrap();
        let opts = ChunkOpts {
            target: 40,
            overlap_pct: 0,
            min: 0,
        };
        let mut count = |s: &str| Ok::<_, Infallible>(s.chars().count() / 2 + 12);
        let chunks = chunk(&doc.text, TextKind::Markdown, &opts, &mut count).unwrap();
        let mut out = format!(
            "页：{:?} {:?}\n--- 正文\n{}\n",
            doc.page_unit, doc.pages, doc.text
        );
        for c in &chunks {
            let chars: String = doc
                .text
                .chars()
                .skip(c.char_start)
                .take(c.char_end - c.char_start)
                .collect();
            assert_eq!(chars, c.text);
            writeln!(
                out,
                "=== 字符 {}..{} 页 {:?} 标题“{}”\n{}",
                c.char_start,
                c.char_end,
                doc.page_range(c.char_start, c.char_end),
                c.heading,
                c.text
            )
            .unwrap();
        }
        out
    }

    #[test]
    fn snapshot_formats() {
        for ext in XBERG_EXTS {
            // `.htm` 与 `.html` 相同
            if *ext == "htm" {
                continue;
            }
            insta::assert_snapshot!(format!("sample_{ext}"), render(&format!("sample.{ext}")));
        }
    }

    #[test]
    fn pages_of_pdf_and_slides() {
        let pdf = extract_doc(&fixture("sample.pdf"), 1 << 20).unwrap();
        assert_eq!(pdf.page_unit, "page");
        assert_eq!(pdf.pages.iter().map(|p| p.1).collect::<Vec<_>>(), [1, 2]);
        let second = pdf.text.find("# 常见问题").unwrap();
        let at = pdf.text[..second].chars().count();
        assert_eq!(pdf.page_range(at, at + 4), Some((2, 2)));
        assert_eq!(pdf.page_range(0, at + 4), Some((1, 2)));
        let pptx = extract_doc(&fixture("sample.pptx"), 1 << 20).unwrap();
        assert_eq!(pptx.page_unit, "slide");
        assert_eq!(pptx.pages.len(), 2);
        // Word 的分页符不当作页码
        let docx = extract_doc(&fixture("sample.docx"), 1 << 20).unwrap();
        assert!(docx.pages.is_empty());
        assert_eq!(docx.page_range(0, 1), None);
    }

    #[test]
    fn encrypted_documents() {
        for name in [
            "encrypted.pdf",
            "encrypted.docx",
            "encrypted.doc",
            "encrypted.xls",
        ] {
            let err = extract_doc(&fixture(name), 1 << 20).unwrap_err();
            assert!(matches!(err, DocError::Encrypted(_)), "{name}：{err}");
            assert!(
                err.to_string()
                    .ends_with("已加密或受密码保护，只能按文件名搜索。")
            );
        }
        // 未加密的 Word 97–2003 文件头标志位不误判
        let bytes = std::fs::read(fixture("sample.doc")).unwrap();
        assert!(!sealed("doc", &bytes));
    }

    #[test]
    fn broken_and_limits() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.pdf");
        std::fs::write(&bad, b"%PDF-1.4\n not really").unwrap();
        let err = extract_doc(&bad, 1 << 20).unwrap_err();
        assert!(matches!(err, DocError::Extract { .. }), "{err}");
        let err = extract_doc(&fixture("sample.docx"), 1024).unwrap_err();
        assert!(
            matches!(err, DocError::Read(ReadError::TooLarge { .. })),
            "{err}"
        );
        let err = extract_doc(&dir.path().join("missing.pdf"), 1024).unwrap_err();
        assert!(matches!(err, DocError::Read(ReadError::Fs(_))), "{err}");
        assert!(is_xberg("pdf") && !is_xberg("md") && !is_xberg("txt"));
    }
}
