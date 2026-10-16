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

/// 改了档位后按新档位重建，与新文件一起处理完；完成后进度清空
#[test]
fn rebuild_after_param_change() {
    let mut e = env(&[("狗.bmp", &bmp(64, 64))]);
    e.start(true, true);
    e.wait_idle(0);
    let n = e.set_and_requeue("image_tokens", ParamValue::Int(280));
    assert_eq!(n, BTreeMap::from([("image".into(), 1)]));
    e.put("新.txt", "新文件。".as_bytes());
    e.scan();
    let s = e.wait_until(|s| s.state == IndexState::Idle && s.done == 2 && s.rebuild.is_none());
    assert_eq!((s.pending, s.failed), (0, 0));
    assert!(e.stored("新.txt"));
    assert_eq!(
        e.strings("SELECT params_version FROM vectors WHERE params_version LIKE 'image%'"),
        ["image-v1-t280"]
    );
}
