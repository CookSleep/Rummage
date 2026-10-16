//! 启动时执行待迁移的数据目录更改：
//! 1. `data-dir.pending` 改名为 `data-dir.migrating`，中途意外退出时下次启动提示上次未完成，而不是重复迁移；
//! 2. 等待旧进程（含 `rummage-core.exe` 和 WebView2 的浏览器进程）退出、释放文件锁；
//! 3. 逐个复制文件（CopyFileExW，大文件也能显示进度和取消）、核对大小并写入磁盘，超过半秒未完成时显示进度框；
//! 4. 全部成功后才改写指向文件、删除原目录。任何一步失败都删除新目录中本次复制的内容，继续使用原目录。
//!
//! 调用方（`startup`）负责独占打开 `data-dir.lock`
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::time::Duration;

use crate::{
    Notice, NoticeKind, PENDING, RUNNING, SystemDirs, check_target, default_dir, normalize, notice,
    parse_pointer, platform, same_path, shown, write_pointer,
};

const CANCELLED: &str = "已取消";

/// Progress::cancel 的取值：0 进行中，1 已取消，2 已开始切换、不能再取消
pub(crate) const COMMITTED: i32 = 2;

#[derive(Default)]
pub(crate) struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    /// Windows 的 BOOL，直接交给 CopyFileExW 轮询，大文件复制到一半也能取消；取值见 COMMITTED
    pub cancel: AtomicI32,
    pub finished: AtomicBool,
}

impl Progress {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed) == 1
    }
}

pub(crate) fn cancelled() -> io::Error {
    io::Error::new(ErrorKind::Interrupted, CANCELLED)
}

/// 迁移失败：reason 为 None 表示用户取消；leftover 为新目录中未能删除的副本的说明
#[derive(Debug, PartialEq)]
pub(crate) struct Failure {
    reason: Option<String>,
    leftover: Option<String>,
}

impl Failure {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: Some(reason.into()),
            leftover: None,
        }
    }

    pub fn cancelled() -> Self {
        Self {
            reason: None,
            leftover: None,
        }
    }

    fn discarding(mut self, to: &Path, created: &[PathBuf]) -> Self {
        self.leftover = discard(to, created);
        self
    }
}

/// 递归统计文件数与总大小，不跟随链接；读取失败的条目跳过。设置页的占用统计也用它
pub fn dir_stats(dir: &Path) -> (u64, u64) {
    let (mut files, mut bytes) = (0, 0);
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
                continue;
            }
            files += 1;
            bytes += meta.len();
        }
    }
    (files, bytes)
}

/// 删除文件或文件夹；只读文件先去掉只读属性
fn remove_any(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        return fs::remove_dir_all(path);
    }
    match fs::remove_file(path) {
        Err(err) if err.kind() == ErrorKind::PermissionDenied && meta.permissions().readonly() => {
            let mut perms = meta.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            fs::set_permissions(path, perms)?;
            fs::remove_file(path)
        }
        other => other,
    }
}

/// 把文件内容写入磁盘（FlushFileBuffers 需要写权限）；只读文件临时去掉只读属性
fn sync_file(path: &Path) -> io::Result<()> {
    let err = match fs::OpenOptions::new().write(true).open(path) {
        Ok(file) => return file.sync_all(),
        Err(err) => err,
    };
    let mut perms = fs::metadata(path)?.permissions();
    if err.kind() != ErrorKind::PermissionDenied || !perms.readonly() {
        return Err(err);
    }
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(path, perms.clone())?;
    let result = fs::OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|file| file.sync_all());
    perms.set_readonly(true);
    fs::set_permissions(path, perms)?;
    result
}

/// 逐个复制文件、核对大小并写入磁盘；遇到链接、复制失败或用户取消时立即返回错误
fn copy_tree(from: &Path, to: &Path, progress: &Progress) -> io::Result<()> {
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        if progress.cancelled() {
            return Err(cancelled());
        }
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let meta = fs::symlink_metadata(&src)?;
        if meta.file_type().is_symlink() {
            return Err(io::Error::other(format!(
                "数据目录中有符号链接，无法复制：{}",
                shown(&src)
            )));
        }
        if meta.is_dir() {
            fs::create_dir(&dst)?;
            copy_tree(&src, &dst, progress)?;
            continue;
        }
        let start = progress.done.load(Ordering::Relaxed);
        platform::copy_file(&src, &dst, progress).map_err(|err| match err.kind() {
            ErrorKind::Interrupted => err,
            kind => io::Error::new(kind, format!("复制 \"{}\" 失败：{err}", shown(&src))),
        })?;
        if fs::metadata(&dst)?.len() != meta.len() {
            return Err(io::Error::other(format!(
                "\"{}\" 复制后大小不一致",
                shown(&src)
            )));
        }
        sync_file(&dst).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!("\"{}\" 写入磁盘失败：{err}", shown(&dst)),
            )
        })?;
        progress.done.store(start + meta.len(), Ordering::Relaxed);
    }
    Ok(())
}

/// 删除新目录中本次复制的内容。created 为本次新建的目录（由深到浅，第一个是新目录本身），为空表示新目录原本就有：
/// 本次新建的整个删除，上级只删空目录；原本就有的空目录只清空。删不干净时返回给用户的说明
fn discard(to: &Path, created: &[PathBuf]) -> Option<String> {
    let result = if created.is_empty() {
        fs::read_dir(to).and_then(|entries| {
            entries
                .flatten()
                .try_for_each(|entry| remove_any(&entry.path()))
        })
    } else {
        fs::remove_dir_all(to)
    };
    for dir in created.iter().skip(1) {
        let _ = fs::remove_dir(dir);
    }
    result.err().map(|err| {
        log::warn!("新目录中未复制完的文件未能删除：{err}");
        "新目录中有未复制完的文件，可以删除。".to_string()
    })
}

/// 复制并校验，返回本次新建的目录；失败时已删除本次复制的内容
fn copy_verified(
    from: &Path,
    to: &Path,
    base: &Path,
    progress: &Progress,
) -> Result<Vec<PathBuf>, Failure> {
    // 确认时已按系统目录和被索引文件夹校验过，这里只防止目录在重启前被占用或改变
    check_target(to, from, base, &SystemDirs::default(), &[]).map_err(Failure::new)?;
    let stats = dir_stats(from);
    progress.total.store(stats.1, Ordering::Relaxed);
    let created: Vec<PathBuf> = to
        .ancestors()
        .take_while(|dir| !dir.exists())
        .map(Path::to_path_buf)
        .collect();
    if let Err(err) = fs::create_dir_all(to) {
        return Err(Failure::new(format!(
            "无法创建新目录，请确认该位置可以写入后重新选择。（{err}）"
        ))
        .discarding(to, &created));
    }
    // 规范化为带 \\?\ 前缀的绝对路径，支持超过 260 个字符的长路径
    let result = from.canonicalize().and_then(|src| {
        copy_tree(&src, &to.canonicalize()?, progress)?;
        if dir_stats(to) != stats {
            return Err(io::Error::other(
                "复制结果与原目录不一致，请检查新位置所在的磁盘后重新更改。",
            ));
        }
        Ok(())
    });
    match result {
        Ok(()) => Ok(created),
        Err(err) if err.kind() == ErrorKind::Interrupted => {
            Err(Failure::cancelled().discarding(to, &created))
        }
        Err(err) => Err(Failure::new(err.to_string()).discarding(to, &created)),
    }
}

/// 复制、校验、写入指向文件（内容为确认时记下的 pointer），最后删除原目录。
/// 返回原目录未能完全删除时的说明
fn change(
    base: &Path,
    from: &Path,
    to: &Path,
    pointer: Option<&str>,
    progress: &Progress,
) -> Result<Option<String>, Failure> {
    let created = copy_verified(from, to, base, progress)?;
    // 之后不能再取消；取消标志已设置时按取消处理，保证进度框显示的状态与结果一致
    if progress
        .cancel
        .compare_exchange(0, COMMITTED, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return Err(Failure::cancelled().discarding(to, &created));
    }
    if let Err(err) = write_pointer(base, pointer) {
        let failure = Failure::new(format!("无法写入数据目录设置：{err}"));
        return Err(failure.discarding(to, &created));
    }
    Ok(fs::remove_dir_all(from).err().map(|err| err.to_string()))
}

/// 在后台线程迁移；超过半秒未完成时在当前线程显示进度框（此时还没有创建任何窗口）
fn with_progress(
    progress: &Progress,
    work: impl FnOnce() -> Result<Option<String>, Failure> + Send,
) -> Result<Option<String>, Failure> {
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let result = work();
            progress.finished.store(true, Ordering::Release);
            result
        });
        for _ in 0..10 {
            if worker.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !worker.is_finished() {
            platform::show_progress(progress);
        }
        worker.join().unwrap_or_else(|_| {
            Err(Failure::new(
                "迁移过程意外中断，请重新启动 Rummage 后重新更改数据目录。",
            ))
        })
    })
}

/// 执行待迁移的更改，返回本次使用的数据目录和结果
pub(crate) fn run_pending(base: &Path, current: &Path) -> (PathBuf, Notice) {
    let failed = |failure: Failure, to: &Path| {
        let (title, mut details) = match failure.reason {
            None => ("已取消更改数据目录", vec![]),
            Some(reason) => ("更改数据目录失败", vec![format!("原因：{reason}")]),
        };
        details.extend(failure.leftover);
        let mut rows = vec![("原目录", current)];
        if !to.as_os_str().is_empty() {
            rows.push(("新目录", to));
        }
        let notice = notice(
            NoticeKind::Warning,
            title,
            "数据仍在原目录。",
            &rows,
            details,
        );
        (current.to_path_buf(), notice)
    };
    let (pending, running) = (base.join(PENDING), base.join(RUNNING));
    // 先改名再迁移：中途程序意外退出时，下次启动不会重复迁移，而是提示上次未完成
    if let Err(err) = fs::rename(&pending, &running) {
        let _ = fs::remove_file(&pending);
        return failed(
            Failure::new(format!(
                "无法读取上次更改数据目录的记录，请重新更改数据目录。（{err}）"
            )),
            Path::new(""),
        );
    }
    let text = fs::read_to_string(&running).unwrap_or_default();
    // 用 split 而不是 lines：第 4 行为空（迁到默认目录）时也要读出来
    let mut lines = text.split('\n').map(|line| line.trim_end_matches('\r'));
    let from = PathBuf::from(lines.next().unwrap_or_default());
    let to = PathBuf::from(lines.next().unwrap_or_default());
    let pid = lines.next().and_then(|line| line.trim().parse().ok());
    let pointer = lines.next();
    // 第 4 行为空只在新目录确实是默认目录时才表示删除指向文件；否则（如 3 行标记以换行结尾）按不完整处理，
    // 不能删掉指向文件后丢失新目录
    let incomplete =
        pointer.is_none_or(|line| line.is_empty() && !same_path(&to, &default_dir(base)));
    let result = if from.as_os_str().is_empty() || to.as_os_str().is_empty() || incomplete {
        Err(Failure::new(
            "上次更改数据目录的记录不完整，请重新更改数据目录。",
        ))
    } else if normalize(&from) != normalize(current) {
        Err(Failure::new(
            "数据目录在确认更改后发生了变化，请重新更改数据目录。",
        ))
    } else {
        // 空行表示新目录是默认位置，删除指向文件
        let pointer = pointer.filter(|line| !line.is_empty());
        let progress = Progress::default();
        with_progress(&progress, || {
            platform::wait_previous(pid, &from, &progress)?;
            change(base, &from, &to, pointer, &progress)
        })
    };
    let _ = fs::remove_file(&running);
    match result {
        Ok(None) => {
            let notice = notice(
                NoticeKind::Success,
                "数据目录已更改",
                "数据目录已更改。",
                &[("新目录", &to)],
                vec![],
            );
            (to, notice)
        }
        Ok(Some(detail)) => {
            log::warn!("原数据目录未能完全删除：{detail}");
            let notice = notice(
                NoticeKind::Warning,
                "数据目录已更改",
                "原目录未能清理完成，可以手动删除。",
                &[("新目录", &to), ("原目录", &from)],
                vec![],
            );
            (to, notice)
        }
        Err(failure) => failed(failure, &to),
    }
}

/// 上次迁移中途意外退出时的提示。写指向文件是最后一步：启动时读到的指向文件原始内容与标记第 4 行完全相同，
/// 才说明已经写好；不能按路径推断，同名换盘、省略盘符时都无法区分原目录和新目录
pub(crate) fn interrupted(
    base: &Path,
    dir: &Path,
    pointer: &io::Result<Option<String>>,
    text: &str,
) -> Notice {
    let mut lines = text.split('\n').map(|line| line.trim_end_matches('\r'));
    let from = Path::new(lines.next().unwrap_or_default());
    let to = Path::new(lines.next().unwrap_or_default());
    let expected = lines.nth(1);
    let rows = [("原目录", from), ("新目录", to)];
    let title = "上次更改数据目录未完成";
    let warn = |title, message| notice(NoticeKind::Warning, title, message, &rows, vec![]);
    let (Some(expected), Ok(raw)) = (expected, pointer) else {
        return warn(
            title,
            "无法确定是否已复制完成。请比较两个目录中的文件，确认新目录完整后再删除原目录。",
        );
    };
    let written = match raw {
        // 空的第 4 行只在新目录确实是默认目录时才表示删除指向文件
        None => expected.is_empty() && same_path(to, &default_dir(base)),
        Some(line) => !expected.is_empty() && line == expected,
    };
    if !written {
        return warn(title, "数据仍在原目录，新目录可以删除。");
    }
    // 已写好：本次实际用上了指向文件记录的目录，才能提示原目录可以删除
    let recorded = match raw {
        None => default_dir(base),
        Some(line) => parse_pointer(base, line),
    };
    if same_path(dir, &recorded) {
        return warn("数据目录已更改", "已切换到新目录，原目录可以删除。");
    }
    warn(
        title,
        "数据已复制到新目录，但新目录现在无法访问。\n\n请勿删除原目录。",
    )
}

#[cfg(test)]
mod tests;
