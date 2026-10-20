//! 搜索的测试：过滤、按文件聚合、融合与排序、文件名与全文通道、长图切片与文档内嵌媒体、校准与匹配度门槛、以图搜图
use super::*;
use crate::vindex::quantize;
use crate::{db, vec_to_blob};

mod fusion;
mod text;

pub(crate) struct Fixture {
    pub db: Mutex<Db>,
    _dir: tempfile::TempDir,
}

impl Fixture {
    pub fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        Fixture {
            db: Mutex::new(Db::open(dir.path()).unwrap()),
            _dir: dir,
        }
    }

    pub fn root(&self, path: &str) -> i64 {
        let db = self.db.lock().unwrap();
        db.conn
            .execute(
                "INSERT INTO roots (path, settings_json, settings_version, added_at)
                 VALUES (?1, '{}', 1, 0)",
                [path],
            )
            .unwrap();
        db.conn.last_insert_rowid()
    }

    pub fn file(&self, root: i64, name: &str, kind: &str, mtime: i64) -> i64 {
        let db = self.db.lock().unwrap();
        db.conn
            .execute(
                "INSERT INTO files (root_id, rel_path, name, ext, kind, size, mtime, status, updated_at)
                 VALUES (?1, ?2, ?2, '', ?3, 1, ?4, 'stored', 0)",
                params![root, name, kind, mtime],
            )
            .unwrap();
        db.conn.last_insert_rowid()
    }

    pub fn segment(&self, file: i64, modality: &str, v: &[f32], space: &Space) -> i64 {
        let db = self.db.lock().unwrap();
        db::insert_space(&db.conn, space).unwrap();
        db.conn
            .execute(
                "INSERT INTO segments (file_id, modality, locator_json, created_at)
                 VALUES (?1, ?2, '{\"line\":1}', 0)",
                params![file, modality],
            )
            .unwrap();
        let id = db.conn.last_insert_rowid();
        db.conn
            .execute(
                "INSERT INTO vectors (segment_id, space_id, dim, vec, params_version, coarse)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id,
                    space.id,
                    v.len() as i64,
                    vec_to_blob(v),
                    space.preprocess,
                    quantize(v)
                ],
            )
            .unwrap();
        id
    }

    /// 只有正文、没有向量的片段
    pub fn text(&self, file: i64, text: &str) -> i64 {
        let db = self.db.lock().unwrap();
        db.conn
            .execute(
                "INSERT INTO segments (file_id, modality, locator_json, text, created_at)
                 VALUES (?1, 'text', '{}', ?2, 0)",
                params![file, text],
            )
            .unwrap();
        db.conn.last_insert_rowid()
    }

    pub fn search(&self, index: &Mutex<VIndex>, q: &Query) -> Vec<Hit> {
        search(&self.db, index, q).unwrap()
    }

    pub fn index(&self) -> Mutex<VIndex> {
        Mutex::new(VIndex::load(&self.db.lock().unwrap()).unwrap())
    }
}

pub(crate) fn text_space() -> Space {
    Space {
        id: "t".into(),
        family: "embeddinggemma-2".into(),
        model_sha256: "m".into(),
        mmproj_sha256: None,
        runtime: "llama.cpp b11461".into(),
        backend: "cpu".into(),
        precision: "q8_0".into(),
        prefix_scheme: "A".into(),
        preprocess: "text-v1".into(),
    }
}

/// `image` 为真时是 140 档的图像空间
pub(crate) fn space(id: &str, image: bool) -> Space {
    let mut s = Space {
        id: id.into(),
        ..text_space()
    };
    if image {
        s.mmproj_sha256 = Some("mm".into());
        s.preprocess = "image-v1-t140".into();
    }
    s
}

/// 第 `i` 维为 1 的 768 维向量
pub(crate) fn unit(i: usize) -> Vec<f32> {
    let mut v = vec![0f32; 768];
    v[i] = 1.0;
    v
}

/// 与 `unit(0)` 的余弦为 `cos` 的单位向量
fn along(cos: f32) -> Vec<f32> {
    let mut v = unit(0);
    v[0] = cos;
    v[1] = (1.0 - cos * cos).sqrt();
    v
}

const PARAMS: SearchParams = SearchParams {
    candidates: 200,
    limit: 100,
    rrf_k: 60.0,
    weight_semantic: 1.0,
    weight_name: 1.0,
    weight_text: 1.0,
    video_windows: 5,
    video_gap_ms: 500,
    min_relevance: 2.0,
};

fn semantic<'a>(v: &'a [f32], space: &'a Space, filter: &'a Filter) -> Query<'a> {
    Query {
        vector: Some((v, space)),
        coarse: v,
        kind: if space.mmproj_sha256.is_some() {
            QueryKind::Similar
        } else {
            QueryKind::Text
        },
        modality: None,
        text: None,
        filter,
        params: PARAMS,
        cancel: None,
    }
}

fn by_name<'a>(name: &'a str, filter: &'a Filter) -> Query<'a> {
    Query {
        vector: None,
        coarse: &[],
        kind: QueryKind::Text,
        modality: None,
        text: Some(name),
        filter,
        params: PARAMS,
        cancel: None,
    }
}

fn file_names(hits: &[Hit]) -> Vec<&str> {
    hits.iter().map(|h| h.name.as_str()).collect()
}

#[test]
fn comparable_rules() {
    let t = text_space();
    let img = space("i", true);
    assert!(comparable(&t, &t));
    // 后端不参与
    let vulkan = Space {
        backend: "vulkan".into(),
        ..t.clone()
    };
    assert!(comparable(&t, &vulkan));
    assert!(comparable(
        &t,
        &Space {
            backend: "vulkan".into(),
            ..img.clone()
        }
    ));
    // 文本查询可比较任意档位、任意 mmproj 的图像空间
    assert!(comparable(&t, &img));
    assert!(comparable(
        &t,
        &Space {
            preprocess: "image-v1-t280".into(),
            mmproj_sha256: Some("other".into()),
            ..img.clone()
        }
    ));
    assert!(!comparable(
        &t,
        &Space {
            preprocess: "text-v2".into(),
            ..t.clone()
        }
    ));
    let changed: [fn(&mut Space); 5] = [
        |s| s.family = "x".into(),
        |s| s.model_sha256 = "x".into(),
        |s| s.runtime = "x".into(),
        |s| s.precision = "bf16".into(),
        |s| s.prefix_scheme = "B".into(),
    ];
    for f in changed {
        for target in [&t, &img] {
            let mut s = target.clone();
            f(&mut s);
            assert!(!comparable(&t, &s), "{s:?}");
            assert!(!comparable(&img, &s), "{s:?}");
        }
    }
    // 以图搜图：只比较同 mmproj、同档位的图像空间
    assert!(comparable(&img, &img));
    assert!(comparable(
        &img,
        &Space {
            backend: "vulkan".into(),
            ..img.clone()
        }
    ));
    assert!(!comparable(&img, &t));
    assert!(!comparable(
        &img,
        &Space {
            preprocess: "image-v1-t280".into(),
            ..img.clone()
        }
    ));
    assert!(!comparable(
        &img,
        &Space {
            mmproj_sha256: Some("other".into()),
            ..img.clone()
        }
    ));
}

#[test]
fn filters_apply_before_coarse() {
    let f = Fixture::new();
    let a = f.root("D:\\甲");
    let b = f.root("D:\\乙");
    let t = text_space();
    // 根目录甲中有大量更接近的片段，只取 1 个候选时仍能找到乙中的那个
    for i in 0..20 {
        let id = f.file(a, &format!("a{i}.txt"), "document", 1000);
        f.segment(id, "text", &along(0.9), &t);
    }
    let low = f.file(b, "low.txt", "document", 50);
    f.segment(low, "text", &along(0.1), &t);
    let pic = f.file(b, "pic.jpg", "image", 2000);
    f.segment(pic, "image", &along(0.05), &space("i", true));
    // 不可比较的空间中的片段不参与
    let other = f.file(b, "other.txt", "document", 50);
    f.segment(
        other,
        "text",
        &along(1.0),
        &Space {
            model_sha256: "x".into(),
            ..space("o", false)
        },
    );
    let index = f.index();
    let q = unit(0);
    let p = SearchParams {
        candidates: 1,
        ..PARAMS
    };
    let run = |filter: &Filter| {
        let mut q = semantic(&q, &t, filter);
        q.params = p;
        file_names(&f.search(&index, &q))
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
    };
    let roots = Filter {
        root_ids: vec![b],
        ..Filter::default()
    };
    assert_eq!(run(&roots), ["low.txt", "pic.jpg"]);
    let kinds = Filter {
        kinds: vec!["image".into()],
        ..Filter::default()
    };
    assert_eq!(run(&kinds), ["pic.jpg"]);
    let time = Filter {
        since: Some(10),
        until: Some(100),
        ..Filter::default()
    };
    assert_eq!(run(&time), ["low.txt"]);
    let none = run(&Filter::default());
    assert_eq!(none.len(), 2);
    assert!(none[0].starts_with('a') && none[1] == "pic.jpg");
}

#[test]
fn text_query_finds_audio_segments() {
    let f = Fixture::new();
    let r = f.root("D:\\录音");
    let aud = Space {
        preprocess: "audio-v1-s10-o2-q-60".into(),
        ..space("a", true)
    };
    let rec = f.file(r, "会议.m4a", "audio", 0);
    f.segment(rec, "audio", &along(0.3), &aud);
    let best = f.segment(rec, "audio", &along(0.8), &aud);
    let pic = f.file(r, "pic.jpg", "image", 0);
    f.segment(pic, "image", &along(0.5), &space("i", true));
    let index = f.index();
    let q = unit(0);
    let filter = Filter::default();
    let hits = f.search(&index, &semantic(&q, &text_space(), &filter));
    assert_eq!(file_names(&hits), ["会议.m4a", "pic.jpg"]);
    // 最佳命中的音频段在前
    assert_eq!(hits[0].segments[0].segment_id, best);
    assert_eq!(hits[0].segments.len(), 2);
    // 以图搜图不检索音频
    let img = space("i", true);
    let hits = f.search(&index, &semantic(&q, &img, &filter));
    assert_eq!(file_names(&hits), ["pic.jpg"]);
}

fn seg(id: i64, a: u64, b: u64, cosine: f32) -> SegmentHit {
    SegmentHit {
        segment_id: id,
        locator_json: serde_json::json!({ "start_ms": a, "end_ms": b }).to_string(),
        cosine,
    }
}

#[test]
fn video_windows_merged() {
    // 按 z 从高到低（余弦在这里与 z 同序）：0–2、2–4 相邻，6.4–8 与 4–6 间隔 0.4 秒，10–12 单独；第 6 个窗口超出前 5 个不取
    let segs = vec![
        seg(1, 2000, 4000, 0.9),
        seg(2, 0, 2000, 0.8),
        seg(3, 10_000, 12_000, 0.7),
        seg(4, 6400, 8000, 0.6),
        seg(5, 4000, 6000, 0.5),
        seg(6, 20_000, 22_000, 0.4),
    ];
    // 键的顺序随 serde_json 的 feature 而变，按值比较
    let view = |got: &[SegmentHit]| -> Vec<(i64, u64, u64)> {
        got.iter()
            .map(|s| {
                let v: serde_json::Value = serde_json::from_str(&s.locator_json).unwrap();
                let ms = |k: &str| v[k].as_u64().unwrap();
                (s.segment_id, ms("start_ms"), ms("end_ms"))
            })
            .collect()
    };
    let got = merge_windows(segs.clone(), 5, 500);
    assert_eq!(view(&got), [(1, 0, 8000), (3, 10_000, 12_000)]);
    assert_eq!(got[0].cosine, 0.9);
    // 间隔 0 时 6.4 秒不与 6 秒合并
    let got = merge_windows(segs.clone(), 5, 0);
    assert_eq!(
        got.iter().map(|s| s.segment_id).collect::<Vec<_>>(),
        [1, 3, 4]
    );
    // 只取 1 个
    let got = merge_windows(segs, 1, 500);
    assert_eq!(view(&got), [(1, 2000, 4000)]);
    // 没有时间定位的原样保留
    let odd = SegmentHit {
        segment_id: 9,
        locator_json: "{}".into(),
        cosine: 0.95,
    };
    assert_eq!(merge_windows(vec![odd.clone()], 5, 500), [odd]);
}

/// 带音频与纯画面的窗口在不同空间，余弦不可比：合并后的代表窗口与各段的顺序按名次（z），不按余弦
#[test]
fn video_windows_follow_rank() {
    // 名次：1 在前；2 的余弦更高但名次在后
    let segs = vec![
        seg(1, 0, 2000, 0.3),
        seg(2, 2000, 4000, 0.9),
        seg(3, 10_000, 12_000, 0.8),
    ];
    let got = merge_windows(segs, 5, 500);
    assert_eq!(
        got.iter()
            .map(|s| (s.segment_id, s.cosine))
            .collect::<Vec<_>>(),
        [(1, 0.3), (3, 0.8)]
    );
}

#[test]
fn text_query_finds_video_windows() {
    let f = Fixture::new();
    let r = f.root("D:\\视频");
    let av = Space {
        preprocess: "video-v1-w6-o0-f6-t70-av".into(),
        ..space("av", true)
    };
    let v = Space {
        preprocess: "video-v1-w2-o0-f2-t70-v".into(),
        ..space("v", true)
    };
    let talk = f.file(r, "讲座.mp4", "video", 0);
    f.segment(talk, "video_av", &along(0.6), &av);
    let clip = f.file(r, "片段.mp4", "video", 0);
    f.segment(clip, "video_v", &along(0.3), &v);
    let index = f.index();
    let q = unit(0);
    let filter = Filter::default();
    let hits = f.search(&index, &semantic(&q, &text_space(), &filter));
    // 两个子空间分开排名，各自的第一名都在结果中
    assert_eq!(file_names(&hits), ["讲座.mp4", "片段.mp4"]);
    assert_eq!(hits[0].score, hits[1].score);
    // 以图搜图不检索视频
    let hits = f.search(&index, &semantic(&q, &space("i", true), &filter));
    assert!(hits.is_empty());
}

#[test]
fn aggregates_by_file() {
    let f = Fixture::new();
    let r = f.root("D:\\资料");
    let t = text_space();
    let one = f.file(r, "one.md", "document", 0);
    let s1 = f.segment(one, "text", &along(0.5), &t);
    let s2 = f.segment(one, "text", &along(0.9), &t);
    let s3 = f.segment(one, "text", &along(0.7), &t);
    let two = f.file(r, "two.md", "document", 0);
    f.segment(two, "text", &along(0.8), &t);
    let index = f.index();
    let q = unit(0);
    let hits = f.search(&index, &semantic(&q, &t, &Filter::default()));
    assert_eq!(file_names(&hits), ["one.md", "two.md"]);
    let h = &hits[0];
    assert_eq!(h.path, "D:\\资料\\one.md");
    assert_eq!(h.kind, "document");
    assert!(!h.name_hit && !h.offline && !h.cloud);
    assert!((h.cosine.unwrap() - 0.9).abs() < 1e-3);
    let segs: Vec<i64> = h.segments.iter().map(|s| s.segment_id).collect();
    assert_eq!(segs, [s2, s3, s1]);
    assert_eq!(h.segments[0].locator_json, "{\"line\":1}");
    assert!(hits[0].score > hits[1].score);
    // 宽高随结果带回；不合法的值当作未记录
    assert_eq!((h.width, h.height), (None, None));
    f.db.lock()
        .unwrap()
        .conn
        .execute(
            "UPDATE files SET width = 640, height = -1 WHERE id = ?1",
            [one],
        )
        .unwrap();
    let hits = f.search(&index, &semantic(&q, &t, &Filter::default()));
    assert_eq!((hits[0].width, hits[0].height), (Some(640), None));
    // RRF：第 1 名 1/61
    assert!((hits[0].score - 1.0 / 61.0).abs() < 1e-12);
}

#[test]
fn rrf_order() {
    let f = Fixture::new();
    let r = f.root("D:\\资料");
    let t = text_space();
    for (name, cos) in [("alpha.txt", 0.9), ("beta.txt", 0.8), ("gamma.txt", 0.7)] {
        let id = f.file(r, name, "document", 0);
        f.segment(id, "text", &along(cos), &t);
    }
    f.file(r, "gamma-notes.pdf", "document", 0);
    let index = f.index();
    let q = unit(0);
    let filter = Filter::default();
    let mut query = semantic(&q, &t, &filter);
    query.text = Some("gamma");
    let hits = f.search(&index, &query);
    // gamma.txt：1/63 + 名称第 1 或第 2 名；gamma-notes.pdf 只有名称命中
    assert_eq!(hits[0].name, "gamma.txt");
    assert!(hits[0].name_hit && hits[0].cosine.is_some());
    // 其余名次取决于两个文件名的 bm25 先后，alpha.txt 总是第 2（同分时余弦高的在前）
    let mut names = file_names(&hits);
    assert_eq!(names.len(), 4);
    assert_eq!(names[1], "alpha.txt");
    names[2..].sort();
    assert_eq!(names[2..], ["beta.txt", "gamma-notes.pdf"]);
    let pdf = hits.iter().find(|h| h.name == "gamma-notes.pdf").unwrap();
    assert!(pdf.cosine.is_none() && pdf.segments.is_empty());
    // 文件名权重为 0 时只按语义
    query.params.weight_name = 0.0;
    let hits = f.search(&index, &query);
    assert_eq!(file_names(&hits), ["alpha.txt", "beta.txt", "gamma.txt"]);
    // 结果数量上限
    query.params.limit = 2;
    assert_eq!(f.search(&index, &query).len(), 2);
    assert_eq!(
        fuse(&[(1.0, vec![1, 2, 3]), (2.0, vec![3])], 60.0, |_| None),
        [
            (3, 1.0 / 63.0 + 2.0 / 61.0),
            (1, 1.0 / 61.0),
            (2, 1.0 / 62.0)
        ]
    );
    // 同分时余弦高的在前，没有余弦的在后，再按 id
    let cos = |id: i64| [None, Some(0.2), Some(0.9), None][id as usize];
    assert_eq!(
        fuse(&[(1.0, vec![1, 0]), (1.0, vec![2, 3])], 60.0, cos),
        [
            (2, 1.0 / 61.0),
            (1, 1.0 / 61.0),
            (0, 1.0 / 62.0),
            (3, 1.0 / 62.0)
        ]
    );
}

/// 长图：整图与切片（另一档位的空间、带切分参数的参数版本）都参加文字检索，同一文件只出一个结果、取最高分，
/// 最佳命中的切片排在 `segments` 第一位并带着它的区域；以图搜图只比较同档位的整图
#[test]
fn long_image_slices_aggregate() {
    let f = Fixture::new();
    let root = f.root("C:\r");
    let long = f.file(root, "长图.png", "image", 0);
    let photo = f.file(root, "照片.jpg", "image", 0);
    let img = space("i", true);
    let slice = Space {
        id: "i560".into(),
        preprocess: "image-v1-t560".into(),
        ..img.clone()
    };
    f.segment(long, "image", &along(0.3), &img);
    let top = f.segment(long, "image", &along(0.5), &slice);
    let best = f.segment(long, "image", &along(0.9), &slice);
    f.segment(photo, "image", &along(0.7), &img);
    {
        let db = f.db.lock().unwrap();
        for (id, y) in [(top, 0), (best, 90)] {
            let loc = format!(r#"{{"region":{{"x":0,"y":{y},"width":100,"height":100}}}}"#);
            db.conn
                .execute(
                    "UPDATE segments SET locator_json = ?2 WHERE id = ?1",
                    params![id, loc],
                )
                .unwrap();
            db.conn
                .execute(
                    "UPDATE vectors SET params_version = 'image-v1-t560-slice-v1-r2.5-n12'
                     WHERE segment_id = ?1",
                    [id],
                )
                .unwrap();
        }
    }
    let index = f.index();
    let (q, t, none) = (unit(0), text_space(), Filter::default());
    let hits = f.search(&index, &semantic(&q, &t, &none));
    assert_eq!(file_names(&hits), ["长图.png", "照片.jpg"]);
    let h = &hits[0];
    assert!((h.cosine.unwrap() - 0.9).abs() < 1e-3, "{h:?}");
    assert_eq!(h.segments.len(), 3);
    assert_eq!(h.segments[0].segment_id, best);
    assert_eq!(
        h.segments[0].locator_json,
        r#"{"region":{"x":0,"y":90,"width":100,"height":100}}"#
    );
    let hits = f.search(&index, &semantic(&q, &img, &none));
    assert_eq!(file_names(&hits), ["照片.jpg", "长图.png"]);
    assert_eq!(hits[1].segments.len(), 1);
}
