//! 协议消息的测试：请求、回复与事件的序列化往返，握手与版本
use super::*;

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(msg: T) {
    let line = encode(&msg);
    assert!(!line.contains('\n'));
    assert_eq!(decode::<T>(&line).unwrap(), msg);
}

#[test]
fn whole_drive_roots() {
    for path in [
        r"C:\",
        "d:",
        "E:/",
        "/storage/emulated/0",
        "/storage/1A2B-3c4d/",
    ] {
        assert!(whole_drive(path), "{path}");
    }
    for path in [
        r"C:\Users",
        r"\\server\share",
        "/storage/emulated/0/DCIM",
        "/storage/emulated",
        "/storage/1A2B-3C4",
        "/storage/1A2B_3C4D",
        "/storage",
    ] {
        assert!(!whole_drive(path), "{path}");
    }
}

#[test]
fn handshake_round_trip() {
    round_trip(Hello {
        protocol: PROTOCOL_VERSION,
        token: "abc\n123".into(),
    });
    round_trip::<Result<Welcome, ApiError>>(Ok(Welcome {
        protocol: PROTOCOL_VERSION,
        core_version: "0.1.0".into(),
    }));
    round_trip::<Result<Welcome, ApiError>>(Err(ApiError {
        code: ApiError::UNAUTHORIZED.into(),
        message: "连接令牌不正确。".into(),
    }));
}

#[test]
fn request_round_trip() {
    for body in [
        Request::Ping,
        Request::GetParams,
        Request::Shutdown,
        Request::ListRoots,
        Request::AddRoot {
            path: r"D:\资料".into(),
            excludes: vec!["*.tmp".into()],
        },
        Request::RemoveRoot { id: 3 },
        Request::IndexStatus,
        Request::FailedFiles {
            after: None,
            limit: 200,
        },
        Request::FailedFiles {
            after: Some(FailedCursor {
                failed_at: 1_760_000_000_000,
                file_id: 9,
            }),
            limit: 200,
        },
        Request::RetryFailed { file_id: 3 },
        Request::RetryAllFailed,
        Request::IgnoreFailed { file_id: 3 },
        Request::IgnoreAllFailed,
        Request::PauseIndexing { until: None },
        Request::PauseIndexing {
            until: Some(1_760_000_000_000),
        },
        Request::ResumeIndexing,
        Request::BackgroundQuota { exhausted: true },
        Request::Search {
            text: Some("柯基 \"飞盘\"".into()),
            image_path: None,
            similar: Some(SimilarTo {
                file_id: 3,
                segment_id: None,
            }),
            kinds: vec!["image".into()],
            root_ids: vec![1, 2],
            since: Some(0),
            until: None,
            limit: Some(20),
            snippets: true,
            supersede: false,
        },
        Request::FileInfo { file_id: 3 },
        Request::Thumbnail { file_id: 3 },
        Request::SegmentText { segment_id: 9 },
        Request::VideoFrame {
            file_id: 3,
            ms: 192_500,
        },
        Request::RegionThumb { segment_id: 9 },
        Request::SetupStatus,
        Request::FinishSetup,
        Request::SetOnboardingStep { step: 2 },
        Request::DownloadModels {
            source: ModelSource::ModelScope,
            mmproj: true,
            npu: false,
        },
        Request::ImportModels {
            paths: vec![r"D:\下载.gguf".into()],
        },
        Request::CancelModelTask,
        Request::VerifyModel {
            mmproj: false,
            npu: true,
        },
        Request::VerifyModels {
            mmproj: true,
            npu: false,
        },
        Request::DeleteModel {
            mmproj: false,
            npu: true,
        },
        Request::SetProxy {
            setting: ProxySetting {
                mode: ProxyMode::Custom,
                url: "127.0.0.1:7890".into(),
            },
        },
        Request::RunSelftest { onboarding: true },
        Request::SetPreferredDevice {
            device: "cpu".into(),
        },
        Request::UnblockGpu {
            device: "gpu:RTX|0000:01:00.0".into(),
            failed: false,
        },
        Request::SetRootExcludes {
            id: 2,
            excludes: vec!["node_modules".into(), "照片/**/*.tmp".into()],
            skip_nomedia: Some(false),
        },
        Request::GetAppearance,
        Request::SetAppearance {
            appearance: Appearance {
                ui_font: FontMode::Custom,
                ui_font_name: "微软雅黑".into(),
                ..Appearance::default()
            },
        },
        Request::GetNotices,
        Request::DismissNotice {
            notice: Notice::DbRestored,
        },
        Request::Estimate,
        Request::DbStatus,
        Request::ClearIndex,
        Request::GetHistory,
        Request::AddHistory {
            query: "柯基 \"飞盘\"".into(),
        },
        Request::RemoveHistory {
            query: "旅行".into(),
        },
        Request::ClearHistory,
        Request::SetParam {
            key: "weight_name".into(),
            value: ParamValue::Float(1.5),
        },
        Request::SetParam {
            key: "result_limit".into(),
            value: ParamValue::Int(50),
        },
        Request::SetParam {
            key: "pause_on_battery".into(),
            value: ParamValue::Bool(false),
        },
    ] {
        round_trip(ClientMsg::Request { id: 7, body });
    }
}

#[test]
fn response_and_event_round_trip() {
    for result in [
        Ok(Reply::Pong),
        Ok(Reply::Done),
        Ok(Reply::DoneWithWarning {
            message: "设置已保存。".into(),
        }),
        Ok(Reply::Params {
            values: params::defaults(),
        }),
        Ok(Reply::Roots {
            roots: vec![RootInfo {
                id: 1,
                path: r"E:\照片".into(),
                offline: true,
                missing: false,
                files: 42,
                excludes: vec!["*.tmp".into()],
                skip_nomedia: true,
                scanning: true,
            }],
        }),
        Err(ApiError {
            code: ApiError::BAD_REQUEST.into(),
            message: "没有名为“x”的参数。".into(),
        }),
        Ok(Reply::Results {
            hits: vec![SearchHit {
                file_id: 3,
                path: r"D:\资料.md".into(),
                name: "a.md".into(),
                kind: "document".into(),
                size: 10,
                mtime: 1,
                cloud: false,
                offline: true,
                missing: false,
                score: 0.03,
                cosine: Some(0.5),
                z: Some(2.5),
                match_pct: Some(88),
                low_relevance: false,
                segments: vec![SegmentHit {
                    segment_id: 9,
                    locator_json: "{}".into(),
                    cosine: 0.5,
                }],
                name_hit: true,
                text_segments: vec![9, 4],
                width: Some(640),
                height: None,
                snippet: Some(Snippet {
                    segment_id: 9,
                    text: "正文……".into(),
                }),
            }],
            took_ms: 12,
            partial: true,
            semantic_unavailable: false,
            unavailable: None,
        }),
        Ok(Reply::Results {
            hits: Vec::new(),
            took_ms: 1,
            partial: false,
            semantic_unavailable: true,
            unavailable: Some(SemanticUnavailable::ImagesOff),
        }),
        Ok(Reply::File(FileInfo {
            file_id: 3,
            path: r"D:\资料\a.md".into(),
            name: "a.md".into(),
            ext: "md".into(),
            kind: "document".into(),
            cloud: true,
            offline: false,
            missing: false,
            size: 10,
            mtime: 1,
            width: None,
            height: None,
            status: "stored".into(),
            job: Some(JobInfo {
                stage: "failed".into(),
                attempts: 3,
                last_error: Some("坏了。".into()),
            }),
            segments: 2,
            indexed_at: Some(5),
            spaces: vec![VectorSpaceInfo {
                preprocess: "text-v1".into(),
                backend: "cpu".into(),
            }],
        })),
        Ok(Reply::Thumbnail {
            path: r"D:\data\thumbs\ab.webp".into(),
        }),
        Ok(Reply::FailedFiles {
            files: vec![FailedFile {
                file_id: 3,
                name: "a.pdf".into(),
                path: r"D:\资料\a.pdf".into(),
                kind: "document".into(),
                reason: Some("无法提取正文，文件可能已损坏或格式不受支持。".into()),
                failed_at: 1_760_000_000_000,
                retryable: true,
            }],
            total: 7,
            retryable: 6,
        }),
        Ok(Reply::Segment {
            segment: Some(SegmentPreview {
                file_id: 3,
                locator_json: r#"{"page_start":3,"page_end":3}"#.into(),
                text: Some("正文".into()),
                before: None,
                after: Some("下一段".into()),
            }),
        }),
        Ok(Reply::Segment { segment: None }),
        Ok(Reply::Proxy(ProxySetting::default())),
        Ok(Reply::Estimate(Estimate {
            scanning: false,
            roots: Vec::new(),
            free_gb: None,
        })),
        Ok(Reply::Appearance(Appearance::default())),
        Ok(Reply::Notices {
            notices: vec![Notice::DbRestored],
        }),
        Ok(Reply::Notices {
            notices: Vec::new(),
        }),
        Ok(Reply::DbStatus {
            error: None,
            rebuildable: false,
        }),
        Ok(Reply::DbStatus {
            error: Some("索引数据库已损坏。".into()),
            rebuildable: true,
        }),
        Ok(Reply::History {
            queries: vec!["柯基".into(), "年终 总结".into()],
        }),
    ] {
        round_trip(CoreMsg::Response { id: 1, result });
    }
    round_trip(CoreMsg::Event {
        body: Event::ParamsChanged {
            values: params::defaults(),
        },
    });
    let status = IndexStatus {
        state: IndexState::Running,
        paused: None,
        slow: false,
        total: 3,
        done: 1,
        failed: 1,
        pending: 1,
        by_kind: BTreeMap::from([(
            "image".to_string(),
            IndexCounts {
                total: 3,
                done: 1,
                failed: 1,
                pending: 1,
            },
        )]),
        backend: Some("CPU".into()),
        error: None,
        eta_s: None,
        rebuild: None,
        round: Some(RoundStatus { done: 1, total: 2 }),
    };
    round_trip(CoreMsg::Response {
        id: 2,
        result: Ok(Reply::Index(status.clone())),
    });
    round_trip(CoreMsg::Event {
        body: Event::IndexProgress(status.clone()),
    });
    round_trip(Event::IndexProgress(IndexStatus {
        state: IndexState::Error,
        error: Some("推理引擎无法启动。".into()),
        ..status.clone()
    }));
    let line = encode(&Event::IndexProgress(IndexStatus {
        state: IndexState::NoModel,
        ..status.clone()
    }));
    assert_eq!(encode(&IndexState::NoMmproj), "\"no_mmproj\"");
    for (r, s) in [
        (SemanticUnavailable::NoModel, "no_model"),
        (SemanticUnavailable::ImagesOff, "images_off"),
        (SemanticUnavailable::NoMmproj, "no_mmproj"),
        (SemanticUnavailable::Paused, "paused"),
        (SemanticUnavailable::Engine, "engine"),
        (SemanticUnavailable::QueryTooLong, "query_too_long"),
    ] {
        assert_eq!(encode(&r), format!("\"{s}\""));
        round_trip(r);
    }
    assert!(line.contains("\"type\":\"IndexProgress\"") && line.contains("\"byKind\""));
    assert!(line.contains("\"state\":\"no_model\"") && line.contains("\"paused\":null"));
    assert!(line.contains("\"etaS\":null"));
    assert!(line.contains(r#""round":{"done":1,"total":2}"#), "{line}");
    let line = encode(&Event::IndexProgress(IndexStatus {
        eta_s: Some(125),
        ..status.clone()
    }));
    assert!(line.contains("\"etaS\":125"));
    let rebuild = encode(&Event::IndexProgress(IndexStatus {
        rebuild: Some(RebuildStatus {
            done: 2,
            total: 5,
            by_kind: BTreeMap::from([("image".to_string(), 3)]),
        }),
        ..status.clone()
    }));
    assert!(
        rebuild.contains(r#""rebuild":{"done":2,"total":5,"byKind":{"image":3}}"#),
        "{rebuild}"
    );
    round_trip(decode::<Event>(&rebuild).unwrap());
    assert_eq!(
        decode::<Event>(&line).unwrap(),
        Event::IndexProgress(IndexStatus {
            eta_s: Some(125),
            ..status.clone()
        })
    );
    for paused in [
        PauseReason::Manual { until: None },
        PauseReason::Manual { until: Some(5) },
        PauseReason::Sleeping,
        PauseReason::DiskLow { free_gb: 1.5 },
        PauseReason::Quota,
        PauseReason::Hot,
        PauseReason::Battery,
        PauseReason::LowBattery,
        PauseReason::ScreenOn,
        PauseReason::Fullscreen,
    ] {
        round_trip(Event::IndexProgress(IndexStatus {
            paused: Some(paused),
            slow: false,
            ..status.clone()
        }));
    }
    assert_eq!(
        encode(&PauseReason::Manual { until: None }),
        r#"{"type":"manual","until":null}"#
    );
    assert_eq!(
        encode(&PauseReason::DiskLow { free_gb: 1.5 }),
        r#"{"type":"disk_low","freeGb":1.5}"#
    );
    assert_eq!(encode(&PauseReason::Battery), r#"{"type":"battery"}"#);
    assert_eq!(
        encode(&PauseReason::LowBattery),
        r#"{"type":"low_battery"}"#
    );
    assert_eq!(encode(&PauseReason::Sleeping), r#"{"type":"sleeping"}"#);
    assert_eq!(encode(&PauseReason::ScreenOn), r#"{"type":"screen_on"}"#);
    round_trip(CoreMsg::Event {
        body: Event::Selftest(SelftestStatus::default()),
    });
    assert_eq!(encode(&Event::RootsChanged), r#"{"type":"RootsChanged"}"#);
    round_trip(CoreMsg::Event {
        body: Event::RootsChanged,
    });
    round_trip(CoreMsg::Event {
        body: Event::ScanProgress {
            root_id: 1,
            done: true,
            stats: ScanStats {
                added: 1,
                too_large: 2,
                ..ScanStats::default()
            },
        },
    });
}

#[test]
fn fields_are_camel_case() {
    let line = encode(&Welcome {
        protocol: PROTOCOL_VERSION,
        core_version: "0.1.0".into(),
    });
    assert!(line.contains("\"coreVersion\""));
    let line = encode(&Event::ScanProgress {
        root_id: 1,
        done: false,
        stats: ScanStats::default(),
    });
    assert!(line.contains("\"rootId\"") && line.contains("\"tooLarge\""));
}

#[test]
fn decode_rejects_garbage() {
    assert!(decode::<ClientMsg>("{\"type\":\"Nope\"}").is_err());
    assert!(decode::<ClientMsg>("not json").is_err());
}
