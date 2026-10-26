//! 文件名与正文通道：短文件名用 LIKE、FTS 特殊字符不出错、正文通道的命中与排序。
use super::*;

#[test]
fn short_name_uses_like() {
    let f = Fixture::new();
    let r = f.root("D:\\资料");
    f.file(r, "ab_c.txt", "document", 1);
    f.file(r, "abxc.txt", "document", 2);
    f.file(r, "柯基.jpg", "image", 3);
    f.file(r, "50%折扣.png", "image", 4);
    let index = f.index();
    let none = Filter::default();
    let run = |q: &str| {
        f.search(&index, &by_name(q, &none))
            .iter()
            .map(|h| h.name.clone())
            .collect::<Vec<_>>()
    };
    // 不足 3 个字时 trigram 匹配不到任何东西，靠 LIKE；% 和 _ 按字面匹配
    assert_eq!(run("b_"), ["ab_c.txt"]);
    assert_eq!(run("柯基"), ["柯基.jpg"]);
    assert_eq!(run("0%"), ["50%折扣.png"]);
    assert_eq!(run("AB"), ["abxc.txt", "ab_c.txt"]);
    // 3 个字及以上走 FTS
    assert_eq!(run("b_c"), ["ab_c.txt"]);
    assert_eq!(run("柯基.j"), ["柯基.jpg"]);
    assert!(run("xyz").is_empty());
    // 多个词全部命中才算：都不少于 3 个字时走 FTS，否则整体退回 LIKE
    assert_eq!(run("txt abx"), ["abxc.txt"]);
    assert_eq!(run("txt ab"), ["abxc.txt", "ab_c.txt"]);
    assert!(run("txt 柯基").is_empty());
    // 过滤条件同样生效
    let images = Filter {
        kinds: vec!["image".into()],
        ..Filter::default()
    };
    let hits = f.search(&index, &by_name("txt", &images));
    assert!(hits.is_empty());
    let hits = f.search(&index, &by_name("a", &images));
    assert!(hits.is_empty());
    // 只匹配文件名，不匹配目录
    f.db.lock()
        .unwrap()
        .conn
        .execute(
            "UPDATE files SET rel_path = 'zzz\\' || name WHERE name = 'abxc.txt'",
            [],
        )
        .unwrap();
    assert!(run("zzz").is_empty());
}

#[test]
fn fts_special_chars_safe() {
    let f = Fixture::new();
    let r = f.root("D:\\资料");
    f.file(r, "it's a (draft) v1.0 - final*.txt", "document", 0);
    let index = f.index();
    let none = Filter::default();
    for q in [
        "\"",
        "\"\"\"",
        "a\" OR \"b",
        "* AND NOT",
        "NEAR(a b)",
        "name:",
        "rel_path : x",
        "^abc",
        "a-b-c",
        "{name}",
        "\\",
        "%_%",
        "'; DROP TABLE files; --",
    ] {
        f.search(&index, &by_name(q, &none));
    }
    for q in ["(draft)", "it's", "final*", "v1.0 - "] {
        assert_eq!(f.search(&index, &by_name(q, &none)).len(), 1, "{q}");
    }
}

#[test]
fn text_channel() {
    let f = Fixture::new();
    let a = f.root("D:\\甲");
    let b = f.root("D:\\乙");
    let report = f.file(a, "report.docx", "document", 100);
    let s1 = f.text(report, "本季度销售额同比增长百分之十二");
    let s2 = f.text(report, "销售额增长主要来自华东地区");
    f.text(report, "附录：数据来源");
    let memo = f.file(b, "memo.pdf", "document", 200);
    let s3 = f.text(memo, "会议讨论了销售额的统计口径");
    let pic = f.file(b, "pic.jpg", "image", 300);
    f.segment(pic, "image", &unit(0), &space("i", true));
    let index = f.index();
    let none = Filter::default();
    let run = |q: &str, filter: &Filter| f.search(&index, &by_name(q, filter));

    // 中文短语走 FTS：同一文件的多个片段合并，全部命中片段按匹配度记下
    let hits = run("销售额", &none);
    assert_eq!(hits.len(), 2);
    let r = hits.iter().find(|h| h.name == "report.docx").unwrap();
    let mut segs = r.text_segments.clone();
    segs.sort();
    assert_eq!(segs, [s1, s2]);
    assert!(!r.name_hit && r.cosine.is_none() && r.segments.is_empty());
    let m = hits.iter().find(|h| h.name == "memo.pdf").unwrap();
    assert_eq!(m.text_segments, [s3]);
    // 短语按整体匹配，不拆成词
    assert!(run("增长销售额", &none).is_empty());
    assert_eq!(file_names(&run("华东地区", &none)), ["report.docx"]);
    // 不足 3 个字时退回 LIKE，按修改时间从新到旧
    assert_eq!(file_names(&run("口径", &none)), ["memo.pdf"]);
    assert_eq!(file_names(&run("增长", &none)), ["report.docx"]);
    // 只有 1 个字时不走正文通道
    assert!(run("额", &none).is_empty());
    // 多个词各自匹配，同一片段全部含有才算，与顺序无关
    assert_eq!(file_names(&run("华东地区 销售额", &none)), ["report.docx"]);
    assert_eq!(file_names(&run("销售额  增长主要", &none)), ["report.docx"]);
    assert!(run("销售额 统计口径 华东地区", &none).is_empty());
    // 有词不足 3 个字时整体退回 LIKE
    assert_eq!(file_names(&run("华东 增长", &none)), ["report.docx"]);
    assert_eq!(file_names(&run("销售额 口径", &none)), ["memo.pdf"]);
    assert!(run("口径 华东", &none).is_empty());
    // 文件名与正文都命中时两路相加
    let both = f.file(a, "销售额汇总.xlsx", "document", 0);
    f.text(both, "各月销售额");
    let hits = run("销售额", &none);
    assert_eq!(hits[0].name, "销售额汇总.xlsx");
    assert!(hits[0].name_hit && !hits[0].text_segments.is_empty());
    // 过滤条件同样作用于正文通道
    let roots = Filter {
        folders: vec![(b, String::new())],
        ..Filter::default()
    };
    assert_eq!(file_names(&run("销售额", &roots)), ["memo.pdf"]);
    assert_eq!(file_names(&run("口径", &roots)), ["memo.pdf"]);
    assert!(run("增长", &roots).is_empty());
    let images = Filter {
        kinds: vec!["image".into()],
        ..Filter::default()
    };
    assert!(run("销售额", &images).is_empty());
    let time = Filter {
        since: Some(50),
        until: Some(150),
        ..Filter::default()
    };
    assert_eq!(file_names(&run("销售额", &time)), ["report.docx"]);
    // 权重为 0 时正文通道不参与
    let mut q = by_name("口径", &none);
    q.params.weight_text = 0.0;
    assert!(f.search(&index, &q).is_empty());
    // 以图搜图没有文字查询，不走正文通道
    let img = space("i", true);
    let v = unit(0);
    let hits = f.search(&index, &semantic(&v, &img, &none));
    assert_eq!(file_names(&hits), ["pic.jpg"]);
    assert!(hits[0].text_segments.is_empty());
    // 级联删除后搜不到
    f.db.lock()
        .unwrap()
        .conn
        .execute("DELETE FROM files WHERE id = ?1", [memo])
        .unwrap();
    assert!(run("口径", &none).is_empty());
    f.db.lock()
        .unwrap()
        .conn
        .execute("DELETE FROM roots WHERE id = ?1", [a])
        .unwrap();
    assert!(run("销售额", &none).is_empty());
    assert!(run("增长", &none).is_empty());
}

#[test]
fn text_channel_special_chars_safe() {
    let f = Fixture::new();
    let r = f.root("D:\\资料");
    let id = f.file(r, "a.md", "document", 0);
    f.text(id, "it's a (draft) v1.0 - final* \"引号\" 50%_off \\路径");
    let index = f.index();
    let none = Filter::default();
    for q in [
        "\"",
        "\"\"\"",
        "a\" OR \"b",
        "* AND NOT",
        "NEAR(a b)",
        "text:",
        "text : x",
        "^abc",
        "{text}",
        "\\",
        "%_%",
        "'; DROP TABLE segments; --",
    ] {
        f.search(&index, &by_name(q, &none));
    }
    for q in [
        "(draft)",
        "it's",
        "final*",
        "v1.0 - ",
        "\"引号\"",
        "50%_",
        "%_",
        "\\路",
        "_o",
        "(draft) final* \"引号\"",
    ] {
        let hits = f.search(&index, &by_name(q, &none));
        assert_eq!(hits.len(), 1, "{q}");
        assert!(!hits[0].text_segments.is_empty(), "{q}");
    }
    // % 与 _ 不当作通配符
    assert!(f.search(&index, &by_name("v%", &none)).is_empty());
    assert!(f.search(&index, &by_name("a_", &none)).is_empty());
}

#[test]
fn like_takes_newest_files_and_stops() {
    let f = Fixture::new();
    let r = f.root("D:\\资料");
    // 取修改时间最新的文件，同一修改时间的按文件 id 从大到小；同一文件的多个命中片段都记下
    let mut files = Vec::new();
    for i in 0..(KEYWORD_LIMIT as i64 + 5) {
        let id = f.file(r, &format!("{i}.txt"), "document", i / 2);
        f.text(id, "季度报告");
        f.text(id, "无关");
        f.text(id, "报告附录");
        files.push(id);
    }
    let index = f.index();
    let none = Filter::default();
    let mut q = by_name("报告", &none);
    q.params.limit = 300;
    let hits = f.search(&index, &q);
    let mut ids: Vec<i64> = hits.iter().map(|h| h.file_id).collect();
    ids.sort_by(|a, b| b.cmp(a));
    let newest: Vec<i64> = files.iter().rev().take(KEYWORD_LIMIT).copied().collect();
    assert_eq!(ids, newest);
    assert!(hits.iter().all(|h| h.text_segments.len() == 2));
}

#[test]
fn superseded_search_is_cancelled() {
    let f = Fixture::new();
    let r = f.root("D:\\资料");
    let id = f.file(r, "报告.docx", "document", 1);
    f.text(id, "季度报告");
    let index = f.index();
    let none = Filter::default();
    let latest = Arc::new(AtomicU64::new(1));
    let query = |ticket| Query {
        cancel: Some(Cancel {
            latest: latest.clone(),
            ticket,
        }),
        ..by_name("报告", &none)
    };
    assert_eq!(f.search(&index, &query(1)).len(), 1);
    assert!(matches!(
        search(&f.db, &index, &query(0)),
        Err(DbError::Cancelled)
    ));
    // 中止后进度回调已卸下，不带 cancel 的检索照常
    latest.store(5, AtomicOrdering::Relaxed);
    assert_eq!(f.search(&index, &by_name("报告", &none)).len(), 1);
}

/// 短词的 LIKE 沿修改时间索引逐个文件读取，不另外排序：否则要先找出全部命中，取够即停就没有意义。
/// 按根目录、种类、时间过滤时也一样
#[test]
fn like_reads_in_mtime_order() {
    let f = Fixture::new();
    let db = f.db.lock().unwrap();
    let filters = [
        Filter::default(),
        Filter {
            kinds: vec!["image".into()],
            folders: vec![(1, String::new()), (2, "照片".into())],
            since: Some(0),
            until: Some(9),
            exclude_file: Some(3),
        },
    ];
    for filter in &filters {
        for words in [vec!["报告"], vec!["季度报告", "ab"]] {
            let (names, name_args) = names_sql(&words, filter);
            let (texts, text_args, fts) = texts_sql(&words, filter);
            assert!(!fts);
            for (sql, args) in [(names, name_args), (texts, text_args)] {
                let plan: Vec<String> = db
                    .conn
                    .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                    .unwrap()
                    .query_map(params_from_iter(args), |row| row.get(3))
                    .unwrap()
                    .collect::<Result<_, _>>()
                    .unwrap();
                assert!(plan.iter().all(|p| !p.contains("TEMP B-TREE")), "{plan:?}");
            }
        }
    }
}
