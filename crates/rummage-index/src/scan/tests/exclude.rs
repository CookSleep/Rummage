//! 排除规则：默认排除的目录与文件、只在同级有项目文件时排除的构建输出目录、隐藏与系统文件，
//! 以及用户规则（按名称匹配任意深度、按路径相对根目录匹配、匹配到目录时排除整个目录、修改规则后重新对账）。
use super::*;

#[test]
fn default_dir_exclusions() {
    let e = env();
    for d in EXCLUDED_DIRS {
        e.put(&format!("{d}/x.txt"), b"x");
        // 大小写不同也排除
        e.put(&format!("sub/{}/x.txt", d.to_uppercase()), b"x");
    }
    e.put("ok/x.txt", b"x");
    let s = e.scan();
    assert_eq!(e.names(), ["ok/x.txt"]);
    assert_eq!(s.excluded, 2 * EXCLUDED_DIRS.len() as u64);
}

#[test]
fn build_dirs_need_project_file() {
    let e = env();
    let markers: Vec<String> = PROJECT_FILES
        .iter()
        .map(|f| f.to_string())
        .chain(["app.sln".into(), "app.csproj".into()])
        .collect();
    for (i, m) in markers.iter().enumerate() {
        e.put(&format!("p{i}/{m}"), b"x");
        e.put(&format!("p{i}/build/out.txt"), b"x");
        e.put(&format!("p{i}/dist/out.txt"), b"x");
    }
    // 没有项目文件的 build、dist 照常扫描
    e.put("plain/build/out.txt", b"x");
    e.put("plain/dist/out.txt", b"x");
    // 项目文件在上级而不在同级时不排除
    e.put("q/package.json", b"x");
    e.put("q/docs/build/out.txt", b"x");
    e.scan();
    let names = e.names();
    for (i, _) in markers.iter().enumerate() {
        assert!(!names.contains(&format!("p{i}/build/out.txt")), "p{i}");
        assert!(!names.contains(&format!("p{i}/dist/out.txt")), "p{i}");
    }
    assert!(names.contains(&"plain/build/out.txt".to_string()));
    assert!(names.contains(&"plain/dist/out.txt".to_string()));
    assert!(names.contains(&"q/docs/build/out.txt".to_string()));
}

#[test]
fn file_exclusions() {
    let e = env();
    for name in [
        "~$报告.docx",
        "a.tmp",
        "B.TMP",
        "c.part",
        "d.crdownload",
        "empty.txt",
    ] {
        e.put(name, if name == "empty.txt" { b"" } else { b"x" });
    }
    e.put("ok.txt", b"x");
    let s = e.scan();
    assert_eq!(e.names(), ["ok.txt"]);
    assert_eq!(s.excluded, 6);
}

#[cfg(windows)]
#[test]
fn hidden_and_system_excluded() {
    let e = env();
    let h = e.put("h.txt", b"x");
    let s = e.put("s.txt", b"x");
    e.put("ok.txt", b"x");
    platform::set_attributes(&h, FILE_ATTRIBUTE_HIDDEN).unwrap();
    platform::set_attributes(&s, FILE_ATTRIBUTE_SYSTEM).unwrap();
    let stats = e.scan();
    assert_eq!(e.names(), ["ok.txt"]);
    assert_eq!(stats.excluded, 2);
    // 恢复普通属性，临时目录才能删掉
    platform::set_attributes(&h, 0x80).unwrap();
    platform::set_attributes(&s, 0x80).unwrap();
}

#[test]
fn glob_name_rules_any_depth() {
    let e = env();
    // 无效规则跳过，不影响其他规则
    e.exclude(&["*.LOG", "缓存", "["]);
    e.put("a.log", b"x");
    e.put("a/b/c.log", b"x");
    e.put("x/y/缓存/z.txt", b"x");
    e.put("缓存.txt", b"x");
    e.put("a/ok.txt", b"x");
    let s = e.scan();
    assert_eq!(e.names(), ["a/ok.txt", "缓存.txt"]);
    assert_eq!(s.excluded, 3);
}

#[test]
fn glob_path_rules_anchored() {
    let e = env();
    e.exclude(&["docs/*.txt"]);
    e.put("docs/a.txt", b"x");
    e.put("docs/sub/a.txt", b"x");
    e.put("other/docs/a.txt", b"x");
    e.scan();
    assert_eq!(e.names(), ["docs/sub/a.txt", "other/docs/a.txt"]);
    // 跨层要写 **
    e.exclude(&["docs/**/*.txt"]);
    e.scan();
    assert_eq!(e.names(), ["other/docs/a.txt"]);
}

#[test]
fn glob_dir_match_excludes_subtree() {
    let e = env();
    e.exclude(&["Photos/RAW"]);
    e.put("photos/raw/2024/a.jpg", b"x");
    e.put("photos/raw.jpg", b"x");
    let s = e.scan();
    assert_eq!(e.names(), ["photos/raw.jpg"]);
    assert_eq!(s.excluded, 1);
}

#[test]
fn glob_settings_changes() {
    let e = env();
    e.put("a.txt", b"x");
    e.put("ok.txt", b"x");
    e.scan();
    // 新增规则后，已有记录按消失处理
    e.exclude(&["ok.*"]);
    let s = e.scan();
    assert_eq!(s.removed, 1);
    assert_eq!(e.names(), ["a.txt"]);
    // 设置不是合法 JSON 时忽略用户规则
    e.exec(&format!(
        "UPDATE roots SET settings_json = 'oops' WHERE id = {}",
        e.id
    ));
    e.scan();
    assert_eq!(e.names().len(), 2);
}

#[test]
fn leading_slash_and_exact_rules() {
    let e = env();
    // 以 / 开头只匹配根目录下的路径
    e.exclude(&["/a.txt"]);
    e.put("a.txt", b"x");
    e.put("sub/a.txt", b"x");
    e.scan();
    assert_eq!(e.names(), ["sub/a.txt"]);
    // 精确规则：特殊字符用 `\` 转义、按字面匹配，不误伤同名或相似的文件（`*`、`?` 不能出现在 Windows 文件名中，只检查写法）
    assert_eq!(exclude::exact_rule(r"x*y?\.txt"), r"/x\*y\?\\.txt");
    let files = [
        "报告[1].txt",
        "{a,b}.txt",
        "子/c[d].txt",
        "年度 汇总/会议 记录[终].txt",
    ];
    for f in files {
        e.put(f, b"x");
    }
    e.put("报告1.txt", b"x");
    e.put("a.txt.bak", b"x");
    e.put("其他/报告[1].txt", b"x");
    e.put("年度 汇总/会议 记录终.txt", b"x");
    let rules: Vec<String> = files.iter().map(|f| exclude::exact_rule(f)).collect();
    assert_eq!(rules[0], r"/报告\[1\].txt");
    assert_eq!(rules[1], r"/\{a,b\}.txt");
    assert_eq!(rules[2], r"子/c\[d\].txt");
    // 保存时作分隔符的 `\` 换成 `/`，转义保留，往返不变
    for r in &rules {
        assert_eq!(&exclude::slashes(r), r);
    }
    assert_eq!(exclude::slashes(r"年度 汇总\会议 记录\[终\].txt"), rules[3]);
    assert_eq!(exclude::slashes(r"a\b\\c\"), r"a/b\\c/");
    e.exclude(&rules.iter().map(String::as_str).collect::<Vec<_>>());
    e.scan();
    let mut names = e.names();
    names.sort();
    assert_eq!(
        names,
        [
            "a.txt",
            "a.txt.bak",
            "sub/a.txt",
            "其他/报告[1].txt",
            "年度 汇总/会议 记录终.txt",
            "报告1.txt"
        ]
    );
}
