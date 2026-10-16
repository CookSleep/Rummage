//! 网盘占位文件与卷类型检查。
//! - **任何读取文件内容的代码路径（打开、读字节、quick_hash、内容哈希、缩略图、解码、提取）都必须先调用
//!   `ensure_local`**，这是唯一的检查函数，不要另写一份。OneDrive 等网盘的占位文件一旦被打开或读取就会触发下载，
//!   几千个文件可能耗尽流量；`ensure_local` 用 `GetFileAttributesW` 读属性，不打开文件。
//! - 扫描时用目录枚举得到的属性调用 `is_placeholder_attrs`，同样不打开文件（见 `scan`）。
//! - `check_volume` 在添加根目录时拒绝网络驱动器和不认识的文件系统：不走 Cloud Files API 的网盘
//!   （虚拟磁盘挂载、读取即流式下载）无法用属性识别，只能按卷拦下。
//! - 非 Windows 平台没有这些属性，始终视为本地文件。
use std::path::{Component, Path, Prefix};

use crate::platform;

pub(crate) const FILE_ATTRIBUTE_HIDDEN: u32 = 0x0000_0002;
pub(crate) const FILE_ATTRIBUTE_SYSTEM: u32 = 0x0000_0004;
pub(crate) const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
pub(crate) const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
pub(crate) const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
pub(crate) const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

/// 支持索引的文件系统；`GetVolumeInformationW` 对 FAT12 / FAT16 返回 `FAT`
const FILE_SYSTEMS: &[&str] = &["NTFS", "ReFS", "exFAT", "FAT32", "FAT"];

/// 消息都是能直接展示给用户的中文整句
#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error("\"{0}\" 仅在云端，尚未下载到本机，不能读取内容。")]
    Placeholder(String),
    #[error("不支持索引这个位置：{path}（{reason}），请选择本机磁盘上的文件夹。")]
    Unsupported { path: String, reason: String },
    #[error("无法访问 \"{0}\"，请确认该位置存在且可以打开。")]
    Inaccessible(String),
}

/// 读取文件出错的类别，给用户看的原因按它选择
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoCause {
    Busy,
    NotFound,
    Denied,
    Other,
}

pub fn io_kind(err: &std::io::Error) -> IoCause {
    // 32、33：Windows 的共享冲突与锁定冲突，文件正被其他程序独占
    if cfg!(windows) && matches!(err.raw_os_error(), Some(32 | 33)) {
        return IoCause::Busy;
    }
    match err.kind() {
        std::io::ErrorKind::NotFound => IoCause::NotFound,
        std::io::ErrorKind::PermissionDenied => IoCause::Denied,
        _ => IoCause::Other,
    }
}

/// 读取文件出错时给用户看的原因（整句）；系统错误码等细节由调用方写日志
pub fn io_cause(err: &std::io::Error) -> &'static str {
    match io_kind(err) {
        IoCause::Busy => "文件正被其他程序占用。",
        IoCause::NotFound => "找不到该文件，它可能已被移动或删除。",
        IoCause::Denied => "没有读取权限。",
        IoCause::Other => "请确认它存在且可以打开。",
    }
}

/// 带有任一“按需下载”属性即为网盘占位文件
pub fn is_placeholder_attrs(attrs: u32) -> bool {
    attrs
        & (FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
            | FILE_ATTRIBUTE_RECALL_ON_OPEN
            | FILE_ATTRIBUTE_OFFLINE)
        != 0
}

/// 读取文件内容之前的统一检查：是占位文件时返回错误，不打开文件
pub fn ensure_local(path: &Path) -> Result<(), FsError> {
    let attrs = platform::file_attributes(path).map_err(|err| {
        log::warn!("无法读取 {} 的属性：{err}", path.display());
        FsError::Inaccessible(shown(path))
    })?;
    if is_placeholder_attrs(attrs) {
        return Err(FsError::Placeholder(shown(path)));
    }
    Ok(())
}

/// 只接受本地固定盘或可移动盘上 NTFS、ReFS、exFAT、FAT32（FAT）的卷
pub fn check_volume(path: &Path) -> Result<(), FsError> {
    let unsupported = |reason: String| FsError::Unsupported {
        path: shown(path),
        reason,
    };
    // UNC 路径一律是网络位置，不必去访问网络
    if let Some(Component::Prefix(p)) = path.components().next()
        && matches!(p.kind(), Prefix::UNC(..) | Prefix::VerbatimUNC(..))
    {
        return Err(unsupported("网络位置".into()));
    }
    let (kind, fs) = platform::volume(path).map_err(|err| {
        log::warn!("无法读取 {} 所在卷的信息：{err}", path.display());
        FsError::Inaccessible(shown(path))
    })?;
    if kind == platform::DRIVE_REMOTE {
        return Err(unsupported("网络驱动器".into()));
    }
    if kind != platform::DRIVE_FIXED && kind != platform::DRIVE_REMOVABLE {
        return Err(unsupported("不是本地磁盘或可移动磁盘".into()));
    }
    if !FILE_SYSTEMS.iter().any(|f| f.eq_ignore_ascii_case(&fs)) {
        let fs = if fs.is_empty() { "未知" } else { &fs };
        return Err(unsupported(format!("该磁盘的格式（{fs}）不受支持")));
    }
    Ok(())
}

/// 提示中去掉规范化带来的 `\\?\` 前缀，便于用户查找；根目录也以这种形式存库
pub(crate) fn shown(path: &Path) -> String {
    let raw = path.display().to_string();
    match raw.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!(r"\\{rest}"),
        None => raw.strip_prefix(r"\\?\").unwrap_or(&raw).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_attrs() {
        assert!(!is_placeholder_attrs(0));
        // 普通文件、隐藏、系统、只读、目录、带重解析点（OneDrive 已下载的文件也带）都不是占位文件
        assert!(!is_placeholder_attrs(0x20));
        assert!(!is_placeholder_attrs(
            FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM | 0x01 | 0x10 | 0x400
        ));
        // 已固定在本机（PINNED 0x80000）不是占位文件
        assert!(!is_placeholder_attrs(0x0008_0000));
        for a in [
            FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
            FILE_ATTRIBUTE_RECALL_ON_OPEN,
            FILE_ATTRIBUTE_OFFLINE,
        ] {
            assert!(is_placeholder_attrs(a));
            assert!(is_placeholder_attrs(a | 0x20 | 0x400));
            // 释放空间（UNPINNED 0x100000）后的占位文件
            assert!(is_placeholder_attrs(a | 0x0010_0000));
        }
        assert!(is_placeholder_attrs(
            FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS | FILE_ATTRIBUTE_RECALL_ON_OPEN
        ));
    }

    #[test]
    fn local_file_passes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        ensure_local(&file).unwrap();
        let err = ensure_local(&dir.path().join("missing.txt")).unwrap_err();
        assert!(matches!(err, FsError::Inaccessible(_)), "{err}");
    }

    #[test]
    fn temp_volume_supported() {
        let dir = tempfile::tempdir().unwrap();
        check_volume(dir.path()).unwrap();
        check_volume(&dir.path().canonicalize().unwrap()).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn unc_rejected() {
        let err = check_volume(Path::new(r"\\server\share\photos")).unwrap_err();
        assert_eq!(
            err.to_string(),
            r#"不支持索引这个位置：\\server\share\photos（网络位置），请选择本机磁盘上的文件夹。"#
        );
    }

    #[test]
    fn shown_strips_verbatim() {
        assert_eq!(shown(Path::new(r"\\?\D:\资料")), r"D:\资料");
        assert_eq!(shown(Path::new(r"\\?\UNC\nas\share")), r"\\nas\share");
        assert_eq!(shown(Path::new(r"D:\资料")), r"D:\资料");
    }
}
