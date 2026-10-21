//! 测速的测试：按参数安排测速（与所选的黄金样本一致时沿用样本耗时）、参数快照、按测速结果选择设备、旧缓存的回退
use std::path::PathBuf;

use rummage_media::video::VideoPart;

use super::super::speed::{Plan, SpeedPlan, plan};
use super::*;
use crate::Input;

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/golden")
}

/// 默认的视频预设“找物体”
const OBJECTS: VideoSpeed = VideoSpeed {
    window_s: 2,
    frames: 2,
    audio: false,
};

fn plan_with(values: &BTreeMap<String, ParamValue>, video: VideoSpeed, images: bool) -> SpeedPlan {
    let g = golden::load(&golden_dir(), values, video, u64::MAX, golden::Refs::Gguf).unwrap();
    plan(&golden_dir(), &g, values, video, images, u64::MAX).unwrap()
}

fn reused(p: &Option<Plan>) -> Option<&Vec<String>> {
    match p {
        Some(Plan::Reuse(ids)) => Some(ids),
        _ => None,
    }
}

fn measured(p: &Option<Plan>) -> Option<&Vec<Input>> {
    match p {
        Some(Plan::Measure(inputs)) => Some(inputs),
        _ => None,
    }
}

fn keys(p: &SpeedPlan) -> Vec<&str> {
    p.params.keys().map(String::as_str).collect()
}

#[test]
fn reuses_golden_timings_when_params_match() {
    let mut values = params::defaults();
    let m = golden::manifest(&golden_dir()).unwrap();
    // 图片总是按当前档位取黄金样本，各档都复用
    for tier in [70, 280, 1120] {
        values.insert("image_tokens".into(), ParamValue::Int(tier));
        let g = golden::load(
            &golden_dir(),
            &values,
            OBJECTS,
            u64::MAX,
            golden::Refs::Gguf,
        )
        .unwrap();
        assert!(g.samples.iter().all(|s| match &s.input {
            Input::Image { tokens, .. } => *tokens == tier as u32,
            _ => s.modality != Modality::Image,
        }));
        let p = plan_with(&values, OBJECTS, true);
        let images = reused(&p.image).unwrap();
        assert_eq!(images.len(), m.images.len());
        assert!(images.iter().all(|id| {
            g.samples
                .iter()
                .any(|s| s.id == *id && s.modality == Modality::Image)
        }));
    }
    // 默认 10 秒一段与 5–10 秒的音频样本相当
    let p = plan_with(&values, OBJECTS, true);
    assert_eq!(reused(&p.audio).unwrap().len(), m.audios.len());
    // 三个预设各有对应的视频变体，两个帧档位都复用
    for tier in [70, 140] {
        values.insert("video_frame_tokens".into(), ParamValue::Int(tier));
        for (vs, id) in [
            (OBJECTS, "vid01_f2_v"),
            (
                VideoSpeed {
                    window_s: 4,
                    frames: 4,
                    audio: false,
                },
                "vid01_f4_v",
            ),
            (
                VideoSpeed {
                    window_s: 6,
                    frames: 6,
                    audio: true,
                },
                "vid01_f6_av",
            ),
        ] {
            let p = plan_with(&values, vs, true);
            assert_eq!(*reused(&p.video).unwrap(), [id]);
        }
    }
}

#[test]
fn measures_when_params_differ() {
    let mut values = params::defaults();
    values.insert("audio_segment_s".into(), ParamValue::Int(30));
    let p = plan_with(&values, OBJECTS, true);
    let Some([Input::Audio { samples, .. }]) = measured(&p.audio).map(Vec::as_slice) else {
        panic!("应另测一段音频")
    };
    assert_eq!(samples.len(), 30 * 16_000);
    // 自定义的窗口长度与所选变体（“找物体”，2 秒）不同：按当前设置组装一个只看画面的片段
    let custom = VideoSpeed {
        window_s: 3,
        frames: 2,
        audio: false,
    };
    let p = plan_with(&values, custom, true);
    let Some(
        [
            Input::Video {
                parts, preprocess, ..
            },
        ],
    ) = measured(&p.video).map(Vec::as_slice)
    else {
        panic!("应另测一个视频片段")
    };
    assert_eq!(parts.len(), 2);
    assert!(parts.iter().all(|x| matches!(x, VideoPart::Frame(_))));
    let frame_tokens = params::int(&values, "video_frame_tokens") as u32;
    assert_eq!(
        *preprocess,
        space::video_preprocess(3, 0, 2, frame_tokens, false)
    );
    // 没有对应变体的帧数（取默认变体，与当前设置不同）：带音频时每帧前有时间戳与一段音频
    let p = plan_with(
        &values,
        VideoSpeed {
            window_s: 6,
            frames: 3,
            audio: true,
        },
        true,
    );
    let Some([Input::Video { parts, .. }]) = measured(&p.video).map(Vec::as_slice) else {
        panic!("应另测一个视频片段")
    };
    assert_eq!(parts.len(), 12);
    // 关闭识别的类型参数不一致时不测；图片总是复用
    values.insert("index_images".into(), ParamValue::Bool(false));
    values.insert("index_audio".into(), ParamValue::Bool(false));
    values.insert("index_video".into(), ParamValue::Bool(false));
    let p = plan_with(&values, custom, true);
    assert!(reused(&p.image).is_some() && p.audio.is_none() && p.video.is_none());
    assert_eq!(keys(&p), ["chunk_tokens", "image_tokens", "index_threads"]);
}

#[test]
fn snapshot_keys() {
    let mut values = params::defaults();
    values.insert("chunk_tokens".into(), ParamValue::Int(200));
    // 只测文本时只记文本相关的参数
    let p = plan_with(&values, OBJECTS, false);
    assert!(p.image.is_none() && p.audio.is_none() && p.video.is_none());
    assert_eq!(keys(&p), ["chunk_tokens", "index_threads"]);
    assert_eq!(p.params["chunk_tokens"], ParamValue::Int(200));
    let p = plan_with(&values, OBJECTS, true);
    assert_eq!(
        keys(&p),
        [
            "audio_segment_s",
            "chunk_tokens",
            "image_tokens",
            "index_threads",
            "video_frame_tokens",
            "video_preset",
        ]
    );
    // “自定义”时另记窗口参数
    values.insert("video_preset".into(), ParamValue::Int(0));
    let p = plan_with(&values, OBJECTS, true);
    for k in ["video_window_s", "video_frames", "video_audio"] {
        assert!(p.params.contains_key(k), "{k}");
    }
}

fn with_speed(ms: f64, image_ms: f64, chunk: i64) -> CandidateReport {
    let mut r = report(Status::Passed, Some(ms));
    r.speed = Some(DeviceSpeed {
        image_ms: Some(image_ms),
        params: BTreeMap::from([("image_tokens".to_string(), ParamValue::Int(chunk))]),
        ..DeviceSpeed::default()
    });
    r
}

#[test]
fn selects_by_speed_when_all_measured() {
    // 样本耗时 0 号更快，按当前参数测速 1 号更快
    let reports = vec![with_speed(10.0, 900.0, 280), with_speed(20.0, 300.0, 280)];
    assert_eq!(select(&reports, true), Some(1));
    // 有一个没有测速结果（旧的缓存）时按样本耗时
    let mut old = reports.clone();
    old[1].speed = None;
    assert_eq!(select(&old, true), Some(0));
    // 测速时的参数不同（沿用的上次结果）也按样本耗时
    let mut mixed = reports.clone();
    mixed[1] = with_speed(20.0, 300.0, 560);
    assert_eq!(select(&mixed, true), Some(0));
    // 未通过的不参与比较
    let mut failed = reports;
    failed.push(report(Status::Failed, Some(1.0)));
    assert_eq!(select(&failed, true), Some(1));
}

#[test]
fn reads_cache_without_speed() {
    let mut r = report(Status::Passed, Some(5.0));
    let mut json = serde_json::to_value(&r).unwrap();
    json.as_object_mut().unwrap().remove("speed");
    let old: CandidateReport = serde_json::from_value(json).unwrap();
    assert_eq!(old.speed, None);
    // 解除屏蔽后连同测速结果一起清掉
    r.speed = Some(DeviceSpeed::default());
    super::super::cache::unblocked(&mut r);
    assert_eq!(r.speed, None);
}
