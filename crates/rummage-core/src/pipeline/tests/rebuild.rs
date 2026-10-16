//! 设置与索引绑定：符合当前设置时不重建，改参数后只把不符的文件放回重建。
use super::*;

/// 建好的索引都符合当前设置：启动或再检查时不重建
#[test]
fn fresh_index_needs_no_rebuild() {
    let long = bmp(100, 400);
    let mut e = env(&[
        ("长图.bmp", &long),
        ("方图.bmp", &bmp(64, 64)),
        ("说明.md", "# 标题\n\n正文。\n".as_bytes()),
    ]);
    e.start(true, true);
    e.wait_idle(0);
    assert!(
        e.pipeline
            .as_ref()
            .unwrap()
            .requeue(&[])
            .unwrap()
            .is_empty()
    );
    assert_eq!(e.status().rebuild, None);
    assert_eq!(
        e.strings("SELECT DISTINCT params_version FROM vectors WHERE params_version LIKE 'text%'"),
        ["text-v1-t64-c384-o15-m64"]
    );
}

/// 改长图判定比例：只有长图重建，新成为长图的补上切片，普通图片不动
#[test]
fn ratio_change_rebuilds_only_long_images() {
    let long = bmp(100, 400);
    let mut e = env(&[("长图.bmp", &long), ("方图.bmp", &bmp(64, 64))]);
    // 按 5 倍判定时长宽比 4 的图不是长图，没有切片
    e.extra.push(("long_image_ratio", ParamValue::Float(5.0)));
    e.start(true, true);
    e.wait_idle(0);
    assert_eq!(e.stub.embeds.load(Ordering::Relaxed), 2);
    let n = e.set_and_requeue("long_image_ratio", ParamValue::Float(2.5));
    assert_eq!(n, BTreeMap::from([("image".into(), 1)]));
    e.wait_until(|s| s.state == IndexState::Idle && s.rebuild.is_none());
    // 只算了 5 段切片：整图向量本次运行中刚算过（推理线程记住的最近结果），方图没有重算
    assert_eq!(e.stub.embeds.load(Ordering::Relaxed), 2 + 5);
    assert_eq!(
        e.count("SELECT COUNT(*) FROM vectors WHERE params_version LIKE '%slice%'"),
        5
    );
    assert!(
        e.pipeline
            .as_ref()
            .unwrap()
            .requeue(&[])
            .unwrap()
            .is_empty()
    );
    // 再改回 5 倍：长图删掉切片
    e.set_and_requeue("long_image_ratio", ParamValue::Float(5.0));
    e.wait_until(|s| s.state == IndexState::Idle && s.rebuild.is_none());
    assert_eq!(
        e.count("SELECT COUNT(*) FROM vectors WHERE params_version LIKE '%slice%'"),
        0
    );
    assert_eq!(e.count("SELECT COUNT(*) FROM vectors"), 2);
}

/// 默认的各项版本：整图 280 档，长图切片 140 档，视频帧 140 档
#[test]
fn default_versions() {
    let w = crate::pipeline::rebuild::wanted(&params::defaults());
    assert_eq!(w.image, "image-v1-t280");
    assert_eq!(w.slice, "image-v1-t140-slice-v1-r2.5-n12");
    assert_eq!(w.video, "video-v2-w2-o0-f2-t140-v");
    assert_eq!(w.video_av, None);
}

/// 三个档位各自只改变自己的版本：切片档位只影响长图，图片档位影响整图与文档的内嵌媒体，视频帧档位影响视频与
/// 文档的内嵌媒体
#[test]
fn each_tier_changes_only_its_versions() {
    let base = params::defaults();
    let wanted = |key: &str, v: i64| {
        let mut values = base.clone();
        values.insert(key.to_string(), ParamValue::Int(v));
        let mut w = crate::pipeline::rebuild::wanted(&values);
        w.doc_media = serde_json::to_string(&crate::pipeline::docmedia::tokens(
            &values, true, true, true,
        ))
        .unwrap();
        w
    };
    let before = wanted("image_tokens", 280);
    let slice = wanted("long_image_tokens", 560);
    assert_eq!(slice.slice, "image-v1-t560-slice-v1-r2.5-n12");
    assert_eq!(
        rummage_index::Wanted {
            slice: before.slice.clone(),
            ..slice
        },
        before
    );
    let image = wanted("image_tokens", 140);
    assert_eq!(image.image, "image-v1-t140");
    assert_ne!(image.doc_media, before.doc_media);
    assert_eq!(
        rummage_index::Wanted {
            image: before.image.clone(),
            doc_media: before.doc_media.clone(),
            ..image
        },
        before
    );
    let video = wanted("video_frame_tokens", 70);
    assert_eq!(video.video, "video-v2-w2-o0-f2-t70-v");
    assert_ne!(video.doc_media, before.doc_media);
    assert_eq!(
        rummage_index::Wanted {
            video: before.video.clone(),
            doc_media: before.doc_media.clone(),
            ..video
        },
        before
    );
}

/// 只改长图每段的档位：只有长图重建，方图不动；整图向量的档位不变
#[test]
fn slice_tier_rebuilds_only_long_images() {
    let long = bmp(100, 400);
    let mut e = env(&[("长图.bmp", &long), ("方图.bmp", &bmp(64, 64))]);
    e.start(true, true);
    e.wait_idle(0);
    let n = e.set_and_requeue("long_image_tokens", ParamValue::Int(280));
    assert_eq!(n, BTreeMap::from([("image".into(), 1)]));
    e.wait_until(|s| s.state == IndexState::Idle && s.rebuild.is_none());
    assert_eq!(
        e.strings("SELECT DISTINCT params_version FROM vectors ORDER BY params_version"),
        ["image-v1-t280", "image-v1-t280-slice-v1-r2.5-n12"]
    );
    assert!(
        e.pipeline
            .as_ref()
            .unwrap()
            .requeue(&[])
            .unwrap()
            .is_empty()
    );
}

/// 改了档位后按新档位重建，与新文件一起处理完；完成后进度清空
#[test]
fn rebuild_after_param_change() {
    let mut e = env(&[("狗.bmp", &bmp(64, 64))]);
    e.start(true, true);
    e.wait_idle(0);
    let n = e.set_and_requeue("image_tokens", ParamValue::Int(560));
    assert_eq!(n, BTreeMap::from([("image".into(), 1)]));
    e.put("新.txt", "新文件。".as_bytes());
    e.scan();
    let s = e.wait_until(|s| s.state == IndexState::Idle && s.done == 2 && s.rebuild.is_none());
    assert_eq!((s.pending, s.failed), (0, 0));
    assert!(e.stored("新.txt"));
    assert_eq!(
        e.strings("SELECT params_version FROM vectors WHERE params_version LIKE 'image%'"),
        ["image-v1-t560"]
    );
}

/// 作业所用的参数在领取时取：引擎建好以后才改的参数，之后领取的作业按新参数计算
#[test]
fn claimed_job_takes_params_at_claim() {
    let mut e = env(&[("方图.bmp", &bmp(64, 64))]);
    lock(&e.db).meta_set(ONBOARDED_KEY, "0").unwrap();
    e.start(true, true);
    {
        let p = e.pipeline.as_ref().unwrap();
        lock(&p.shared.params).insert("image_tokens".into(), ParamValue::Int(560));
        p.hold(Hold::Setup, false);
    }
    e.wait_idle(0);
    assert_eq!(
        e.strings("SELECT params_version FROM vectors WHERE params_version LIKE 'image%'"),
        ["image-v1-t560"]
    );
}
