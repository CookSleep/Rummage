//! 参数表的测试：键唯一且默认值合法、分组连续、校验与规范化、给前端的序列化、错误消息
use super::*;

#[test]
fn keys_unique_and_defaults_valid() {
    for (i, d) in PARAMS.iter().enumerate() {
        assert!(
            PARAMS[i + 1..].iter().all(|o| o.key != d.key),
            "{} 重复",
            d.key
        );
        validate(d.key, &(d.default)()).unwrap();
        assert!(d.help.chars().count() <= 60, "{} 的说明过长", d.key);
        assert_eq!(d.affects_index, !d.index_kinds.is_empty(), "{}", d.key);
        // 选项已写出单位，名称中不再重复
        if let ParamKind::Named { unit, .. } = d.kind {
            assert!(unit.is_empty() || !d.label.contains(unit), "{}", d.key);
        }
    }
}

/// 界面按顺序分组：同组同小标题的参数必须相邻，否则同一个小标题会出现两次
#[test]
fn sections_contiguous() {
    let mut seen: Vec<(Group, &str)> = Vec::new();
    for d in PARAMS.iter().filter(|d| !d.section.is_empty()) {
        let cur = (d.group, d.section);
        if seen.last() != Some(&cur) {
            assert!(
                !seen.contains(&cur),
                "{} 的小标题“{}”不相邻",
                d.key,
                d.section
            );
            seen.push(cur);
        }
    }
}

#[test]
fn defaults_cover_all_keys() {
    let map = defaults();
    assert_eq!(map.len(), PARAMS.len());
    assert!(PARAMS.iter().all(|d| map.contains_key(d.key)));
}

#[test]
fn validate_rejects_bad_values() {
    let cases = [
        ("chunk_tokens", ParamValue::Int(127)),
        ("chunk_tokens", ParamValue::Int(513)),
        ("chunk_tokens", ParamValue::Float(384.0)),
        ("chunk_tokens", ParamValue::Bool(true)),
        ("weight_name", ParamValue::Float(2.1)),
        ("weight_name", ParamValue::Float(f64::NAN)),
        ("weight_name", ParamValue::Bool(false)),
        ("image_tokens", ParamValue::Int(141)),
        ("image_tokens", ParamValue::Float(140.0)),
        ("pause_on_battery", ParamValue::Int(1)),
        ("video_preset", ParamValue::Int(4)),
        ("video_preset", ParamValue::Float(1.0)),
        ("video_decoder", ParamValue::Int(2)),
        ("video_frame_tokens", ParamValue::Int(280)),
        ("no_such_key", ParamValue::Int(1)),
    ];
    for (key, value) in cases {
        assert!(validate(key, &value).is_err(), "{key} = {value:?}");
    }
}

#[test]
fn validate_accepts_good_values() {
    validate("chunk_tokens", &ParamValue::Int(128)).unwrap();
    validate("weight_name", &ParamValue::Float(0.5)).unwrap();
    validate("image_tokens", &ParamValue::Int(1120)).unwrap();
    validate("pause_on_battery", &ParamValue::Bool(false)).unwrap();
    validate("video_preset", &ParamValue::Int(0)).unwrap();
}

#[test]
fn validate_normalizes_int_to_float() {
    assert_eq!(
        validate("weight_name", &ParamValue::Int(2)).unwrap(),
        ParamValue::Float(2.0)
    );
    assert_eq!(
        validate("result_limit", &ParamValue::Int(50)).unwrap(),
        ParamValue::Int(50)
    );
}

#[test]
fn def_serializes_for_frontend() {
    let json = serde_json::to_string(def("image_tokens").unwrap()).unwrap();
    assert!(
        json.contains(
            r#""kind":{"Named":{"options":[[70,"最低"],[140,"较低"],[280,"标准"],[560,"较高"],[1120,"最高"]],"unit":"Token"}}"#
        ),
        "{json}"
    );
    assert!(
        json.contains(r#""affectsIndex":true,"indexKinds":["image"]"#),
        "{json}"
    );
    assert!(json.contains(r#""default":280"#), "{json}");
    assert!(json.contains(r#""section":"图片""#), "{json}");
    let json = serde_json::to_string(def("long_image_tokens").unwrap()).unwrap();
    assert!(json.contains(r#""default":140"#), "{json}");
    assert!(json.contains(r#""indexKinds":["image"]"#), "{json}");
    let json = serde_json::to_string(def("video_frame_tokens").unwrap()).unwrap();
    assert!(
        json.contains(r#""kind":{"Named":{"options":[[70,"低"],[140,"标准"]],"unit":"Token"}}"#),
        "{json}"
    );
    assert!(json.contains(r#""default":140"#), "{json}");
    assert!(json.contains(r#""indexKinds":["video"]"#), "{json}");
    let json = serde_json::to_string(def("result_limit").unwrap()).unwrap();
    assert!(json.contains(r#""indexKinds":[]"#), "{json}");
    let json = serde_json::to_string(def("video_preset").unwrap()).unwrap();
    assert!(
        json.contains(
            r#""kind":{"Named":{"options":[[1,"找物体"],[2,"找动作"],[3,"找说话内容"],[0,"自定义"]],"unit":""}}"#
        ),
        "{json}"
    );
}

#[test]
fn error_message_is_sentence() {
    let err = validate("chunk_tokens", &ParamValue::Int(1)).unwrap_err();
    assert_eq!(
        err.to_string(),
        "“文本分段长度（Token）”应在 128 到 512 之间。"
    );
}

#[test]
fn platform_only_params() {
    for key in ["pause_when_screen_on", "thermal_slow_headroom"] {
        let d = def(key).unwrap();
        assert_eq!(d.platform, "android");
        assert_eq!(d.shown(), cfg!(target_os = "android"), "{key}");
    }
    // Android 上不起作用：视频一律用 MediaCodec 解码，没有全屏检测与显卡，识别在应用进程内的线程中运行、没有作业对象限制内存
    for key in [
        "video_decoder",
        "pause_on_fullscreen",
        "gpu_selftest_timeout_s",
        "worker_mem_gb",
    ] {
        let d = def(key).unwrap();
        assert_eq!(d.platform, "windows");
        assert_eq!(d.shown(), cfg!(windows), "{key}");
    }
    assert!(def("pause_on_battery").unwrap().shown());
    assert_eq!(
        flag(&defaults(), "pause_on_battery"),
        !cfg!(target_os = "android")
    );
}
