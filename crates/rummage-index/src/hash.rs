//! 文件哈希，都用 BLAKE3，读取前先调用 `fsx::ensure_local`。
//! - `quick_hash`：文件大小（8 字节小端）+ 首尾各 64 KiB 的内容；不超过 128 KiB 的文件取全部内容。
//!   用于失败黑名单与改名、移动的识别。只读首尾，内容中间变了而大小不变时识别不出。
//!   `quick_hash_with_id` 顺便用同一个句柄取文件 ID（`platform::file_id`），不额外打开文件。
//! - `content_hash`：全部内容，用于内容相同的图片只算一次向量。
//! - `file_id`：只取文件 ID，不读内容，扫描配对改名与移动时用。
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use crate::fsx::{self, FsError};
use crate::platform;

/// 首尾各取的字节数
const EDGE: u64 = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum HashError {
    #[error(transparent)]
    Fs(#[from] FsError),
    /// 系统错误只写日志
    #[error("无法读取 \"{path}\"：{}", fsx::io_cause(.source))]
    Io { path: String, source: io::Error },
}

fn open(path: &Path) -> Result<File, HashError> {
    fsx::ensure_local(path)?;
    File::open(path).map_err(|source| io_err(path, source))
}

fn io_err(path: &Path, source: io::Error) -> HashError {
    log::warn!("无法读取 {}：{source}", path.display());
    HashError::Io {
        path: fsx::shown(path),
        source,
    }
}

pub fn quick_hash(path: &Path) -> Result<[u8; 32], HashError> {
    quick_hash_with_id(path).map(|(h, _)| h)
}

/// `quick_hash` 与文件 ID（卷序列号 + 文件 ID，只在 NTFS 上有，见 `platform::file_id`）
pub fn quick_hash_with_id(path: &Path) -> Result<([u8; 32], Option<String>), HashError> {
    let mut file = open(path)?;
    let id = platform::file_id(&file);
    let res = (|| {
        let size = file.metadata()?.len();
        let mut hasher = blake3::Hasher::new();
        hasher.update(&size.to_le_bytes());
        if size <= 2 * EDGE {
            hasher.update_reader(&mut file)?;
        } else {
            hasher.update_reader((&mut file).take(EDGE))?;
            file.seek(SeekFrom::Start(size - EDGE))?;
            hasher.update_reader((&mut file).take(EDGE))?;
        }
        Ok(*hasher.finalize().as_bytes())
    })();
    res.map(|h| (h, id)).map_err(|err| io_err(path, err))
}

pub fn file_id(path: &Path) -> Result<Option<String>, HashError> {
    Ok(platform::file_id(&open(path)?))
}

pub fn content_hash(path: &Path) -> Result<[u8; 32], HashError> {
    let mut file = open(path)?;
    let mut hasher = blake3::Hasher::new();
    hasher
        .update_reader(&mut file)
        .map_err(|err| io_err(path, err))?;
    Ok(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expected_quick(bytes: &[u8]) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(&(bytes.len() as u64).to_le_bytes());
        let n = bytes.len();
        if n as u64 <= 2 * EDGE {
            h.update(bytes);
        } else {
            h.update(&bytes[..EDGE as usize]);
            h.update(&bytes[n - EDGE as usize..]);
        }
        *h.finalize().as_bytes()
    }

    #[test]
    fn hashes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.bin");
        for n in [
            0usize,
            10,
            2 * EDGE as usize,
            2 * EDGE as usize + 1,
            300_000,
        ] {
            let bytes: Vec<u8> = (0..n).map(|i| (i * 7 % 251) as u8).collect();
            std::fs::write(&file, &bytes).unwrap();
            assert_eq!(quick_hash(&file).unwrap(), expected_quick(&bytes), "{n}");
            assert_eq!(
                content_hash(&file).unwrap(),
                *blake3::hash(&bytes).as_bytes()
            );
        }
        // 只改中间的内容：quick_hash 不变，内容哈希改变
        let mut bytes = vec![1u8; 300_000];
        std::fs::write(&file, &bytes).unwrap();
        let (q, c) = (quick_hash(&file).unwrap(), content_hash(&file).unwrap());
        bytes[150_000] = 2;
        std::fs::write(&file, &bytes).unwrap();
        assert_eq!(quick_hash(&file).unwrap(), q);
        assert_ne!(content_hash(&file).unwrap(), c);
        // 大小变了 quick_hash 也变
        bytes.push(1);
        std::fs::write(&file, &bytes).unwrap();
        assert_ne!(quick_hash(&file).unwrap(), q);

        // 文件 ID：同一个文件改名后不变，不同文件不同（只在 NTFS 上有）
        let (_, id) = quick_hash_with_id(&file).unwrap();
        assert_eq!(file_id(&file).unwrap(), id);
        let other = dir.path().join("b.bin");
        std::fs::write(&other, b"x").unwrap();
        let renamed = dir.path().join("c.bin");
        std::fs::rename(&file, &renamed).unwrap();
        assert_eq!(file_id(&renamed).unwrap(), id);
        if id.is_some() {
            assert_ne!(file_id(&other).unwrap(), id);
        }

        let err = quick_hash(&dir.path().join("missing")).unwrap_err();
        assert!(
            matches!(err, HashError::Fs(FsError::Inaccessible(_))),
            "{err}"
        );
    }
}
