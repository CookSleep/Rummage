//! 迁移数据目录的测试：复制与切换、取消与失败、中断后的提示、指针文件
use super::*;
use crate::tests::{base_with_data, none};
use crate::{LOCK, POINTER, pending_text, pointer_text, resolve, startup};

fn all_text(notice: &Notice) -> String {
    format!(
        "{} {} {}",
        notice.title,
        notice.message,
        notice.details.join(" ")
    )
}

#[test]
fn change_copies_switches_and_removes_old() {
    let (root, data) = base_with_data();
    let base = root.path();
    let outside = tempfile::tempdir().unwrap();
    let new = outside.path().join("a").join("新数据");
    let progress = Progress::default();
    let text = pointer_text(base, &new, false);
    assert_eq!(
        change(base, &data, &new, text.as_deref(), &progress),
        Ok(None)
    );
    assert!(!data.exists());
    assert_eq!(fs::read(new.join("index.db")).unwrap(), b"db");
    assert_eq!(
        fs::read(new.join("thumbs").join("ab").join("x.webp")).unwrap(),
        b"img"
    );
    assert!(new.join("webview").is_dir());
    assert_eq!(progress.done.load(Ordering::Relaxed), 5);
    assert_eq!(progress.cancel.load(Ordering::Relaxed), COMMITTED);
    assert_eq!(resolve(base, &none()).unwrap(), new);
    // 改回默认位置时删除指向文件
    assert_eq!(
        change(base, &new, &data, None, &Progress::default()),
        Ok(None)
    );
    assert!(!base.join(POINTER).exists());
    assert!(!new.exists());
    assert_eq!(resolve(base, &none()).unwrap(), data);
    assert_eq!(fs::read(data.join("index.db")).unwrap(), b"db");
}

/// 只读文件也能写入磁盘，复制后仍为只读
#[test]
fn syncs_readonly_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ro.bin");
    fs::write(&path, b"ro").unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_readonly(true);
    fs::set_permissions(&path, perms).unwrap();
    sync_file(&path).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    assert!(perms.readonly());
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(&path, perms).unwrap();
    assert!(sync_file(&dir.path().join("missing")).is_err());
}

#[test]
fn change_rejects_non_empty_target() {
    let (root, data) = base_with_data();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("keep.txt"), b"keep").unwrap();
    let err = change(
        root.path(),
        &data,
        outside.path(),
        Some("x"),
        &Progress::default(),
    )
    .unwrap_err();
    assert_eq!(err, Failure::new(crate::NOT_EMPTY));
    assert_eq!(fs::read(outside.path().join("keep.txt")).unwrap(), b"keep");
    assert!(data.join("index.db").exists());
    assert!(!root.path().join(POINTER).exists());
}

#[test]
fn cancel_discards_copy_and_keeps_old() {
    let (root, data) = base_with_data();
    let outside = tempfile::tempdir().unwrap();
    // 已存在的空目录只清空，不删除
    let progress = Progress::default();
    progress.cancel.store(1, Ordering::Relaxed);
    let err = change(root.path(), &data, outside.path(), Some("x"), &progress).unwrap_err();
    assert_eq!(err, Failure::cancelled());
    assert!(outside.path().is_dir());
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    assert!(data.join("thumbs").join("ab").join("x.webp").exists());
    assert!(!root.path().join(POINTER).exists());
    // 本次新建的多层目录全部删除，原本就有的上级保留
    let progress = Progress::default();
    progress.cancel.store(1, Ordering::Relaxed);
    let deep = outside.path().join("a").join("b").join("新数据");
    assert_eq!(
        change(root.path(), &data, &deep, Some("x"), &progress).unwrap_err(),
        Failure::cancelled()
    );
    assert!(!outside.path().join("a").exists());
    assert!(outside.path().is_dir());
}

/// 某个文件被独占打开无法读取：删除已复制的内容，原目录和指向文件保持不动
#[cfg(windows)]
#[test]
fn copy_failure_keeps_old_data() {
    use std::os::windows::fs::OpenOptionsExt;

    let (root, data) = base_with_data();
    let old = root.path().join("old");
    fs::create_dir_all(&old).unwrap();
    fs::write(old.join("index.db"), b"old").unwrap();
    fs::write(old.join("z.bin"), b"locked").unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(old.join("z.bin"))
        .unwrap();
    let outside = tempfile::tempdir().unwrap();
    let new = outside.path().join("新数据");
    let err = change(root.path(), &old, &new, Some("x"), &Progress::default())
        .unwrap_err()
        .reason
        .unwrap();
    drop(lock);
    assert!(err.contains("z.bin"), "{err}");
    assert!(!err.contains(r"\\?\"), "{err}");
    assert!(!new.exists());
    assert_eq!(fs::read(old.join("index.db")).unwrap(), b"old");
    assert_eq!(fs::read(old.join("z.bin")).unwrap(), b"locked");
    assert!(!root.path().join(POINTER).exists());
    assert!(data.exists());
}

/// 等待旧进程时取消、旧进程不存在时直接继续
#[cfg(windows)]
#[test]
fn wait_previous_honors_cancel() {
    use std::os::windows::fs::OpenOptionsExt;

    let dir = tempfile::tempdir().unwrap();
    let progress = Progress::default();
    assert_eq!(platform::wait_previous(None, dir.path(), &progress), Ok(()));
    // 当前进程不会退出：取消后立即返回
    progress.cancel.store(1, Ordering::Relaxed);
    assert_eq!(
        platform::wait_previous(Some(std::process::id()), dir.path(), &progress),
        Err(Failure::cancelled())
    );
    // lockfile 被占用且已取消
    let lockfile = dir.path().join("webview").join("EBWebView");
    fs::create_dir_all(&lockfile).unwrap();
    let held = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .share_mode(0)
        .open(lockfile.join("lockfile"))
        .unwrap();
    assert_eq!(
        platform::wait_previous(None, dir.path(), &progress),
        Err(Failure::cancelled())
    );
    drop(held);
    assert_eq!(
        platform::wait_previous(None, dir.path(), &Progress::default()),
        Ok(())
    );
}

/// 上次迁移中途意外退出（migrating 标记残留）：按启动时指向文件的原始内容判断是否已完成，不重复迁移，标记清理
#[test]
fn startup_reports_interrupted_migration() {
    let (root, data) = base_with_data();
    let base = root.path();
    let outside = tempfile::tempdir().unwrap();
    let new = outside.path().join("新数据");
    let running = pending_text(base, &data, &new);

    // 指向文件还没写：副本可以删除
    fs::write(base.join(RUNNING), &running).unwrap();
    let (dir, notices) = startup(base, &none(), None);
    assert_eq!(dir.unwrap(), data);
    assert_eq!(notices.len(), 1);
    assert!(all_text(&notices[0]).contains("新目录可以删除"));
    assert!(!base.join(RUNNING).exists());
    assert!(!new.exists());

    // 指向文件已写好、新目录可用：迁移已完成
    fs::create_dir_all(&new).unwrap();
    write_pointer(base, pointer_text(base, &new, false).as_deref()).unwrap();
    fs::write(base.join(RUNNING), &running).unwrap();
    let (dir, notices) = startup(base, &none(), None);
    assert_eq!(dir.unwrap(), new);
    assert!(all_text(&notices[0]).contains("已切换到新目录"));
    assert!(data.join("index.db").exists());
    assert!(!base.join(RUNNING).exists() && !base.join(LOCK).exists());

    // 指向文件已写好但新目录无法访问：返回错误，标记保留到能确定时再处理
    fs::remove_dir(&new).unwrap();
    fs::write(base.join(RUNNING), &running).unwrap();
    let (dir, notices) = startup(base, &none(), None);
    assert!(matches!(dir, Err(crate::Error::Missing(missing)) if missing == new));
    assert!(notices.is_empty());
    assert!(base.join(RUNNING).exists());

    // 用户选择改用默认目录后，仍按启动时指向文件的原始内容判断为已完成，不能提示删除
    let (dir, notices) = startup(base, &none(), Some(crate::Fallback::UseDefault));
    assert_eq!(dir.unwrap(), data);
    assert_eq!(notices.len(), 2);
    assert_eq!(notices[0].title, "已改用默认数据目录");
    let msg = all_text(&notices[1]);
    assert!(
        msg.contains("无法访问") && !msg.contains("可以删除"),
        "{msg}"
    );
    assert!(!base.join(POINTER).exists() && !base.join(RUNNING).exists());
}

fn interrupted_text(base: &Path, dir: &str, pointer: Option<&str>, marker: &str) -> String {
    let pointer = Ok(pointer.map(str::to_string));
    all_text(&interrupted(base, Path::new(dir), &pointer, marker))
}

/// 是否已写好只看指向文件原文与标记第 4 行是否完全相同
#[test]
fn interrupted_compares_pointer_text() {
    // 同名换盘，原指向文件记录完整路径，复制中途崩溃
    let base = Path::new(r"C:\Apps\Rummage");
    let marker = "D:\\Rummage 数据\nE:\\Rummage 数据\n1\nE:\\Rummage 数据";
    let msg = interrupted_text(base, r"D:\Rummage 数据", Some(r"D:\Rummage 数据"), marker);
    assert!(
        msg.contains("数据仍在原目录") && !msg.contains("已切换到新目录"),
        "{msg}"
    );
    let msg = interrupted_text(base, r"E:\Rummage 数据", Some(r"E:\Rummage 数据"), marker);
    assert!(msg.contains("已切换到新目录"), "{msg}");

    // U 盘：迁移时是 E:，指向文件省略盘符；完成后再启动时 U 盘变成 F:
    let marker = "E:\\old\nE:\\Rummage 数据\n1\n\\Rummage 数据";
    let base = Path::new(r"F:\Apps\Rummage");
    let msg = interrupted_text(base, r"F:\Rummage 数据", Some(r"\Rummage 数据"), marker);
    assert!(msg.contains("已切换到新目录"), "{msg}");
    // 已写好但这次没用上新目录：不能提示删除原目录
    let msg = interrupted_text(
        base,
        r"F:\Apps\Rummage\data",
        Some(r"\Rummage 数据"),
        marker,
    );
    assert!(
        msg.contains("无法访问") && !msg.contains("可以删除"),
        "{msg}"
    );

    // 迁到默认目录：完成后指向文件已删除；未完成时指向文件仍是原目录
    let default = default_dir(base).display().to_string();
    let marker = "F:\\old\n".to_string() + &default + "\n1\n";
    let msg = interrupted_text(base, &default, None, &marker);
    assert!(msg.contains("已切换到新目录"), "{msg}");
    let msg = interrupted_text(base, r"F:\old", Some(r"\old"), &marker);
    assert!(msg.contains("数据仍在原目录"), "{msg}");

    // 标记不完整或指向文件读取失败：无法确定
    let msg = interrupted_text(base, r"F:\old", Some(r"\old"), "F:\\old\nE:\\new\n1");
    assert!(msg.contains("无法确定"), "{msg}");
    let failed: io::Result<Option<String>> = Err(io::Error::other("x"));
    let notice = interrupted(base, Path::new(r"F:\old"), &failed, &marker);
    assert!(all_text(&notice).contains("无法确定"));
}

/// 启动时执行待迁移的更改：成功后切换；原目录与标记不符或标记不完整时不迁移；标记文件都会清理
#[test]
fn run_pending_switches_or_keeps() {
    let (root, data) = base_with_data();
    let base = root.path();
    let outside = tempfile::tempdir().unwrap();
    let new = outside.path().join("新数据");
    let other = outside.path().join("other");
    for marker in [
        pending_text(base, &other, &new),
        format!("{}\n{}\n0", data.display(), new.display()),
    ] {
        fs::write(base.join(crate::PENDING), marker).unwrap();
        let (dir, notice) = run_pending(base, &data);
        assert_eq!(dir, data);
        assert_eq!(notice.kind, NoticeKind::Warning);
        assert!(all_text(&notice).contains("数据仍在原目录"));
        assert_eq!(notice.paths.len(), 2);
        assert!(!new.exists());
        assert!(!base.join(crate::PENDING).exists() && !base.join(RUNNING).exists());
    }

    // 经 startup 完整走一遍：独占锁、迁移、切换、清理。进程号 0 不存在，不必等待
    let marker = pending_text(base, &data, &new);
    let marker = marker.replace(&format!("\n{}\n", std::process::id()), "\n0\n");
    fs::write(base.join(crate::PENDING), marker).unwrap();
    let (dir, notices) = startup(base, &none(), None);
    assert_eq!(dir.unwrap(), new);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].kind, NoticeKind::Success);
    assert!(!data.exists());
    assert_eq!(fs::read(new.join("index.db")).unwrap(), b"db");
    assert_eq!(resolve(base, &none()).unwrap(), new);
    assert!(!base.join(crate::PENDING).exists() && !base.join(RUNNING).exists());
    assert!(!base.join(LOCK).exists());
}

/// 3 行标记以换行结尾，读出空的第 4 行：新目录不是默认目录时不能当作删除指向文件，按不完整处理、不迁移；
/// 中断判断时指向文件不存在也不能判为已写好
#[test]
fn empty_pointer_line_requires_default_target() {
    let (root, data) = base_with_data();
    let base = root.path();
    let outside = tempfile::tempdir().unwrap();
    let new = outside.path().join("新数据");
    let marker = format!("{}\n{}\n0\n", data.display(), new.display());
    fs::write(base.join(crate::PENDING), &marker).unwrap();
    let (dir, notice) = run_pending(base, &data);
    assert_eq!(dir, data);
    assert!(all_text(&notice).contains("记录不完整"));
    assert!(!new.exists());
    assert!(data.join("index.db").exists());
    assert!(!base.join(POINTER).exists());
    let pointer: io::Result<Option<String>> = Ok(None);
    let notice = interrupted(base, &data, &pointer, &marker);
    assert!(all_text(&notice).contains("数据仍在原目录"));
}

/// 复制完成后写指向文件失败：删除副本，原目录和指向文件保持不动
#[test]
fn pointer_write_failure_discards_copy() {
    let (root, data) = base_with_data();
    let base = root.path();
    // 指向文件的位置被同名文件夹占着，改名覆盖时失败
    fs::create_dir_all(base.join(POINTER).join("keep")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let new = outside.path().join("新数据");
    let err = change(base, &data, &new, Some("x"), &Progress::default())
        .unwrap_err()
        .reason
        .unwrap();
    assert!(err.contains("无法写入数据目录设置"), "{err}");
    assert!(!new.exists());
    assert_eq!(fs::read(data.join("index.db")).unwrap(), b"db");
    assert!(base.join(POINTER).join("keep").is_dir());
}

/// 原目录中的文件不允许删除：已切换到新目录，提示原目录未能清理完成
#[cfg(windows)]
#[test]
fn partial_removal_reports_leftover() {
    use std::os::windows::fs::OpenOptionsExt;

    let (root, data) = base_with_data();
    let base = root.path();
    // FILE_SHARE_READ：允许复制时读取，不允许删除
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(data.join("index.db"))
        .unwrap();
    let outside = tempfile::tempdir().unwrap();
    let new = outside.path().join("新数据");
    let text = pointer_text(base, &new, false);
    let result = change(base, &data, &new, text.as_deref(), &Progress::default());
    drop(held);
    assert!(matches!(result, Ok(Some(_))), "{result:?}");
    assert_eq!(fs::read(new.join("index.db")).unwrap(), b"db");
    assert_eq!(resolve(base, &none()).unwrap(), new);
    assert!(data.join("index.db").exists());
}

/// 锁文件无法创建：不迁移，待迁移标记删除，继续使用原目录
#[test]
fn lock_failure_keeps_old_dir() {
    let (root, data) = base_with_data();
    let base = root.path();
    fs::create_dir_all(base.join(LOCK)).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let new = outside.path().join("新数据");
    fs::write(base.join(crate::PENDING), pending_text(base, &data, &new)).unwrap();
    let (dir, notices) = startup(base, &none(), None);
    assert_eq!(dir.unwrap(), data);
    assert_eq!(notices.len(), 1);
    assert!(all_text(&notices[0]).contains("无法写入 Rummage 的安装目录"));
    assert!(!base.join(crate::PENDING).exists());
    assert!(!new.exists());
}
