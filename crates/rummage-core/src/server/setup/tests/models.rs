//! 模型任务的测试：校验与删除、引导中的校验、与设备检测互斥、后台导入
use super::*;

/// 等模型任务结束
fn finished(rx: &mut tokio::sync::broadcast::Receiver<Event>) -> ModelTask {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        assert!(std::time::Instant::now() < deadline, "等待模型任务结束超时");
        match rx.try_recv() {
            Ok(Event::ModelTask(t)) if t.state != ModelTaskState::Running => return t,
            Ok(_) => {}
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
}

#[test]
fn verify_and_delete_models() {
    let dir = crate::test_dir();
    let state = open(dir.path());
    let c = cfg(dir.path());
    let mut rx = state.events.subscribe();
    // 没有安装时不能校验
    let err = respond(
        &state,
        Request::VerifyModel {
            mmproj: true,
            npu: false,
        },
    )
    .unwrap_err();
    assert_eq!(err.code, ApiError::BAD_REQUEST);

    // 校验在后台进行，可以取消；取消时不记结果
    fake_model(&c.model, &space::MODEL);
    let body = Request::VerifyModel {
        mmproj: false,
        npu: false,
    };
    assert_eq!(respond(&state, body), Ok(Reply::Done));
    assert_eq!(respond(&state, Request::CancelModelTask), Ok(Reply::Done));
    let task = finished(&mut rx);
    assert_eq!(task.kind, ModelTaskKind::Verify);
    assert_eq!(task.state, ModelTaskState::Cancelled);
    assert_eq!(task.message.as_deref(), Some("已取消校验。"));
    assert_eq!(setup(&state).model.verified, None);

    fake_model(&c.mmproj, &space::MMPROJ);
    let part = c
        .mmproj
        .with_file_name(format!("{}.part", space::MMPROJ.name));
    std::fs::write(&part, b"x").unwrap();
    set_verified(&state, space::MMPROJ.name, Some((&c.mmproj, true)));
    set_verified(&state, space::MODEL.name, Some((&c.model, false)));
    let s = setup(&state);
    assert_eq!(
        (s.mmproj.verified, s.model.verified),
        (Some(true), Some(false))
    );

    // 设备检测进行中不能删除、下载或导入
    lock(&state.setup.selftest).running = true;
    let download = Request::DownloadModels {
        source: rummage_proto::ModelSource::Official,
        mmproj: true,
        npu: false,
    };
    let import = Request::ImportModels {
        paths: vec![c.mmproj.to_string_lossy().into_owned()],
    };
    for body in [
        Request::DeleteModel {
            mmproj: true,
            npu: false,
        },
        download,
        import,
    ] {
        let err = respond(&state, body).unwrap_err();
        assert_eq!(err.code, ApiError::BAD_REQUEST);
        assert_eq!(err.message, "设备检测正在进行，请等它完成后再试。");
    }
    assert_eq!(setup(&state).task.unwrap().state, ModelTaskState::Cancelled);
    lock(&state.setup.selftest).running = false;

    assert_eq!(
        respond(
            &state,
            Request::DeleteModel {
                mmproj: true,
                npu: false
            }
        ),
        Ok(Reply::Done)
    );
    let task = finished(&mut rx);
    assert_eq!(task.state, ModelTaskState::Done);
    assert_eq!(task.file.as_deref(), Some(space::MMPROJ.name));
    assert_eq!(task.message.as_deref(), Some("已删除图片与音视频模型。"));
    assert!(!c.mmproj.exists() && !part.exists() && c.model.exists());
    let s = setup(&state);
    assert!(!s.mmproj.ready && s.mmproj.partial_bytes == 0);
    // 只有下载了一部分的文件时也能删除
    std::fs::write(&part, b"x").unwrap();
    assert_eq!(
        respond(
            &state,
            Request::DeleteModel {
                mmproj: true,
                npu: false
            }
        ),
        Ok(Reply::Done)
    );
    let task = finished(&mut rx);
    assert_eq!(task.message.as_deref(), Some("已删除下载了一部分的文件。"));
    assert!(!part.exists());
    // 校验记录随删除清除：再放回同样大小的文件也不算校验过
    fake_model(&c.mmproj, &space::MMPROJ);
    assert_eq!(setup(&state).mmproj.verified, None);

    // 文字模型也能删除，图片与音视频模型保留
    assert_eq!(
        respond(
            &state,
            Request::DeleteModel {
                mmproj: false,
                npu: false
            }
        ),
        Ok(Reply::Done)
    );
    let task = finished(&mut rx);
    assert_eq!(task.file.as_deref(), Some(space::MODEL.name));
    assert_eq!(task.message.as_deref(), Some("已删除文字模型。"));
    assert!(!c.model.exists() && c.mmproj.exists());
    let s = setup(&state);
    assert!(!s.model.ready && s.model.verified.is_none() && s.mmproj.ready);
}

/// 引导中的校验只校验已安装、还没有通过记录的文件，没有时不开始任务
#[test]
fn verify_unverified_models() {
    let dir = crate::test_dir();
    let state = open(dir.path());
    let c = cfg(dir.path());
    let mut rx = state.events.subscribe();
    let body = Request::VerifyModels {
        mmproj: true,
        npu: false,
    };
    assert_eq!(respond(&state, body.clone()), Ok(Reply::Done));
    assert_eq!(setup(&state).task, None);

    fake_model(&c.model, &space::MODEL);
    fake_model(&c.mmproj, &space::MMPROJ);
    set_verified(&state, space::MODEL.name, Some((&c.model, true)));
    // 不需要 mmproj 时只看文字模型，它已通过
    assert_eq!(
        respond(
            &state,
            Request::VerifyModels {
                mmproj: false,
                npu: false
            }
        ),
        Ok(Reply::Done)
    );
    assert_eq!(setup(&state).task, None);
    // 记为未通过的也不自动重校
    set_verified(&state, space::MMPROJ.name, Some((&c.mmproj, false)));
    assert_eq!(respond(&state, body.clone()), Ok(Reply::Done));
    assert_eq!(setup(&state).task, None);
    // 换了文件（修改时间不同）后记录失效，要重新校验
    let f = std::fs::File::options()
        .write(true)
        .open(&c.mmproj)
        .unwrap();
    f.set_modified(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1))
        .unwrap();
    drop(f);
    assert_eq!(setup(&state).mmproj.verified, None);

    assert_eq!(respond(&state, body), Ok(Reply::Done));
    // 第一个带文件名的进度：只校验 mmproj
    let t = loop {
        if let Ok(Event::ModelTask(t)) = rx.blocking_recv()
            && t.file.is_some()
        {
            break t;
        }
    };
    assert_eq!(t.kind, ModelTaskKind::Verify);
    assert_eq!(
        (t.file.as_deref(), t.index, t.count),
        (Some(space::MMPROJ.name), 1, 1)
    );
    assert_eq!(respond(&state, Request::CancelModelTask), Ok(Reply::Done));
    let task = finished(&mut rx);
    assert_eq!(task.state, ModelTaskState::Cancelled);
    let s = setup(&state);
    assert_eq!((s.model.verified, s.mmproj.verified), (Some(true), None));
}

/// 下载、导入或删除进行中不能检测设备；校验不拦。另一个方向见 `verify_and_delete_models`
#[test]
fn selftest_waits_for_model_task() {
    let dir = crate::test_dir();
    let state = open(dir.path());
    fake_model(&cfg(dir.path()).model, &space::MODEL);
    let running = |kind| ModelTask {
        kind,
        state: ModelTaskState::Running,
        phase: ModelTaskPhase::Preparing,
        file: None,
        index: 0,
        count: 0,
        done: 0,
        total: 0,
        speed: None,
        message: None,
    };
    for kind in [
        ModelTaskKind::Download,
        ModelTaskKind::Import,
        ModelTaskKind::Delete,
    ] {
        *lock(&state.setup.task) = Some(running(kind));
        let err = respond(&state, Request::RunSelftest { onboarding: false }).unwrap_err();
        assert_eq!(
            err.message,
            "正在下载、导入或删除模型，请等它完成后再检测设备。"
        );
        assert!(!lock(&state.setup.selftest).running);
    }
    // 任务进行中不能再开始另一个
    let err = respond(
        &state,
        Request::VerifyModel {
            mmproj: false,
            npu: false,
        },
    )
    .unwrap_err();
    assert_eq!(
        err.message,
        "模型正在下载、导入、校验或删除，请等它完成后再试。"
    );
}

#[test]
fn import_runs_in_background() {
    let dir = crate::test_dir();
    let state = open(dir.path());
    let mut rx = state.events.subscribe();
    let odd = dir.path().join("odd.gguf");
    std::fs::write(&odd, b"x").unwrap();
    let body = Request::ImportModels {
        paths: vec![odd.to_string_lossy().into_owned()],
    };
    assert_eq!(respond(&state, body), Ok(Reply::Done));
    let task = finished(&mut rx);
    assert_eq!(task.state, ModelTaskState::Failed);
    assert!(
        task.message
            .unwrap()
            .contains("不是 Rummage 使用的模型文件")
    );
    assert_eq!(setup(&state).task.unwrap().state, ModelTaskState::Failed);
    let empty = Request::ImportModels { paths: Vec::new() };
    assert_eq!(
        respond(&state, empty).unwrap_err().code,
        ApiError::BAD_REQUEST
    );
}
