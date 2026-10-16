//! 大小上限、时长上限、占位文件与内容索引的开关：超过上限不建作业、按库中记下的时长判定、
//! 占位文件保留或删除向量、修改上限或开关后重新判定，以及 `.ts` / `.mts` 是否为视频的判定。
use super::*;

#[test]
fn too_large_not_queued() {
    let e = env();
    let limits = ScanLimits {
        max_doc_mb: 1,
        max_image_mb: 1,
        ..LIMITS
    };
    let big = vec![b'x'; 1024 * 1024 + 1];
    e.put("big.txt", &big);
    e.put("big.rs", &big);
    e.put("big.jpg", &big);
    e.put("big.mp3", &big);
    e.put("small.jpg", &big[..1024 * 1024]);
    let s = scan_root(&e.db, e.id, limits, &AtomicBool::new(false), &mut |_| {}).unwrap();
    assert_eq!(s.too_large, 2);
    let files = e.files();
    let status = |n: &str| files.iter().find(|f| f.0 == n).unwrap().2.clone();
    assert_eq!(status("big.txt"), "too_large");
    // 代码文件只按文件名索引，不限大小
    assert_eq!(status("big.rs"), "pending");
    assert_eq!(status("big.jpg"), "too_large");
    // 音频不限大小
    assert_eq!(status("big.mp3"), "pending");
    assert_eq!(status("small.jpg"), "pending");
    assert_eq!(
        e.jobs(),
        [
            ("big.mp3".into(), "pending".into(), 0),
            ("small.jpg".into(), "pending".into(), 0)
        ]
    );
}

#[test]
fn audio_judged_by_recorded_duration() {
    let e = env();
    e.put("a.mp3", b"x");
    e.put("b.wav", b"y");
    e.scan();
    // 流水线判定 a.mp3 为 5 小时（超过 4 小时上限），b.wav 已索引；扫描本身不读文件
    e.exec(
        "UPDATE files SET status = 'too_large', meta_json = '{\"duration_ms\":18000000}' WHERE name = 'a.mp3';
         DELETE FROM jobs WHERE file_id = (SELECT id FROM files WHERE name = 'a.mp3');
         UPDATE files SET status = 'stored', meta_json = '{\"duration_ms\":60000}' WHERE name = 'b.wav';
         UPDATE jobs SET stage = 'stored' WHERE file_id = (SELECT id FROM files WHERE name = 'b.wav');",
    );
    let s = e.scan();
    assert_eq!((s.updated, s.too_large), (0, 1));
    assert_eq!(e.status("a.mp3"), "too_large");
    // 上限调到 6 小时：按记下的时长重新判定，补建作业，元数据保留
    let s = e.scan_limits(ScanLimits {
        max_audio_min: 360,
        ..LIMITS
    });
    assert_eq!((s.updated, s.too_large), (1, 0));
    assert_eq!(e.status("a.mp3"), "pending");
    assert_eq!(
        e.jobs(),
        [
            ("a.mp3".into(), "pending".into(), 0),
            ("b.wav".into(), "stored".into(), 0)
        ]
    );
    assert_eq!(
        e.count("SELECT COUNT(*) FROM files WHERE meta_json LIKE '%18000000%'"),
        1
    );
    // 内容变了：不再按旧时长判定，清空元数据
    e.exec(
        "DELETE FROM jobs WHERE file_id = (SELECT id FROM files WHERE name = 'a.mp3');
         UPDATE files SET status = 'too_large' WHERE name = 'a.mp3';",
    );
    e.put("a.mp3", b"longer content");
    e.scan();
    assert_eq!(e.status("a.mp3"), "pending");
    assert_eq!(
        e.count("SELECT COUNT(*) FROM files WHERE name = 'a.mp3' AND meta_json = '{}'"),
        1
    );
    // 关闭音频识别：不建作业，已建好的保留
    e.scan_limits(ScanLimits {
        audio: false,
        video: false,
        ..LIMITS
    });
    assert_eq!(e.jobs(), [("b.wav".into(), "stored".into(), 0)]);
}

#[test]
fn image_judged_by_recorded_pixel_limit() {
    let e = env();
    e.put("a.jpg", b"x");
    e.scan();
    // 流水线按 100 百万像素的上限判定 a.jpg 超过
    e.exec(
        "UPDATE files SET status = 'too_large', meta_json = '{\"pixel_limit\":100000000}' WHERE name = 'a.jpg';
         DELETE FROM jobs;",
    );
    let s = e.scan();
    assert_eq!((s.updated, s.too_large), (0, 1));
    assert_eq!(e.status("a.jpg"), "too_large");
    assert!(e.jobs().is_empty());
    // 上限调高：重新建作业，由流水线再判定
    let s = e.scan_limits(ScanLimits {
        max_image_mp: 200,
        ..LIMITS
    });
    assert_eq!((s.updated, s.too_large), (1, 0));
    assert_eq!(e.status("a.jpg"), "pending");
    assert_eq!(e.jobs(), [("a.jpg".into(), "pending".into(), 0)]);
}

#[test]
fn placeholder_files() {
    let e = env();
    e.put("cloud.jpg", b"x");
    e.put("local.txt", b"x");
    e.put("pending.png", b"x");
    let recall = |rel: &str| {
        if rel == "cloud.jpg" {
            fsx::FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
        } else {
            0
        }
    };
    let s = e.scan_attrs(&recall);
    assert_eq!(s.cloud, 1);
    assert_eq!(
        e.files()[0],
        ("cloud.jpg".into(), "image".into(), "cloud".into(), true)
    );
    assert_eq!(e.jobs().len(), 2);
    // 未变化时不重复写入
    assert_eq!(e.scan_attrs(&recall).updated, 0);

    // local.txt 已有向量
    e.store("local.txt");
    // 被释放空间，变回占位文件
    let freed = |rel: &str| {
        if rel == "cloud.jpg" || rel == "local.txt" || rel == "pending.png" {
            fsx::FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS | fsx::FILE_ATTRIBUTE_OFFLINE
        } else {
            0
        }
    };
    let s = e.scan_attrs(&freed);
    assert_eq!((s.updated, s.cloud), (2, 3));
    assert!(e.files().iter().all(|f| f.2 == "cloud" && f.3));
    assert_eq!(e.count("SELECT COUNT(*) FROM vectors"), 1);
    // 已完成的作业保留，未完成的删除
    assert_eq!(e.jobs(), [("local.txt".into(), "stored".into(), 0)]);

    // 重新下载回本机
    let s = e.scan();
    assert_eq!((s.updated, s.cloud), (3, 0));
    assert_eq!(
        e.files(),
        [
            ("cloud.jpg".into(), "image".into(), "pending".into(), false),
            (
                "local.txt".into(),
                "document".into(),
                "stored".into(),
                false
            ),
            (
                "pending.png".into(),
                "image".into(),
                "pending".into(),
                false
            ),
        ]
    );
    assert_eq!(
        e.jobs(),
        [
            ("cloud.jpg".into(), "pending".into(), 0),
            ("local.txt".into(), "stored".into(), 0),
            ("pending.png".into(), "pending".into(), 0),
        ]
    );
    assert_eq!(e.count("SELECT COUNT(*) FROM vectors"), 1);
}

#[test]
fn recall_on_open_dir_not_entered() {
    let e = env();
    e.put("cloud/a.txt", b"x");
    e.put("b.txt", b"x");
    let recall = |rel: &str| {
        if rel == "cloud" {
            FILE_ATTRIBUTE_RECALL_ON_OPEN
        } else {
            0
        }
    };
    let s = e.scan_attrs(&recall);
    assert_eq!(e.names(), ["b.txt"]);
    assert_eq!(s.excluded, 1);

    // 以前扫到过的记录在目录变成按需下载后保留
    e.scan();
    assert_eq!(e.names(), ["b.txt", "cloud/a.txt"]);
    let s = e.scan_attrs(&recall);
    assert_eq!(s.removed, 0);
    assert_eq!(e.names(), ["b.txt", "cloud/a.txt"]);
}

#[test]
fn freed_unchanged_keeps_vectors() {
    let e = env();
    e.put("a.txt", b"x");
    e.scan();
    e.store("a.txt");
    let s = e.scan_attrs(&|_| fsx::FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS);
    assert_eq!(s.updated, 1);
    assert_eq!(e.status("a.txt"), "cloud");
    assert_eq!(e.vectors(), 2);
    assert_eq!(e.jobs(), [("a.txt".into(), "stored".into(), 0)]);
}

#[test]
fn changed_to_cloud_drops_vectors() {
    let e = env();
    e.put("a.txt", b"x");
    e.scan();
    e.store("a.txt");
    e.put("a.txt", b"changed");
    let s = e.scan_attrs(&|_| fsx::FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS);
    assert_eq!(s.updated, 1);
    assert_eq!(e.status("a.txt"), "cloud");
    assert_eq!(e.vectors(), 0);
    assert!(e.jobs().is_empty());
}

#[test]
fn changed_to_too_large_drops_vectors() {
    let e = env();
    e.put("a.txt", b"x");
    e.scan();
    e.store("a.txt");
    e.put("a.txt", &vec![b'x'; 2 * 1024 * 1024]);
    let s = e.scan_limits(ScanLimits {
        max_doc_mb: 1,
        max_image_mb: 1,
        ..LIMITS
    });
    assert_eq!((s.updated, s.too_large), (1, 1));
    assert_eq!(e.status("a.txt"), "too_large");
    assert_eq!(e.vectors(), 0);
    assert!(e.jobs().is_empty());
}

#[test]
fn limit_change_rejudges() {
    let e = env();
    let big = vec![b'x'; 2 * 1024 * 1024];
    e.put("a.txt", &big);
    e.put("done.txt", &big);
    let loose = ScanLimits {
        max_doc_mb: 3,
        max_image_mb: 3,
        ..LIMITS
    };
    let tight = ScanLimits {
        max_doc_mb: 1,
        max_image_mb: 1,
        ..LIMITS
    };
    e.scan_limits(loose);
    e.store("done.txt");
    assert_eq!(e.status("a.txt"), "pending");

    // 调低上限：未完成的改为过大并删除未完成的作业，已完成的保持
    let s = e.scan_limits(tight);
    assert_eq!(s.updated, 1);
    assert_eq!(e.status("a.txt"), "too_large");
    assert_eq!(e.status("done.txt"), "stored");
    assert_eq!(e.jobs(), [("done.txt".into(), "stored".into(), 0)]);
    assert_eq!(e.vectors(), 2);
    // 上限不变时不再写入
    assert_eq!(e.scan_limits(tight).updated, 0);

    // 调高上限：重新排队
    let s = e.scan_limits(loose);
    assert_eq!(s.updated, 1);
    assert_eq!(e.status("a.txt"), "pending");
    assert_eq!(
        e.jobs(),
        [
            ("a.txt".into(), "pending".into(), 0),
            ("done.txt".into(), "stored".into(), 0)
        ]
    );
}

#[test]
fn kind_switches_rejudge() {
    let e = env();
    e.put("a.txt", b"a");
    e.put("b.py", b"b");
    e.put("c.png", b"c");
    e.put("d.ts", b"d");
    e.put("done.png", b"x");
    let no_images = ScanLimits {
        images: false,
        ..LIMITS
    };
    let no_docs = ScanLimits {
        documents: false,
        ..LIMITS
    };
    // 一开始就关闭图片：图片只记录元数据，不建作业；代码文件只按文件名索引
    let s = e.scan_limits(no_images);
    assert_eq!(s.added, 5);
    let names = |e: &Env| e.jobs().into_iter().map(|j| j.0).collect::<Vec<_>>();
    assert_eq!(names(&e), ["a.txt", "d.ts"]);
    assert_eq!(e.scan_limits(no_images), ScanStats::default());

    // 打开图片：补建作业
    assert_eq!(e.scan_limits(LIMITS).updated, 2);
    assert_eq!(names(&e), ["a.txt", "c.png", "d.ts", "done.png"]);
    e.store("done.png");

    // 关闭文档：未完成的文档删除作业，已完成的图片不受影响
    assert_eq!(e.scan_limits(no_docs).updated, 1);
    assert_eq!(names(&e), ["c.png", "d.ts", "done.png"]);
    assert_eq!(e.scan_limits(no_docs), ScanStats::default());
    // 关闭图片与视频：已完成索引的保留向量，待判定的 .ts 删除作业
    e.scan_limits(ScanLimits {
        documents: false,
        images: false,
        video: false,
        ..LIMITS
    });
    assert_eq!(e.jobs(), [("done.png".into(), "stored".into(), 0)]);
    assert_eq!(e.status("c.png"), "pending");

    // 判定为视频的 .ts：按视频的开关建作业
    e.exec("UPDATE files SET kind = 'video' WHERE name = 'd.ts'");
    let no_video = ScanLimits {
        video: false,
        ..LIMITS
    };
    assert_eq!(e.scan_limits(no_video).updated, 2);
    assert_eq!(names(&e), ["a.txt", "c.png", "done.png"]);
    assert_eq!(e.scan_limits(no_video), ScanStats::default());
    assert_eq!(e.scan_limits(LIMITS).updated, 1);
    assert_eq!(names(&e), ["a.txt", "c.png", "d.ts", "done.png"]);
    assert_eq!(e.scan_limits(LIMITS), ScanStats::default());
}

#[test]
fn ts_not_size_limited() {
    let e = env();
    let big = vec![b'x'; 1024 * 1024 + 1];
    e.put("a.ts", &big);
    e.put("b.MTS", &big);
    e.put("c.m2ts", &big);
    let s = e.scan_limits(ScanLimits {
        max_doc_mb: 1,
        max_image_mb: 1,
        ..LIMITS
    });
    assert_eq!(s.too_large, 0);
    assert_eq!(
        e.files(),
        [
            ("a.ts".into(), "other".into(), "pending".into(), false),
            ("b.MTS".into(), "other".into(), "pending".into(), false),
            ("c.m2ts".into(), "video".into(), "pending".into(), false),
        ]
    );
    assert_eq!(e.jobs().len(), 3);
}

#[test]
fn ts_judged_not_video_not_requeued() {
    let e = env();
    e.put(
        "a.ts",
        b"export const x = 1
",
    );
    e.scan();
    assert_eq!(e.jobs(), [("a.ts".into(), "pending".into(), 0)]);
    // 索引流水线判定不是视频：记下 quick_hash，删除作业（rummage-index jobs::Outcome::NotVideo）
    e.exec(
        "UPDATE files SET quick_hash = x'00' WHERE name = 'a.ts';
         DELETE FROM jobs;",
    );
    assert_eq!(e.scan(), ScanStats::default());
    assert!(e.jobs().is_empty());
    // 关闭再打开视频也不重新判定
    e.scan_limits(ScanLimits {
        video: false,
        ..LIMITS
    });
    assert_eq!(e.scan(), ScanStats::default());
    assert!(e.jobs().is_empty());
    // 文件变了：重新排队，由流水线重新判定
    e.put(
        "a.ts",
        b"export const x = 2;
",
    );
    assert_eq!(e.scan().updated, 1);
    assert_eq!(e.jobs(), [("a.ts".into(), "pending".into(), 0)]);
}

/// 判定为视频的 `.ts` 与其他视频一样按记下的时长套时长上限
#[test]
fn ts_video_judged_by_recorded_duration() {
    let e = env();
    e.put("a.ts", b"x");
    e.scan();
    e.exec(
        "UPDATE files SET kind = 'video', status = 'too_large', meta_json = '{\"duration_ms\":18000000}';
         DELETE FROM jobs;",
    );
    let s = e.scan();
    assert_eq!((s.updated, s.too_large), (0, 1));
    assert!(e.jobs().is_empty());
    let s = e.scan_limits(ScanLimits {
        max_audio_min: 360,
        ..LIMITS
    });
    assert_eq!((s.updated, s.too_large), (1, 0));
    assert_eq!(e.status("a.ts"), "pending");
    assert_eq!(e.jobs(), [("a.ts".into(), "pending".into(), 0)]);
}
