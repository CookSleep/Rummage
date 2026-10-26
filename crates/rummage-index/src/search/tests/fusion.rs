//! 分数校准与融合：文档的内嵌媒体并入文档列表、按校准分数融合与匹配度门槛、以图搜图与找相似的
//! 所用的分布、找相似的用已存的向量。
use super::*;

fn calibrate(f: &Fixture, kind: QueryKind, space: &str, modality: &str, mean: f64, std: f64) {
    f.db.lock()
        .unwrap()
        .conn
        .execute(
            "INSERT OR REPLACE INTO calib (kind, space_id, modality, source, mean, std, vectors, updated_at)
             VALUES (?1, ?2, ?3, '', ?4, ?5, 1, 0)",
            params![kind.as_str(), space, modality, mean, std],
        )
        .unwrap();
}

fn lows(hits: &[Hit]) -> Vec<bool> {
    hits.iter().map(|h| h.low).collect()
}

/// 文档的内嵌媒体并入文档列表：文档按正文与内嵌片段中较高的 z 只占一个名次，
/// 不进图片列表；还没校准时内嵌片段照旧留在图片列表中；以图搜图时内嵌图片照旧与独立图片一起排名
#[test]
fn embedded_media_merge_into_documents() {
    let f = Fixture::new();
    let r = f.root("D:\\资料");
    let t = text_space();
    let img = space("i", true);
    let chart = f.file(r, "图表.pdf", "document", 0);
    f.segment(chart, "text", &along(0.6), &t);
    f.segment(chart, "image", &along(0.4), &img);
    let plain = f.file(r, "正文.pdf", "document", 0);
    f.segment(plain, "text", &along(0.7), &t);
    // 只有内嵌图片有向量（扫描件）
    let scan = f.file(r, "插图.pdf", "document", 0);
    f.segment(scan, "image", &along(0.35), &img);
    let photo = f.file(r, "p.jpg", "image", 0);
    f.segment(photo, "image", &along(0.25), &img);
    let index = f.index();
    let (q, none) = (unit(0), Filter::default());
    let query = semantic(&q, &t, &none);

    // 没有背景分布：内嵌图片照旧与独立图片按余弦排名（图表的内嵌图 0.4、插图 0.35、p.jpg 0.25），
    // 扫描件也能搜到；图表在正文与图片两个列表中都得分
    let hits = f.search(&index, &query);
    assert_eq!(
        file_names(&hits),
        ["图表.pdf", "正文.pdf", "插图.pdf", "p.jpg"]
    );
    assert!((hits[0].score - (1.0 / 62.0 + 1.0 / 61.0)).abs() < 1e-12);

    // 只有正文有分布：图片还没校准，结果不变
    calibrate(&f, QueryKind::Text, "t", "text", 0.5, 0.05);
    assert_eq!(
        file_names(&f.search(&index, &query)),
        ["图表.pdf", "正文.pdf", "插图.pdf", "p.jpg"]
    );

    // 正文 z：图表 2、正文 4；图片 z：图表的内嵌图 6、插图 5、p.jpg 3
    calibrate(&f, QueryKind::Text, "i", "image", 0.1, 0.05);
    let hits = f.search(&index, &query);
    assert_eq!(
        file_names(&hits),
        ["图表.pdf", "p.jpg", "插图.pdf", "正文.pdf"]
    );
    // 图表只在文档列表中得分一次
    assert!((hits[0].score - 1.0 / 61.0).abs() < 1e-12);
    assert!((hits[0].z.unwrap() - 6.0).abs() < 1e-2);
    assert_eq!(hits[0].segments.len(), 2);

    // 以图搜图：内嵌图片与独立图片一起排名
    let hits = f.search(&index, &semantic(&q, &img, &none));
    assert_eq!(file_names(&hits), ["图表.pdf", "插图.pdf", "p.jpg"]);
}

/// 文档的余弦普遍高于图片：各模态按余弦单独排名、交替排列，有没有背景分布都一样；有分布后 z 低于门槛的折叠到后面
#[test]
fn calibrated_fusion_and_threshold() {
    let f = Fixture::new();
    let r = f.root("D:\\资料");
    let t = text_space();
    let img = space("i", true);
    for (name, cos) in [("d1.md", 0.8), ("d2.md", 0.65)] {
        let id = f.file(r, name, "document", 0);
        f.segment(id, "text", &along(cos), &t);
    }
    for (name, cos) in [("p1.jpg", 0.3), ("p2.jpg", 0.15)] {
        let id = f.file(r, name, "image", 0);
        f.segment(id, "image", &along(cos), &img);
    }
    f.file(r, "notes.pdf", "document", 0);
    let index = f.index();
    let (q, none) = (unit(0), Filter::default());
    let mut query = semantic(&q, &t, &none);
    query.params.min_relevance = 3.5;

    // 还没有背景分布：同一名次余弦高的在前；没有 z，不折叠
    let hits = f.search(&index, &query);
    assert_eq!(file_names(&hits), ["d1.md", "p1.jpg", "d2.md", "p2.jpg"]);
    assert!(hits.iter().all(|h| h.z.is_none() && !h.low));

    // 只有图片有分布（z 为 4、1）：排序不变，文档不判定门槛；同分时仍是余弦高的在前
    calibrate(&f, QueryKind::Text, "i", "image", 0.1, 0.05);
    // 其他查询类型的分布不参与
    calibrate(&f, QueryKind::Similar, "t", "text", 0.0, 1.0);
    let hits = f.search(&index, &query);
    assert_eq!(file_names(&hits), ["d1.md", "p1.jpg", "d2.md", "p2.jpg"]);
    assert_eq!(lows(&hits), [false, false, false, true]);
    assert!(hits[0].z.is_none());
    assert!((hits[1].z.unwrap() - 4.0).abs() < 1e-3, "{:?}", hits[1].z);

    // 都有分布（文档 z 为 6、3）：仍按模态交替，z 只决定折叠
    calibrate(&f, QueryKind::Text, "t", "text", 0.5, 0.05);
    let hits = f.search(&index, &query);
    assert_eq!(file_names(&hits), ["d1.md", "p1.jpg", "d2.md", "p2.jpg"]);
    assert_eq!(lows(&hits), [false, false, true, true]);
    assert!((hits[2].z.unwrap() - 3.0).abs() < 2e-2);
    assert!((hits[0].score - 1.0 / 61.0).abs() < 1e-12);
    assert!((hits[1].score - 1.0 / 61.0).abs() < 1e-12);

    // 关键词命中的不折叠；未达门槛的排在其余结果之后，即使融合分数更高
    query.text = Some("notes");
    query.params.weight_name = 0.5;
    query.params.min_relevance = 5.0;
    let hits = f.search(&index, &query);
    assert_eq!(
        file_names(&hits),
        ["d1.md", "notes.pdf", "p1.jpg", "d2.md", "p2.jpg"]
    );
    assert_eq!(lows(&hits), [false, false, true, true, true]);
    assert!(hits[1].score < hits[2].score);
    query.text = Some("p2.jpg");
    let hits = f.search(&index, &query);
    let p2 = hits.iter().find(|h| h.name == "p2.jpg").unwrap();
    assert!(p2.name_hit && !p2.low);
    // 先折叠再截取：结果数量上限不会挤掉达标的结果
    query.text = Some("notes");
    query.params.limit = 2;
    assert_eq!(
        file_names(&f.search(&index, &query)),
        ["d1.md", "notes.pdf"]
    );
    // 全部未达门槛时都折叠，顺序不变
    query.text = None;
    query.params.limit = 100;
    query.params.min_relevance = 10.0;
    let hits = f.search(&index, &query);
    assert_eq!(file_names(&hits), ["d1.md", "p1.jpg", "d2.md", "p2.jpg"]);
    assert!(hits.iter().all(|h| h.low));
}

/// 以图搜图用 `similar` 的分布；同一文件的片段按 z 排序
#[test]
fn image_query_uses_similar_stats() {
    let f = Fixture::new();
    let r = f.root("D:\\图");
    let img = space("i", true);
    let a = f.file(r, "a.jpg", "image", 0);
    f.segment(a, "image", &along(0.9), &img);
    let index = f.index();
    let (q, none) = (unit(0), Filter::default());
    calibrate(&f, QueryKind::Text, "i", "image", 0.0, 1.0);
    let hits = f.search(&index, &semantic(&q, &img, &none));
    assert!(hits[0].z.is_none());
    calibrate(&f, QueryKind::Similar, "i", "image", 0.5, 0.1);
    let hits = f.search(&index, &semantic(&q, &img, &none));
    assert!((hits[0].z.unwrap() - 4.0).abs() < 1e-3);
    assert!(!hits[0].low);
    // 门槛只用于文字查询：z 远低于门槛也不折叠，匹配度照常给出
    let mut query = semantic(&q, &img, &none);
    query.params.min_relevance = 10.0;
    let hits = f.search(&index, &query);
    assert!(!hits[0].low && hits[0].z.is_some());
    query.kind = QueryKind::Text;
    calibrate(&f, QueryKind::Text, "i", "image", 0.5, 0.1);
    assert!(f.search(&index, &query)[0].low);

    // 长图的切片在另一档位的空间中：余弦较低、z 较高的切片排在前面
    let t = text_space();
    let slice = Space {
        id: "i560".into(),
        preprocess: "image-v1-t560".into(),
        ..img.clone()
    };
    let long = f.file(r, "long.png", "image", 0);
    let whole = f.segment(long, "image", &along(0.5), &img);
    let part = f.segment(long, "image", &along(0.4), &slice);
    let index = f.index();
    calibrate(&f, QueryKind::Text, "i", "image", 0.3, 0.1);
    calibrate(&f, QueryKind::Text, "i560", "image", 0.1, 0.05);
    let hits = f.search(&index, &semantic(&q, &t, &none));
    let h = hits.iter().find(|h| h.name == "long.png").unwrap();
    let order: Vec<i64> = h.segments.iter().map(|s| s.segment_id).collect();
    assert_eq!(order, [part, whole]);
    assert!((h.z.unwrap() - 6.0).abs() < 2e-2);
    assert!((h.cosine.unwrap() - 0.5).abs() < 1e-3);
}

/// 找相似的按本次查询自身的分数分布换算 z：不用背景分布，不随筛选变化，不含被拿来查的文件
#[test]
fn similar_z_from_own_scores() {
    let f = Fixture::new();
    let a = f.root(r"D:\甲");
    let b = f.root(r"D:\乙");
    let img = space("i", true);
    let src = f.file(a, "src.jpg", "image", 0);
    f.segment(src, "image", &along(1.0), &img);
    let cos: Vec<f32> = (0..crate::calib::MIN_VECTORS)
        .map(|i| 0.2 + 0.03 * i as f32)
        .collect();
    for (i, c) in cos.iter().enumerate() {
        let file = f.file(
            if i % 2 == 0 { a } else { b },
            &format!("{i}.jpg"),
            "image",
            0,
        );
        f.segment(file, "image", &along(*c), &img);
    }
    let index = f.index();
    calibrate(&f, QueryKind::Similar, "i", "image", 0.0, 1.0);
    let n = cos.len() as f32;
    let mean = cos.iter().sum::<f32>() / n;
    let std = (cos.iter().map(|c| (c - mean) * (c - mean)).sum::<f32>() / n).sqrt();
    let q = along(1.0);
    let mut filter = Filter {
        exclude_file: Some(src),
        ..Filter::default()
    };
    let z_of = |filter: &Filter, name: &str| {
        let hits = f.search(&index, &semantic(&q, &img, filter));
        hits.iter().find(|h| h.name == name).unwrap().z.unwrap()
    };
    let want = (cos[19] - mean) / std;
    assert!((z_of(&filter, "19.jpg") - want).abs() < 0.05);
    filter.folders = vec![(b, String::new())];
    assert!((z_of(&filter, "19.jpg") - want).abs() < 0.05);
}

/// 找相似的：用已存的向量，只比较同一模态，被拿来查的文件不出现；过滤条件照常生效
#[test]
fn similar_uses_stored_vector() {
    let f = Fixture::new();
    let a = f.root(r"D:\甲");
    let b = f.root(r"D:\乙");
    let t = text_space();
    let img = space("i", true);
    let src = f.file(a, "src.jpg", "image", 0);
    let seg = f.segment(src, "image", &along(1.0), &img);
    let near = f.file(a, "near.jpg", "image", 0);
    f.segment(near, "image", &along(0.9), &img);
    let far = f.file(b, "far.jpg", "image", 0);
    f.segment(far, "image", &along(0.5), &img);
    // 文本模态中的向量即使同一方向也不参与
    let doc = f.file(a, "doc.md", "document", 0);
    f.segment(doc, "text", &along(1.0), &t);
    let index = f.index();

    let (v, s, m) = stored_vector(&f.db.lock().unwrap(), src, Some(seg))
        .unwrap()
        .unwrap();
    assert_eq!((s.id.as_str(), m.as_str()), ("i", "image"));
    assert!((v[0] - 1.0).abs() < 1e-3);
    // 不指定片段时取第一个有向量的片段；片段不属于该文件、文件没有向量时为空
    assert!(
        stored_vector(&f.db.lock().unwrap(), src, None)
            .unwrap()
            .is_some()
    );
    assert!(
        stored_vector(&f.db.lock().unwrap(), near, Some(seg))
            .unwrap()
            .is_none()
    );
    let bare = f.file(a, "bare.txt", "other", 0);
    assert!(
        stored_vector(&f.db.lock().unwrap(), bare, None)
            .unwrap()
            .is_none()
    );

    let run = |filter: &Filter| {
        let query = Query {
            vector: Some((&v, &s)),
            coarse: &v,
            kind: QueryKind::Similar,
            modality: Some(&m),
            text: None,
            filter,
            params: PARAMS,
            cancel: None,
        };
        file_names(&f.search(&index, &query))
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
    };
    let mut filter = Filter {
        exclude_file: Some(src),
        ..Filter::default()
    };
    assert_eq!(run(&filter), ["near.jpg", "far.jpg"]);
    filter.folders = vec![(b, String::new())];
    assert_eq!(run(&filter), ["far.jpg"]);
    // 不排除时自己排第一
    assert_eq!(run(&Filter::default())[0], "src.jpg");
}

/// 长图切片与整图档位不同：以切片找相似改用整图向量，能找到普通图片；档位相同时仍用切片本身
#[test]
fn similar_from_slice_uses_whole_image() {
    let f = Fixture::new();
    let a = f.root(r"D:\甲");
    let whole = Space {
        preprocess: "image-v1-t280".into(),
        ..space("i280", true)
    };
    let slice = space("i140", true);
    let long = f.file(a, "long.png", "image", 0);
    f.segment(long, "image", &along(1.0), &whole);
    let seg = f.segment(long, "image", &unit(5), &slice);
    let region = |id: i64| {
        f.db.lock()
            .unwrap()
            .conn
            .execute(
                r#"UPDATE segments SET locator_json = '{"region":{"x":0,"y":0,"width":10,"height":10}}' WHERE id = ?1"#,
                [id],
            )
            .unwrap();
    };
    region(seg);
    let other = f.file(a, "other.png", "image", 0);
    f.segment(other, "image", &unit(5), &slice);
    let photo = f.file(a, "photo.jpg", "image", 0);
    f.segment(photo, "image", &along(0.9), &whole);
    let index = f.index();

    let (v, s, m) = stored_vector(&f.db.lock().unwrap(), long, Some(seg))
        .unwrap()
        .unwrap();
    assert_eq!((s.id.as_str(), m.as_str()), ("i280", "image"));
    let filter = Filter {
        exclude_file: Some(long),
        ..Filter::default()
    };
    let query = Query {
        vector: Some((&v, &s)),
        coarse: &v,
        kind: QueryKind::Similar,
        modality: Some(&m),
        text: None,
        filter: &filter,
        params: PARAMS,
        cancel: None,
    };
    assert_eq!(file_names(&f.search(&index, &query)), ["photo.jpg"]);

    // 整图与切片同一档位
    let same = f.file(a, "same.png", "image", 0);
    f.segment(same, "image", &along(1.0), &slice);
    let seg = f.segment(same, "image", &unit(5), &slice);
    region(seg);
    let (v, s, _) = stored_vector(&f.db.lock().unwrap(), same, Some(seg))
        .unwrap()
        .unwrap();
    assert_eq!(s.id, "i140");
    assert!((v[5] - 1.0).abs() < 1e-3);
}
