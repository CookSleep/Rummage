//! `.ts` / `.mts` 是 MPEG-TS 视频还是 TypeScript 代码。
//!
//! 只读文件头：188 字节的包在偏移 0、188、376 处都是同步字节 `0x47`；`.m2ts` / `.mts` 的 192 字节包
//! 前面多 4 字节时间码，同步字节在偏移 4、196、388。两种都不满足或文件太短时不是视频。
//! 读取前先用 `rummage_index::ensure_local` 检查网盘占位文件。

use std::fs::File;
use std::io::Read;
use std::path::Path;

use rummage_index::ensure_local;

use crate::read::ReadError;

const SYNC: u8 = 0x47;
/// 两种包长都需要的文件头长度：192 字节包的第三个同步字节在偏移 388。
const HEAD_BYTES: usize = 389;

pub fn is_mpeg_ts(path: &Path) -> Result<bool, ReadError> {
    ensure_local(path)?;
    let io_err = |source: std::io::Error| {
        log::warn!("无法读取 {}：{source}", path.display());
        ReadError::Io {
            path: path.display().to_string(),
            source,
        }
    };
    let mut head = Vec::with_capacity(HEAD_BYTES);
    File::open(path)
        .map_err(io_err)?
        .take(HEAD_BYTES as u64)
        .read_to_end(&mut head)
        .map_err(io_err)?;
    Ok(is_ts_head(&head))
}

fn is_ts_head(head: &[u8]) -> bool {
    let synced = |offsets: [usize; 3]| offsets.iter().all(|&i| head.get(i) == Some(&SYNC));
    synced([0, 188, 376]) || synced([4, 196, 388])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packets(size: usize, sync_at: usize, n: usize) -> Vec<u8> {
        let mut bytes = vec![0u8; size * n];
        for p in 0..n {
            bytes[p * size + sync_at] = SYNC;
        }
        bytes
    }

    fn check(bytes: &[u8]) -> bool {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.ts");
        std::fs::write(&file, bytes).unwrap();
        is_mpeg_ts(&file).unwrap()
    }

    #[test]
    fn detects_both_packet_sizes() {
        assert!(check(&packets(188, 0, 3)));
        assert!(check(&packets(188, 0, 100)));
        assert!(check(&packets(192, 4, 3)));
        assert!(check(&packets(192, 4, 100)));
    }

    #[test]
    fn typescript_and_short_files() {
        let code = "export const greet = (name: string) => `Hello, ${name}`\n".repeat(20);
        assert!(!check(code.as_bytes()));
        assert!(!check(b""));
        assert!(!check(&[SYNC]));
        // 只有前两个包：第三个同步字节不在文件中。
        assert!(!check(&packets(188, 0, 2)));
        // 有一个同步字节错位。
        let mut bytes = packets(188, 0, 3);
        bytes[188] = 0;
        bytes[189] = SYNC;
        assert!(!check(&bytes));
        // `G` 开头的文本（0x47）。
        assert!(!check("Generated file\n".repeat(40).as_bytes()));
    }

    #[test]
    fn missing_file_is_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(is_mpeg_ts(&dir.path().join("missing.ts")).is_err());
    }
}
