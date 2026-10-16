//! 非 Windows 平台的占位实现，只用于编译和测试：没有网盘占位属性，所有位置都视为本地固定盘上的受支持卷；
//! 没有文件 ID，改名与移动靠 `quick_hash` 识别。
use std::fs::{File, Metadata};
use std::io;
use std::path::Path;

pub const DRIVE_REMOVABLE: u32 = 2;
pub const DRIVE_FIXED: u32 = 3;
pub const DRIVE_REMOTE: u32 = 4;

pub fn attrs(_meta: &Metadata) -> u32 {
    0
}

/// 只给出目录属性（0x10），供添加根目录时判断
pub fn file_attributes(path: &Path) -> io::Result<u32> {
    std::fs::metadata(path).map(|m| if m.is_dir() { 0x10 } else { 0 })
}

pub fn volume(_path: &Path) -> io::Result<(u32, String)> {
    Ok((DRIVE_FIXED, "NTFS".into()))
}

pub fn file_id(_file: &File) -> Option<String> {
    None
}
