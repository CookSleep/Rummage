//! Windows 平台接口：文件属性与卷信息，文件 ID。
//! 属性与卷信息只用 `GetFileAttributesW`、`GetVolumePathNameW`、`GetDriveTypeW`、`GetVolumeInformationW`，
//! 都不打开文件，不会触发网盘占位文件的下载；`file_id` 用调用方已打开（已过 `fsx::ensure_local`）的句柄。
use std::fs::{File, Metadata};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Component, Path, Prefix};

use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, GetDriveTypeW, GetFileAttributesW, GetFileInformationByHandle,
    GetVolumeInformationByHandleW, GetVolumeInformationW, GetVolumePathNameW,
    INVALID_FILE_ATTRIBUTES,
};

/// `GetDriveTypeW` 的返回值
pub const DRIVE_REMOVABLE: u32 = 2;
pub const DRIVE_FIXED: u32 = 3;
pub const DRIVE_REMOTE: u32 = 4;

/// 以 0 结尾的 UTF-16 路径。超过 MAX_PATH 的盘符路径加 `\\?\` 前缀，否则 API 会失败
fn wide(path: &Path) -> Vec<u16> {
    let long = path.as_os_str().len() >= 260
        && matches!(path.components().next(), Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_)));
    let prefix: &[u16] = if long {
        &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16]
    } else {
        &[]
    };
    prefix
        .iter()
        .copied()
        .chain(path.as_os_str().encode_wide())
        .chain([0])
        .collect()
}

/// 目录枚举得到的属性（`DirEntry::metadata` 取自 `FindNextFileW` 的数据，不打开文件）
pub fn attrs(meta: &Metadata) -> u32 {
    meta.file_attributes()
}

/// `.nomedia` 是 Android 的约定
pub fn no_media(_dir: &Path) -> bool {
    false
}

pub fn file_attributes(path: &Path) -> io::Result<u32> {
    let name = wide(path);
    // SAFETY: name 是以 0 结尾的 UTF-16 字符串，调用期间一直有效
    let attrs = unsafe { GetFileAttributesW(name.as_ptr()) };
    if attrs == INVALID_FILE_ATTRIBUTES {
        return Err(io::Error::last_os_error());
    }
    Ok(attrs)
}

/// 路径所在卷的驱动器类型与文件系统名称（如 `NTFS`）
pub fn volume(path: &Path) -> io::Result<(u32, String)> {
    let name = wide(path);
    let mut root = [0u16; 1024];
    // SAFETY: name 以 0 结尾；root 的长度与传入的字符数一致
    if unsafe { GetVolumePathNameW(name.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetVolumePathNameW 成功时 root 是以 0 结尾的字符串
    let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
    let mut fs_name = [0u16; 64];
    // SAFETY: root 以 0 结尾；不需要的输出参数传空指针；fs_name 的长度与传入的字符数一致
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let len = fs_name
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(fs_name.len());
    Ok((kind, String::from_utf16_lossy(&fs_name[..len])))
}

/// 卷序列号 + 文件 ID，如 `1a2b3c4d-0001000000004f2e`。只在 NTFS 上给出：FAT32 / exFAT 的文件 ID 随目录项位置变化、
/// 可能被新文件重用，ReFS 的 64 位 ID 不保证唯一，这些卷上改名与移动靠 `quick_hash` 识别
pub fn file_id(file: &File) -> Option<String> {
    let handle = file.as_raw_handle();
    let mut fs_name = [0u16; 64];
    // SAFETY: handle 在 file 存活期间有效；不需要的输出参数传空指针；fs_name 的长度与传入的字符数一致
    let ok = unsafe {
        GetVolumeInformationByHandleW(
            handle,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    let len = fs_name
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(fs_name.len());
    if ok == 0 || String::from_utf16_lossy(&fs_name[..len]) != "NTFS" {
        return None;
    }
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: handle 有效；info 是可写的结构体，调用期间一直有效
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return None;
    }
    let id = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Some(format!("{:08x}-{id:016x}", info.dwVolumeSerialNumber))
}

/// 测试用：设置隐藏、系统等属性
#[cfg(test)]
pub fn set_attributes(path: &Path, attrs: u32) -> io::Result<()> {
    let name = wide(path);
    // SAFETY: name 是以 0 结尾的 UTF-16 字符串，调用期间一直有效
    if unsafe { windows_sys::Win32::Storage::FileSystem::SetFileAttributesW(name.as_ptr(), attrs) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
