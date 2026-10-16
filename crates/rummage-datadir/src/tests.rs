//! 数据目录的测试：指针文件、启动时的回退、目标位置规则、原子写入；也提供迁移测试共用的辅助函数
use super::*;

/// 临时的安装目录，默认数据目录中带几个文件
pub(crate) fn base_with_data() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let data = default_dir(root.path());
    fs::create_dir_all(data.join("thumbs").join("ab")).unwrap();
    fs::create_dir_all(data.join("webview")).unwrap();
    fs::write(data.join("index.db"), b"db").unwrap();
    fs::write(data.join("thumbs").join("ab").join("x.webp"), b"img").unwrap();
    (root, data)
}

pub(crate) fn none() -> SystemDirs {
    SystemDirs::default()
}

#[test]
fn locate_does_not_migrate() {
    let (root, data) = base_with_data();
    let base = root.path();
    assert_eq!(locate_in(base, &none()).unwrap(), data);
    let missing = base.join("missing");
    fs::write(base.join(POINTER), missing.display().to_string()).unwrap();
    assert!(matches!(locate_in(base, &none()), Err(Error::Missing(dir)) if dir == missing));
    fs::remove_file(base.join(POINTER)).unwrap();
    for marker in [PENDING, RUNNING] {
        fs::write(base.join(marker), "x").unwrap();
        assert!(matches!(locate_in(base, &none()), Err(Error::Changing)));
        // 标记原样保留，留给界面启动时处理
        assert!(base.join(marker).exists());
        fs::remove_file(base.join(marker)).unwrap();
    }
}

#[test]
fn resolves_pointer() {
    let (root, data) = base_with_data();
    let base = root.path();
    assert_eq!(resolve(base, &none()).unwrap(), data);
    let other = tempfile::tempdir().unwrap();
    let other = other.path().join("其他 位置");
    fs::create_dir_all(&other).unwrap();
    fs::write(
        base.join(POINTER),
        format!("\u{feff}{}\r\n", other.display()),
    )
    .unwrap();
    assert_eq!(resolve(base, &none()).unwrap(), other);
    // 指向的目录不存在
    let missing = other.join("missing");
    fs::write(base.join(POINTER), missing.display().to_string()).unwrap();
    assert!(matches!(resolve(base, &none()), Err(Error::Missing(dir)) if dir == missing));
    // 损坏（相对路径、空文件、乱码、非 UTF-8）
    for text in ["relative\\dir", "", "\u{fffd}\u{fffd}"] {
        fs::write(base.join(POINTER), text).unwrap();
        assert!(
            matches!(resolve(base, &none()), Err(Error::Invalid { .. })),
            "{text}"
        );
    }
    fs::write(base.join(POINTER), [0xff, 0xfe, 0x00]).unwrap();
    assert!(matches!(resolve(base, &none()), Err(Error::Invalid { .. })));
    // 指向安装目录中的其他位置、或云同步文件夹：不合规
    let inside = base.join("inside");
    fs::create_dir_all(&inside).unwrap();
    fs::write(base.join(POINTER), inside.display().to_string()).unwrap();
    assert!(
        matches!(resolve(base, &none()), Err(Error::Invalid { reason, .. }) if reason.contains(IN_INSTALL))
    );
    fs::write(base.join(POINTER), other.display().to_string()).unwrap();
    let cloud = SystemDirs {
        cloud: vec![normalize(other.parent().unwrap())],
        ..SystemDirs::default()
    };
    assert!(
        matches!(resolve(base, &cloud), Err(Error::Invalid { reason, .. }) if reason.contains(CLOUD))
    );
}

/// 指向的目录不存在：不带 fallback 时返回错误且不改动；改用默认目录删除指向文件；
/// 无法询问时本次使用默认目录、指向文件保持不动
#[test]
fn startup_falls_back_when_missing() {
    let (root, data) = base_with_data();
    let base = root.path();
    let outside = tempfile::tempdir().unwrap();
    let missing = outside.path().join("移动硬盘");
    let text = missing.display().to_string();
    fs::write(base.join(POINTER), &text).unwrap();

    let (dir, notices) = startup(base, &none(), None);
    assert!(matches!(dir, Err(Error::Missing(dir)) if dir == missing));
    assert!(notices.is_empty());
    assert_eq!(fs::read_to_string(base.join(POINTER)).unwrap(), text);

    let (dir, notices) = startup(base, &none(), Some(Fallback::Skip));
    assert_eq!(dir.unwrap(), data);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].title, "本次使用默认数据目录");
    assert!(notices[0].message.contains("重新连接存储设备后"));
    assert_eq!(fs::read_to_string(base.join(POINTER)).unwrap(), text);

    let (dir, notices) = startup(base, &none(), Some(Fallback::UseDefault));
    assert_eq!(dir.unwrap(), data);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].title, "已改用默认数据目录");
    assert_eq!(notices[0].paths[0], ("原数据目录".into(), text.clone()));
    assert!(!base.join(POINTER).exists());
    // 之后正常启动即使用默认目录
    assert_eq!(startup(base, &none(), None).0.unwrap(), data);

    // 确认期间设备重新连上：仍按用户的选择删除指向文件，改用默认目录
    fs::create_dir_all(&missing).unwrap();
    fs::write(base.join(POINTER), &text).unwrap();
    let (dir, notices) = startup(base, &none(), Some(Fallback::UseDefault));
    assert_eq!(dir.unwrap(), data);
    assert_eq!(notices[0].title, "已改用默认数据目录");
    assert!(!base.join(POINTER).exists());
}

/// 指向文件无法读取时没有原数据目录可显示，提示只列默认目录
#[test]
fn startup_hides_empty_recorded_dir() {
    let (root, data) = base_with_data();
    let base = root.path();
    fs::create_dir_all(base.join(POINTER)).unwrap();
    assert!(matches!(
        startup(base, &none(), None).0,
        Err(Error::Invalid { .. })
    ));
    let (dir, notices) = startup(base, &none(), Some(Fallback::Skip));
    assert_eq!(dir.unwrap(), data);
    assert_eq!(notices[0].paths.len(), 1);
    assert_eq!(notices[0].paths[0].0, "默认目录");
    assert_eq!(notices[0].message, "原数据目录设置无效，本次使用默认目录。");
}

/// 与安装目录同盘时指向文件省略盘符，读取时补上安装目录的盘符
#[cfg(windows)]
#[test]
fn pointer_omits_drive_on_same_disk() {
    let base = Path::new(r"E:\Apps\Rummage");
    // 可移动设备：与安装目录同盘时省略盘符
    assert_eq!(
        pointer_text(base, Path::new(r"E:\Rummage 数据"), true).as_deref(),
        Some(r"\Rummage 数据")
    );
    assert_eq!(
        pointer_text(base, Path::new(r"D:\Rummage 数据"), true).as_deref(),
        Some(r"D:\Rummage 数据")
    );
    // 普通安装：一律写完整路径
    assert_eq!(
        pointer_text(base, Path::new(r"E:\Rummage 数据"), false).as_deref(),
        Some(r"E:\Rummage 数据")
    );
    assert_eq!(pointer_text(base, &default_dir(base), true), None);
    assert_eq!(pointer_text(base, &default_dir(base), false), None);
    assert_eq!(
        parse_pointer(Path::new(r"F:\Apps\Rummage"), r"\Rummage 数据"),
        PathBuf::from(r"F:\Rummage 数据")
    );
    assert_eq!(
        parse_pointer(base, r"D:\Rummage 数据"),
        PathBuf::from(r"D:\Rummage 数据")
    );
    assert_eq!(
        parse_pointer(base, r"\\server\share"),
        PathBuf::from(r"\\server\share")
    );
    // 实际读写一遍（临时目录与测试用的安装目录在同一个盘）
    let (root, _) = base_with_data();
    let new = root.path().with_extension("同盘");
    fs::create_dir_all(&new).unwrap();
    let text = pointer_text(root.path(), &new, true);
    write_pointer(root.path(), text.as_deref()).unwrap();
    let written = fs::read_to_string(root.path().join(POINTER)).unwrap();
    assert!(
        written.starts_with('\\') && !written.starts_with(r"\\"),
        "{written}"
    );
    assert_eq!(resolve(root.path(), &none()).unwrap(), new);
    // 改回默认位置时删除指向文件
    write_pointer(root.path(), None).unwrap();
    assert!(!root.path().join(POINTER).exists());
    fs::remove_dir(&new).unwrap();
}

/// 不存在的盘符查询失败，按不可移动处理；已知文件夹能查到
#[cfg(windows)]
#[test]
fn missing_drive_is_not_removable() {
    if !Path::new("Q:\\").exists() {
        assert!(!platform::is_removable_drive(b'Q'));
    }
    assert!(!platform::known_dirs().is_empty());
}

#[cfg(windows)]
#[test]
fn rejects_network_locations() {
    assert!(is_network(Path::new(r"\\server\share\rummage")));
    assert!(is_network(Path::new(r"\\?\UNC\server\share\rummage")));
    assert!(!is_network(Path::new(r"C:\rummage")));
    let (root, data) = base_with_data();
    assert_eq!(
        check_target(
            Path::new(r"\\server\share\rummage"),
            &data,
            root.path(),
            &none(),
            &[]
        )
        .unwrap_err(),
        NETWORK
    );
    fs::write(root.path().join(POINTER), r"\\server\share\rummage").unwrap();
    assert!(matches!(
        resolve(root.path(), &none()),
        Err(Error::Invalid { .. })
    ));
}

#[test]
fn checks_target_rules() {
    let (root, data) = base_with_data();
    let base = root.path();
    let outside = tempfile::tempdir().unwrap();
    let new = outside.path().join("新数据");
    let check = |to: &Path, current: &Path| check_target(to, current, base, &none(), &[]);
    assert_eq!(check(&new, &data), Ok(()));
    fs::create_dir_all(&new).unwrap();
    assert_eq!(check(&new, &data), Ok(()));
    fs::write(new.join("a.txt"), b"a").unwrap();
    assert_eq!(check(&new, &data).unwrap_err(), NOT_EMPTY);
    assert_eq!(check(&new.join("a.txt"), &data).unwrap_err(), NOT_DIR);
    assert_eq!(check(Path::new("relative"), &data).unwrap_err(), NOT_DIR);
    assert_eq!(check(&data, &data).unwrap_err(), SAME);
    for nested in [data.join("thumbs"), data.join("new")] {
        assert_eq!(check(&nested, &data).unwrap_err(), NESTED);
    }
    // 安装目录及其中除默认数据目录外的位置
    let other = outside.path().join("cur");
    fs::create_dir_all(&other).unwrap();
    assert_eq!(check(base, &other).unwrap_err(), IN_INSTALL);
    assert_eq!(check(&base.join("other"), &other).unwrap_err(), IN_INSTALL);
    // 已有的被索引文件夹之内（含其本身）不行，旁边可以
    let indexed = [outside.path().join("资料")];
    let inside = indexed[0].join("数据");
    assert_eq!(
        check_target(&inside, &data, base, &none(), &indexed).unwrap_err(),
        INDEXED
    );
    assert_eq!(
        check_target(&indexed[0], &data, base, &none(), &indexed).unwrap_err(),
        INDEXED
    );
    assert_eq!(
        check_target(
            &outside.path().join("资料2"),
            &data,
            base,
            &none(),
            &indexed
        ),
        Ok(())
    );
    // 当前在其他位置时可以改回默认位置（为空或不存在）
    fs::remove_dir_all(&data).unwrap();
    assert_eq!(check(&data, &other), Ok(()));
}

#[cfg(windows)]
#[test]
fn blocks_system_locations() {
    let dirs = SystemDirs {
        protected: vec![
            PathBuf::from(r"C:\Users\me"),
            PathBuf::from(r"C:\Users\me\AppData\Roaming"),
            PathBuf::from(r"C:\Users\me\AppData\Local"),
        ],
        known: vec![
            PathBuf::from(r"C:\Users\me\Pictures"),
            PathBuf::from(r"D:\Shell\图片"),
        ],
        system: vec![
            PathBuf::from(r"C:\Windows"),
            PathBuf::from(r"C:\Program Files"),
        ],
        cloud: vec![PathBuf::from(r"C:\Users\me\OneDrive")],
    };
    let current = Path::new(r"C:\Users\me\AppData\Local\Rummage\data");
    let base = Path::new(r"C:\Users\me\AppData\Local\Rummage");
    let cases = [
        (r"C:\", Some(ROOT)),
        (r"\\?\D:\", Some(ROOT)),
        (r"\\?\UNC\server\share\x", Some(NETWORK)),
        (r"C:\Users", Some(PROTECTED)),
        (r"C:\Users\me", Some(PROTECTED)),
        (r"C:\Users\me\AppData", Some(PROTECTED)),
        (r"C:\Users\me\AppData\Local", Some(PROTECTED)),
        (r"C:\Users\me\Pictures", Some(KNOWN)),
        (r"D:\Shell", Some(KNOWN)),
        (r"C:\Windows\x", Some(SYSTEM)),
        (r"C:\Program Files\Rummage", Some(SYSTEM)),
        (r"C:\Users\me\OneDrive", Some(CLOUD)),
        (r"C:\Users\me\OneDrive\Rummage", Some(CLOUD)),
        (r"C:\Users\me\AppData\Local\Rummage\data", Some(SAME)),
        (
            r"C:\Users\me\AppData\Local\Rummage\data\thumbs",
            Some(NESTED),
        ),
        (r"C:\Users\me\AppData\Local\Rummage\other", Some(IN_INSTALL)),
        (r"C:\Users\me\Pictures\Rummage 数据", None),
        (r"C:\Users\me\AppData\Local\其他", None),
        (r"D:\Rummage 数据", None),
    ];
    for (to, expected) in cases {
        assert_eq!(
            target_block(Path::new(to), current, base, &dirs),
            expected,
            "{to}"
        );
    }
}

/// 写指向文件：落盘后改名，不留临时文件
#[test]
fn writes_atomically() {
    let dir = tempfile::tempdir().unwrap();
    write_atomic(&dir.path().join("p"), "abc").unwrap();
    assert_eq!(fs::read_to_string(dir.path().join("p")).unwrap(), "abc");
    assert!(!dir.path().join("p.tmp").exists());
}
