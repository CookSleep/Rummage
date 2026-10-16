//! 文档提取与切片：纯文本直接读取并解码（`read`），PDF、Office 等经 Xberg 提取为
//! Markdown（`doc`），判断 `.ts` / `.mts` 是否为 MPEG-TS 视频（`ts`），按结构切片（`chunk`），列出并取出文档的内嵌媒体（`media`）。

pub mod chunk;
pub mod doc;
pub mod media;
pub mod read;
pub mod ts;

use std::path::Path;

pub use chunk::{Chunk, ChunkOpts, MAX_TOKENS, TextKind, chunk};
pub use doc::{Doc, DocError, extract_doc, is_xberg};
pub use read::{ReadError, TextFile, read_bytes, read_text};
pub use ts::is_mpeg_ts;

/// 文档片段的标题：文件名去掉扩展名；前缀由 rummage-embed 拼接。
pub fn title_for(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_strips_extension() {
        assert_eq!(
            title_for(Path::new(r"D:\资料\年度报告.final.md")),
            "年度报告.final"
        );
        assert_eq!(title_for(Path::new("src/main.rs")), "main");
        assert_eq!(title_for(Path::new("README")), "README");
        assert_eq!(title_for(Path::new(".gitignore")), ".gitignore");
    }
}
