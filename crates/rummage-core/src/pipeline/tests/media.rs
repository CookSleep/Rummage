//! 图片、`.ts`、视频与音频：长图切片、相同内容只推理一次、`.ts` 是否为视频的判定、视频窗口与带音频失败时
//! 改用纯画面、视频预设、音频切段与超长音频、超大图片只记元数据。
use super::*;

/// 长图切片：整图片段在前、不带区域；切片按切片档位的空间计算，参数版本带切分参数，locator 记原图中的区域。
/// 内容相同的另一张长图只推理一次
#[test]
fn long_image_sliced() {
    // 100×400：长宽比 4，段长 100、步长 90，需要 ceil(300 / 90) + 1 = 5 段
    let long = bmp(100, 400);
    let mut e = env(&[("长图.bmp", &long), ("副本.bmp", &long)]);
    e.start(true, true);
    let s = e.wait_idle(0);
    assert_eq!((s.done, s.failed), (2, 0));
    assert_eq!(e.stub.embeds.load(Ordering::Relaxed), 6);
    let values = params::defaults();
    let tier = int(&values, "image_tokens") as u32;
    let cfg = SpaceConfig::current(Backend::Cpu);
    let version = space::image_slice_preprocess(tier, 2.5, 12);
    for name in ["长图.bmp", "副本.bmp"] {
        let rows = e.strings(&format!(
            "SELECT s.locator_json || ' ' || v.space_id || ' ' || v.params_version
             FROM segments s JOIN files f ON f.id = s.file_id JOIN vectors v ON v.segment_id = s.id
             WHERE f.name = '{name}' ORDER BY s.id"
        ));
        let mut want = vec![format!("{{}} {} image-v1-t140", cfg.image(140))];
        for y in [0, 75, 150, 225, 300] {
            want.push(format!(
                r#"{{"region":{{"x":0,"y":{y},"width":100,"height":100}}}} {} {version}"#,
                cfg.image(tier)
            ));
        }
        assert_eq!(rows, want, "{name}");
    }
    assert_eq!(
        e.strings("SELECT width || 'x' || height FROM files ORDER BY id"),
        ["100x400", "100x400"]
    );
}

#[test]
fn same_image_embedded_once() {
    let corgi = fs::read(CORGI).unwrap();
    let mut e = env(&[("a.jpg", &corgi), ("b.jpg", &corgi)]);
    e.start(true, true);
    e.wait_idle(0);
    assert_eq!(
        e.stub.embeds.load(Ordering::Relaxed),
        1,
        "{:?}",
        (e.job("a.jpg"), e.job("b.jpg"))
    );
    // 之后再出现一份：从数据库复制
    e.put("c.jpg", &corgi);
    e.scan();
    let s = e.wait_idle(0);
    assert_eq!(s.done, 3);
    assert_eq!(e.stub.embeds.load(Ordering::Relaxed), 1);
    assert_eq!(e.count("SELECT COUNT(*) FROM vectors"), 3);
    assert_eq!(e.count("SELECT COUNT(DISTINCT vec) FROM vectors"), 1);
    // 复制来的也带宽高
    assert_eq!(
        e.count("SELECT COUNT(*) FROM files WHERE width > 0 AND height > 0"),
        3
    );
}

/// 同步字节对、内容却不是视频的 `.ts`
fn fake_ts() -> Vec<u8> {
    let mut ts = vec![0u8; 188 * 3];
    for i in 0..3 {
        ts[i * 188] = 0x47;
    }
    ts
}

/// 没有 mmproj 时只判定种类：是视频的改为 `video`，不是的仍为 `other` 并记下已判定，都不留作业
#[test]
fn ts_judged_without_video_model() {
    let mut e = env(&[("clip.ts", &fake_ts()), ("app.ts", b"export const x = 1\n")]);
    e.start(true, false);
    let s = e.wait_until(|s| {
        s.total == 0
            && e.count("SELECT COUNT(*) FROM files WHERE kind = 'video' OR quick_hash IS NOT NULL")
                == 2
    });
    assert_eq!(s.failed, 0);
    assert_eq!(
        e.strings("SELECT name || ' ' || kind || ' ' || status FROM files ORDER BY name"),
        ["app.ts other pending", "clip.ts video pending"]
    );
    assert_eq!(e.job("app.ts"), None);
    assert_eq!(e.job("clip.ts"), None);
}

#[test]
fn ts_video_indexed_as_video() {
    if !has_ffmpeg() {
        return;
    }
    // 判定为视频的 .ts 就地按视频处理：内容读不出时记为失败，种类为视频
    let mut e = env(&[("clip.ts", &fake_ts())]);
    e.start(true, true);
    e.wait_until(|s| s.state == IndexState::Idle && s.failed == 1);
    let err = e.job("clip.ts").unwrap().2.unwrap();
    assert!(err.contains("无法读取此文件中的视频"), "{err}");
}

/// 用 ffmpeg 生成 `secs` 秒、320×180、每秒 10 帧的测试视频（MPEG-4 Part 2，LGPL 构建自带的编码器），可带 AAC 音轨
fn make_video(secs: u32, audio: bool) -> Vec<u8> {
    let dir = crate::test_dir();
    let out = dir.path().join("v.mp4");
    let mut cmd = std::process::Command::new(ffmpeg_dir().join("ffmpeg.exe"));
    cmd.args(["-v", "error", "-nostdin", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=duration={secs}:size=320x180:rate=10"));
    if audio {
        cmd.args(["-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:duration={secs}"))
            .args(["-c:a", "aac"]);
    }
    let status = cmd
        .args(["-c:v", "mpeg4", "-y"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success());
    fs::read(out).unwrap()
}

fn spans(e: &Env, name: &str) -> Vec<(i64, i64)> {
    e.strings(&format!(
        "SELECT s.locator_json FROM segments s JOIN files f ON f.id = s.file_id
         WHERE f.name = '{name}' ORDER BY s.id"
    ))
    .iter()
    .map(|l| {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        (
            v["start_ms"].as_i64().unwrap(),
            v["end_ms"].as_i64().unwrap(),
        )
    })
    .collect()
}

#[test]
fn video_windows_stored() {
    if !has_ffmpeg() {
        return;
    }
    let clip = make_video(7, true);
    let mut e = env(&[
        ("片段.mp4", &clip),
        ("副本.mp4", &clip),
        ("坏.mp4", b"ftyp not a video"),
    ]);
    e.start(true, true);
    let s = e.wait_idle(0);
    assert_eq!((s.by_kind["video"].done, s.by_kind["video"].failed), (2, 1));
    // 默认“找物体”：2 秒一段、纯画面；7 秒的视频末段到 7 秒为止；内容相同的只推理一次
    assert_eq!(
        spans(&e, "片段.mp4"),
        [(0, 2000), (2000, 4000), (4000, 6000), (6000, 7000)]
    );
    assert_eq!(e.stub.embeds.load(Ordering::Relaxed), 4);
    assert_eq!(
        e.strings("SELECT DISTINCT v.params_version FROM vectors v"),
        ["video-v2-w2-o0-f2-t70-v"]
    );
    assert_eq!(
        e.strings("SELECT DISTINCT modality FROM segments"),
        ["video_v"]
    );
    for meta in e.strings("SELECT meta_json FROM files WHERE status = 'stored'") {
        let v: serde_json::Value = serde_json::from_str(&meta).unwrap();
        assert_eq!(
            (v["width"].as_u64(), v["height"].as_u64()),
            (Some(320), Some(180))
        );
        assert_eq!(v["has_audio"], true);
        assert_eq!(v["codec"], "mpeg4");
    }
    assert_eq!(
        e.strings("SELECT width || 'x' || height FROM files WHERE status = 'stored'"),
        ["320x180", "320x180"]
    );
}

#[test]
fn video_with_audio_falls_back_to_frames() {
    if !has_ffmpeg() {
        return;
    }
    let mut e = env(&[
        ("讲话.mp4", &make_video(7, true)),
        ("无声.mp4", &make_video(3, false)),
    ]);
    // “找说话内容”：6 秒 6 帧带音频
    e.extra = vec![("video_preset", ParamValue::Int(3))];
    e.start(true, true);
    e.wait_idle(0);
    assert_eq!(spans(&e, "讲话.mp4"), [(0, 6000), (6000, 7000)]);
    let modality = |name: &str| {
        e.strings(&format!(
            "SELECT DISTINCT s.modality || ' ' || v.params_version FROM segments s
             JOIN files f ON f.id = s.file_id JOIN vectors v ON v.segment_id = s.id WHERE f.name = '{name}'"
        ))
    };
    assert_eq!(
        modality("讲话.mp4"),
        ["video_av video-v2-w6-o0-f6-t70-av-m1024"]
    );
    // 没有音轨的只看画面
    assert_eq!(modality("无声.mp4"), ["video_v video-v2-w6-o0-f6-t70-v"]);
    // 建好的都符合当前设置，不重建
    assert!(
        e.pipeline
            .as_ref()
            .unwrap()
            .requeue(&[])
            .unwrap()
            .is_empty()
    );

    // 带音频超过 token 上限：整段改为纯画面，从第一个窗口重新算
    let mut e = env(&[("讲话.mp4", &make_video(7, true))]);
    e.extra = vec![("video_preset", ParamValue::Int(3))];
    e.stub.av_too_long.store(true, Ordering::Relaxed);
    e.start(true, true);
    e.wait_idle(0);
    assert!(e.stored("讲话.mp4"));
    assert_eq!(
        *lock(&e.stub.videos),
        [
            "video-v2-w6-o0-f6-t70-av-m1024",
            "video-v2-w6-o0-f6-t70-v",
            "video-v2-w6-o0-f6-t70-v"
        ]
    );
    // 向量在纯画面空间，参数版本记带音频的版本，按当前设置不必重建
    assert_eq!(
        e.strings(
            "SELECT DISTINCT s.modality || ' ' || sp.preprocess || ' ' || v.params_version FROM segments s
             JOIN vectors v ON v.segment_id = s.id JOIN spaces sp ON sp.id = v.space_id"
        ),
        ["video_v video-v2-w6-o0-f6-t70-v video-v2-w6-o0-f6-t70-av-m1024"]
    );
    assert!(
        e.pipeline
            .as_ref()
            .unwrap()
            .requeue(&[])
            .unwrap()
            .is_empty()
    );
    // 改了片段长度上限：改为只看画面的视频要重建
    assert_eq!(
        e.set_and_requeue("video_max_tokens", ParamValue::Int(2048)),
        BTreeMap::from([("video".into(), 1)])
    );
}

#[test]
fn video_opts_from_presets() {
    let mut values = params::defaults();
    let opts = video_opts(&values);
    assert_eq!(
        opts.windows,
        WindowOpts {
            window_s: 2,
            overlap_s: 0,
            frames: 2
        }
    );
    assert_eq!(opts.audio_tokens, None);
    values.insert("video_preset".into(), ParamValue::Int(3));
    let opts = video_opts(&values);
    assert_eq!((opts.windows.window_s, opts.windows.frames), (6, 6));
    assert_eq!(opts.audio_tokens, Some(1024));
    // 预设下高级项不生效
    values.insert("video_window_s".into(), ParamValue::Int(9));
    assert_eq!(video_opts(&values), opts);
    // 自定义：用高级项，重叠按窗口减 1 秒封顶
    values.insert("video_preset".into(), ParamValue::Int(0));
    values.insert("video_overlap_s".into(), ParamValue::Int(9));
    let opts = video_opts(&values);
    assert_eq!(
        opts.windows,
        WindowOpts {
            window_s: 9,
            overlap_s: 8,
            frames: 2
        }
    );
    assert_eq!(opts.audio_tokens, None);
    values.insert("video_audio".into(), ParamValue::Bool(true));
    assert_eq!(video_opts(&values).audio_tokens, Some(1024));
}

/// 16 kHz 单声道 16 位 PCM WAV：`tone` 秒正弦接 `silence` 秒静音
fn wav(tone: u32, silence: u32) -> Vec<u8> {
    let n = (tone + silence) as usize * 16_000;
    let data: Vec<u8> = (0..n)
        .map(|i| {
            if i < tone as usize * 16_000 {
                ((i as f32 * 0.05).sin() * 8000.0) as i16
            } else {
                0
            }
        })
        .flat_map(i16::to_le_bytes)
        .collect();
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    for v in [16u32, 0x0001_0001, 16_000, 32_000, 0x0010_0002] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    out
}

#[test]
fn audio_segments_stored() {
    if !has_ffmpeg() {
        return;
    }
    // 30 秒：前 10 秒有声音。4 段均匀分布：0–10、6.67–16.67、13.33–23.33、20–30，后两段全是静音，跳过
    let sound = wav(10, 20);
    let mut e = env(&[
        ("录音.wav", &sound),
        ("副本.wav", &sound),
        ("坏.mp3", b"ID3 not audio"),
    ]);
    e.start(true, true);
    let s = e.wait_idle(0);
    assert_eq!((s.by_kind["audio"].done, s.by_kind["audio"].failed), (2, 1));
    // 内容相同的只推理一次
    assert_eq!(e.stub.embeds.load(Ordering::Relaxed), 2);
    let locs = e.strings(
        "SELECT s.locator_json FROM segments s JOIN files f ON f.id = s.file_id
         WHERE f.name = '录音.wav' AND s.modality = 'audio' ORDER BY s.id",
    );
    let spans: Vec<(i64, i64)> = locs
        .iter()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            (
                v["start_ms"].as_i64().unwrap(),
                v["end_ms"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(spans, [(0, 10_000), (6666, 16_666)]);
    assert_eq!(
        e.strings("SELECT DISTINCT v.params_version FROM vectors v"),
        ["audio-v2-s10-o2-q-60"]
    );
    // 元数据，复制来的也有
    for meta in e.strings("SELECT meta_json FROM files WHERE kind = 'audio' AND status = 'stored'")
    {
        let v: serde_json::Value = serde_json::from_str(&meta).unwrap();
        assert_eq!(v["duration_ms"], 30_000);
        assert_eq!(v["codec"], "pcm_s16le");
        assert_eq!(v["has_gps"], false);
    }
    assert!(
        e.job("坏.mp3")
            .unwrap()
            .2
            .unwrap()
            .contains("无法读取此文件中的音频")
    );
}

#[test]
fn long_audio_only_metadata() {
    if !has_ffmpeg() {
        return;
    }
    let mut e = env(&[("长.wav", &wav(70, 0)), ("短.wav", &wav(5, 0))]);
    e.extra = vec![("max_audio_min", ParamValue::Int(1))];
    e.start(true, true);
    let s = e.wait_idle(0);
    assert_eq!(s.by_kind["audio"].done, 1);
    assert_eq!(e.job("长.wav"), None);
    assert_eq!(
        e.strings("SELECT status FROM files WHERE name = '长.wav'"),
        ["too_large"]
    );
    let meta = &e.strings("SELECT meta_json FROM files WHERE name = '长.wav'")[0];
    assert!(meta.contains("\"duration_ms\":70000"), "{meta}");
    assert!(e.stored("短.wav"));
}

/// 超过像素上限的图片同超长音频：只记元数据、按文件名搜索，不算失败、不进黑名单
#[test]
fn huge_image_only_metadata() {
    // 上限取最小的 10 百万像素，图片略超过它
    let mut e = env(&[("大.bmp", &bmp(1000, 10_001))]);
    e.extra = vec![("max_image_mp", ParamValue::Int(10))];
    e.start(true, true);
    let s = e.wait_idle(0);
    assert_eq!(s.failed, 0);
    assert_eq!(e.job("大.bmp"), None);
    assert_eq!(
        e.strings("SELECT status || ' ' || meta_json FROM files WHERE name = '大.bmp'"),
        ["too_large {\"pixel_limit\":10000000}"]
    );
    assert_eq!(e.count("SELECT COUNT(*) FROM blacklist"), 0);
    assert_eq!(e.stub.embeds.load(Ordering::Relaxed), 0);
}
