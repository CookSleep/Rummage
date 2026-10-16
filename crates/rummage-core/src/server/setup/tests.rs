//! 引导请求的测试：引导状态、选择设备与代理、预估，以及子模块共用的辅助函数；设备检测、模型任务的测试在子模块中
use super::*;
use crate::pipeline::Disabled;
use rummage_embed::selftest as embed_selftest;
use rummage_index::{Db, normalize_root, scan_root};
use rummage_proto::{CPU_DEVICE, DeviceStatus, GpuState, PauseReason, ProxyMode, ProxySetting};

mod blocking;
mod devices;
mod models;

/// 假的系统显卡列表（`Config::gpus`）
fn fake_gpus() -> Vec<String> {
    vec!["GTX".to_string(), "RTX".to_string()]
}

fn cfg(dir: &Path) -> Config {
    let models = dir.join("models").join(space::MODEL_DIR);
    Config {
        exe: dir.join("rummage-core.exe"),
        cpu_runtime: dir.join("cpu"),
        vulkan_runtime: dir.join("vulkan"),
        model: models.join(space::MODEL.name),
        mmproj: models.join(space::MMPROJ.name),
        selftest_cache: dir.join("cache").join("selftest.json"),
        golden: dir.join("golden"),
        ffmpeg: dir.join("ffmpeg"),
        embed_tmp: dir.join("tmp"),
        gpus: fake_gpus,
    }
}

fn open(dir: &Path) -> Arc<State> {
    let state = State::new("t".into(), Db::open(dir));
    state.configure(cfg(dir));
    state
}

fn setup(state: &Arc<State>) -> SetupStatus {
    let Ok(Reply::Setup(s)) = respond(state, Request::SetupStatus) else {
        panic!("应返回引导状态");
    };
    *s
}

#[test]
fn onboarding_flag() {
    let dir = crate::test_dir();
    let state = open(dir.path());
    let s = setup(&state);
    assert!(!s.onboarded && !s.model.ready && !s.mmproj.ready);
    assert_eq!(
        (s.model.name.as_str(), s.model.bytes),
        (space::MODEL.name, space::MODEL.size)
    );
    assert_eq!(s.task, None);
    assert!(s.selftest.preferred == AUTO_DEVICE && s.selftest.devices.is_empty());
    // 引导步骤：记下后重启仍在，完成引导时清除
    assert_eq!(s.step, 0);
    let body = Request::SetOnboardingStep { step: 3 };
    assert_eq!(respond(&state, body), Ok(Reply::Done));
    drop(state);
    let state = open(dir.path());
    assert_eq!(setup(&state).step, 3);
    assert_eq!(respond(&state, Request::FinishSetup), Ok(Reply::Done));
    assert!(setup(&state).onboarded);
    assert_eq!(setup(&state).step, 0);
    drop(state);
    assert!(setup(&open(dir.path())).onboarded);
}

#[test]
fn no_result_after_onboarding_is_stale() {
    let dir = crate::test_dir();
    let state = open(dir.path());
    // 引导中还没检测过：不算过期（引导里自己会检测）
    let st = setup(&state).selftest;
    assert!(st.devices.is_empty() && !st.stale);
    // 已完成引导却没有任何检测结果（如上次列显卡卡住、结果已丢弃）：算过期，会在后台自动检测
    assert_eq!(respond(&state, Request::FinishSetup), Ok(Reply::Done));
    let st = setup(&state).selftest;
    assert!(st.devices.is_empty() && st.stale);
}

#[test]
fn preferred_device_and_proxy() {
    let dir = crate::test_dir();
    let state = open(dir.path());
    let set = |device: &str| {
        respond(
            &state,
            Request::SetPreferredDevice {
                device: device.into(),
            },
        )
    };
    assert_eq!(set(CPU_DEVICE), Ok(Reply::Done));
    assert_eq!(setup(&state).selftest.preferred, CPU_DEVICE);
    assert_eq!(set(AUTO_DEVICE), Ok(Reply::Done));
    assert_eq!(setup(&state).selftest.preferred, AUTO_DEVICE);
    // 没有通过检测的显卡不能选
    let err = set("gpu:RTX|0000:01:00.0").unwrap_err();
    assert_eq!(err.code, ApiError::BAD_REQUEST);
    assert_eq!(setup(&state).selftest.preferred, AUTO_DEVICE);
    assert_eq!(set(CPU_DEVICE), Ok(Reply::Done));
    // 没有模型时不能检测
    let err = respond(&state, Request::RunSelftest { onboarding: false }).unwrap_err();
    assert_eq!(err.code, ApiError::BAD_REQUEST);

    let body = Request::SetProxy {
        setting: ProxySetting {
            mode: ProxyMode::Custom,
            url: "127.0.0.1:7890".into(),
        },
    };
    let Ok(Reply::Proxy(saved)) = respond(&state, body) else {
        panic!("应返回保存后的代理设置");
    };
    assert_eq!(saved.url, "http://127.0.0.1:7890");
    assert_eq!(setup(&state).proxy, saved);
    let bad = Request::SetProxy {
        setting: ProxySetting {
            mode: ProxyMode::Custom,
            url: "socks5://127.0.0.1:1".into(),
        },
    };
    assert_eq!(
        respond(&state, bad).unwrap_err().code,
        ApiError::BAD_REQUEST
    );
    let bad = Request::DownloadModels {
        source: rummage_proto::ModelSource::Custom { url: "x".into() },
        mmproj: false,
    };
    assert_eq!(
        respond(&state, bad).unwrap_err().code,
        ApiError::BAD_REQUEST
    );
}

/// 大小正确的模型文件：稀疏文件，不实际占用几百 MB 磁盘空间
fn fake_model(path: &Path, file: &ModelFile) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    crate::platform::sparse_file(path, file.size).unwrap();
}

#[test]
fn estimate_counts_roots() {
    let dir = crate::test_dir();
    let docs = crate::test_dir();
    std::fs::write(docs.path().join("a.txt"), b"hello").unwrap();
    std::fs::write(docs.path().join("b.png"), b"png").unwrap();
    std::fs::write(docs.path().join("c.pdf"), b"pdf").unwrap();
    let state = open(dir.path());
    let body = Request::AddRoot {
        path: docs.path().to_string_lossy().into_owned(),
        excludes: Vec::new(),
    };
    let mut rx = state.events.subscribe();
    state.start_scanner();
    assert!(matches!(
        super::super::respond(body, &state),
        Ok(Reply::Root { .. })
    ));
    // 等扫描全部结束（启动对账与 AddRoot 各一次）
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while state.scan_pending.load(Ordering::Relaxed) > 0 {
        assert!(std::time::Instant::now() < deadline, "等待扫描结束超时");
        let _ = rx.try_recv();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let Ok(Reply::Estimate(e)) = respond(&state, Request::Estimate) else {
        panic!("应返回预估");
    };
    assert!(!e.scanning);
    // 数据目录所在磁盘的剩余空间
    assert_eq!(e.free_gb.is_some_and(|gb| gb > 0.0), cfg!(windows));
    assert_eq!(e.roots.len(), 1);
    assert_eq!(e.roots[0].files, 3);
    assert_eq!(
        e.roots[0].kinds["document"],
        KindTotal { files: 2, bytes: 8 }
    );
    assert_eq!(e.roots[0].kinds["image"], KindTotal { files: 1, bytes: 3 });
    drop(state);
}
