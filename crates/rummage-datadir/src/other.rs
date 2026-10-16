//! 非 Windows 平台的占位实现，只用于编译和测试：没有网络驱动器、可移动设备和已知文件夹的判断，
//! 复制不能中途取消，不等待旧进程，不显示进度框和询问对话框
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use crate::migrate::{Failure, Progress};

pub fn is_remote_drive(_letter: u8) -> bool {
    false
}

pub fn is_removable_drive(_letter: u8) -> bool {
    false
}

pub fn known_dirs() -> Vec<PathBuf> {
    Vec::new()
}

/// 复制单个文件，目标已存在时报错；保留修改时间与权限
pub fn copy_file(src: &Path, dst: &Path, progress: &Progress) -> io::Result<()> {
    let meta = fs::metadata(src)?;
    let mut reader = fs::File::open(src)?;
    let mut writer = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)?;
    let copied = io::copy(&mut reader, &mut writer)?;
    writer.set_times(fs::FileTimes::new().set_modified(meta.modified()?))?;
    writer.set_permissions(meta.permissions())?;
    progress.done.fetch_add(copied, Ordering::Relaxed);
    Ok(())
}

pub fn wait_previous(_pid: Option<u32>, _from: &Path, _progress: &Progress) -> Result<(), Failure> {
    Ok(())
}

/// 没有独占打开，总能拿到锁
pub fn lock(path: &Path) -> io::Result<Option<fs::File>> {
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map(Some)
}

pub fn show_progress(_progress: &Progress) {}

pub fn can_choose() -> bool {
    false
}

pub fn choose(_heading: &str, _content: &str, _footer: &str, _buttons: &[&str]) -> Option<usize> {
    None
}
