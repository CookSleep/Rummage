//! 纯文本文件（`.txt`、`.md`）的读取：不经 Xberg，直接读字节并解码。
//! 读字节的部分（`read_bytes`）也供经 Xberg 提取的文档使用。
//!
//! - 读取前先用 `rummage_index::ensure_local` 检查网盘占位文件，超过大小上限的不读。
//! - 编码：有 BOM（UTF-8、UTF-16LE、UTF-16BE）按 BOM 解码；否则基本合法的 UTF-8 按 UTF-8
//!   （只是末尾不完整，或坏字节不到 1%，例如被截断或个别字节损坏的文件）；
//!   否则用 chardetng 猜测编码（兼顾 GBK 等旧编码）再用 encoding_rs 解码，无法解码的字节换成 U+FFFD。
//! - 没有 BOM 且前 8 KiB 中有 NUL 字节时视为二进制文件（UTF-16 文本没有 BOM 时也会落到这里，属于少见情况）。
//! - 换行统一成 `\n`。

use std::fs::File;
use std::io::Read;
use std::path::Path;

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::{Encoding, UTF_8};
use rummage_index::{FsError, ensure_local, io_cause};

/// 检查 NUL 字节的范围。
const SNIFF_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFile {
    /// 换行已统一成 `\n`。
    pub text: String,
    /// 实际使用的编码名称（WHATWG 名称，如 `UTF-8`、`GBK`、`UTF-16LE`）。
    pub encoding: &'static str,
}

/// 消息都是能直接展示给用户的中文整句。
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error(transparent)]
    Fs(#[from] FsError),
    /// 系统错误只写日志
    #[error("无法读取 \"{path}\"：{}", io_cause(.source))]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("\"{path}\" 有 {size} 字节，超过文档大小上限 {max} 字节，不读取正文。")]
    TooLarge { path: String, size: u64, max: u64 },
    #[error("\"{0}\" 不是文本文件，不读取正文。")]
    NotText(String),
}

pub fn read_text(path: &Path, max_bytes: u64) -> Result<TextFile, ReadError> {
    let bytes = read_bytes(path, max_bytes)?;
    decode(&bytes).ok_or_else(|| ReadError::NotText(path.display().to_string()))
}

/// 检查占位文件后读出全部字节，超过 `max_bytes` 时报错；文本与经 Xberg 提取的文档共用。
pub fn read_bytes(path: &Path, max_bytes: u64) -> Result<Vec<u8>, ReadError> {
    ensure_local(path)?;
    let io_err = |source: std::io::Error| {
        log::warn!("无法读取 {}：{source}", path.display());
        ReadError::Io {
            path: path.display().to_string(),
            source,
        }
    };
    let file = File::open(path).map_err(io_err)?;
    let size = file.metadata().map_err(io_err)?.len();
    let too_large = |size| ReadError::TooLarge {
        path: path.display().to_string(),
        size,
        max: max_bytes,
    };
    if size > max_bytes {
        return Err(too_large(size));
    }
    // 打开后文件可能还在变大，多读一个字节来发现。
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(io_err)?;
    if bytes.len() as u64 > max_bytes {
        return Err(too_large(bytes.len() as u64));
    }
    Ok(bytes)
}

/// 合法的 UTF-8；或只是末尾不完整；或坏字节占比低于 1%。坏字节在解码时换成 U+FFFD。
fn mostly_utf8(bytes: &[u8]) -> bool {
    let Err(err) = std::str::from_utf8(bytes) else {
        return true;
    };
    if err.error_len().is_none() {
        return true;
    }
    let bad: usize = bytes.utf8_chunks().map(|c| c.invalid().len()).sum();
    bad * 100 < bytes.len()
}

/// 二进制内容返回 `None`。
fn decode(bytes: &[u8]) -> Option<TextFile> {
    let (enc, body): (&'static Encoding, &[u8]) = match Encoding::for_bom(bytes) {
        Some((enc, bom)) => (enc, &bytes[bom..]),
        None => {
            if bytes[..bytes.len().min(SNIFF_BYTES)].contains(&0) {
                return None;
            }
            let enc = if mostly_utf8(bytes) {
                UTF_8
            } else {
                let mut det = EncodingDetector::new(Iso2022JpDetection::Deny);
                det.feed(bytes, true);
                det.guess(None, Utf8Detection::Allow)
            };
            (enc, bytes)
        }
    };
    let (text, _) = enc.decode_without_bom_handling(body);
    Some(TextFile {
        text: text.replace("\r\n", "\n").replace('\r', "\n"),
        encoding: enc.name(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "第一行：中文与 English。\r\n第二行\r第三行\n";
    const NORMALIZED: &str = "第一行：中文与 English。\n第二行\n第三行\n";

    fn read_bytes(bytes: &[u8]) -> Result<TextFile, ReadError> {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, bytes).unwrap();
        read_text(&file, 1024)
    }

    #[test]
    fn utf8_with_and_without_bom() {
        let got = read_bytes(SAMPLE.as_bytes()).unwrap();
        assert_eq!(got.text, NORMALIZED);
        assert_eq!(got.encoding, "UTF-8");
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(SAMPLE.as_bytes());
        let got = read_bytes(&bom).unwrap();
        assert_eq!(got.text, NORMALIZED);
        assert_eq!(got.encoding, "UTF-8");
    }

    #[test]
    fn utf16_with_bom() {
        let mut le = vec![0xFF, 0xFE];
        let mut be = vec![0xFE, 0xFF];
        for u in SAMPLE.encode_utf16() {
            le.extend_from_slice(&u.to_le_bytes());
            be.extend_from_slice(&u.to_be_bytes());
        }
        let got = read_bytes(&le).unwrap();
        assert_eq!(got.text, NORMALIZED);
        assert_eq!(got.encoding, "UTF-16LE");
        let got = read_bytes(&be).unwrap();
        assert_eq!(got.text, NORMALIZED);
        assert_eq!(got.encoding, "UTF-16BE");
    }

    #[test]
    fn utf8_tolerates_damage() {
        // 末尾的多字节字符被截断。
        let mut cut = "中文文本".as_bytes().to_vec();
        cut.pop();
        let got = read_bytes(&cut).unwrap();
        assert_eq!(got.encoding, "UTF-8");
        assert_eq!(got.text, "中文文\u{FFFD}");
        // 中间有个别坏字节（低于 1%）。
        let mut bad = "正常的中文内容。".repeat(20).into_bytes();
        bad[30] = 0xFF;
        let got = read_bytes(&bad).unwrap();
        assert_eq!(got.encoding, "UTF-8");
        assert!(got.text.contains('\u{FFFD}'));
        assert!(got.text.starts_with("正常的中文内容。正常"));
    }

    #[test]
    fn crlf_and_emoji() {
        let got = read_bytes("第一行😀\r\n𝄞第二行\r\n".as_bytes()).unwrap();
        assert_eq!(got.text, "第一行😀\n𝄞第二行\n");
    }

    #[test]
    fn gbk_guessed() {
        let text = "这是一个用国标码保存的中文文本文件，记录了安装步骤与常见问题。\r\n第二段：请先关闭杀毒软件。\r\n";
        let (gbk, _, bad) = encoding_rs::GBK.encode(text);
        assert!(!bad);
        assert!(std::str::from_utf8(&gbk).is_err());
        let got = read_bytes(&gbk).unwrap();
        assert_eq!(got.encoding, "GBK");
        assert_eq!(got.text, text.replace("\r\n", "\n"));
    }

    #[test]
    fn binary_rejected() {
        let mut bytes = b"PK\x03\x04".to_vec();
        bytes.extend_from_slice(&[0, 0, 1, 2, 3]);
        let err = read_bytes(&bytes).unwrap_err();
        assert!(matches!(err, ReadError::NotText(_)), "{err}");
        // 8 KiB 之后才出现的 NUL 不影响判定。
        let mut late = vec![b'a'; SNIFF_BYTES];
        late.push(0);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("late.txt");
        std::fs::write(&file, &late).unwrap();
        assert!(read_text(&file, 1 << 20).is_ok());
    }

    #[test]
    fn size_limit_and_missing() {
        let err = read_bytes(&[b'a'; 1025]).unwrap_err();
        assert!(
            matches!(err, ReadError::TooLarge { size: 1025, .. }),
            "{err}"
        );
        assert_eq!(read_bytes(&[b'a'; 1024]).unwrap().text.len(), 1024);
        let dir = tempfile::tempdir().unwrap();
        let err = read_text(&dir.path().join("missing.txt"), 1024).unwrap_err();
        assert!(
            matches!(err, ReadError::Fs(FsError::Inaccessible(_))),
            "{err}"
        );
        // 空文件是合法的文本。
        assert_eq!(read_bytes(b"").unwrap().text, "");
    }
}
