//! `local` 的单元测试。
use super::supervisor::Dead;
use super::*;
use crate::space::SpaceId;
use crate::{MAX_FAILURES, WorkerArgs};

#[test]
fn queue_order() {
    let mut p = Pending::new(Duration::ZERO);
    p.push(Priority::Index, 1);
    p.push(Priority::Index, 2);
    p.push(Priority::Query, 10);
    p.push(Priority::Index, 3);
    p.push(Priority::Query, 11);
    assert_eq!(p.pop(), Some(10));
    assert_eq!(p.pop(), Some(11));
    assert_eq!(p.pop(), Some(1));
    // 后来的查询仍然插到索引请求前面。
    p.push(Priority::Query, 12);
    assert_eq!(p.pop(), Some(12));
    assert_eq!(p.pop(), Some(2));
    assert_eq!(p.pop(), Some(3));
    assert_eq!(p.pop(), None);
}

#[test]
fn limits_from_params() {
    let l = Limits::from_params(&BTreeMap::new());
    assert_eq!(l.mem_bytes, 8 << 30);
    assert_eq!(l.init_timeout, Duration::from_secs(120));
    assert_eq!(l.infer_timeout, Duration::from_secs(120));
    assert_eq!(l.idle, Duration::from_secs(600));
    let values = BTreeMap::from([
        ("worker_mem_gb".to_string(), ParamValue::Int(4)),
        // 越界的取值回到默认值。
        ("infer_timeout_s".to_string(), ParamValue::Int(1)),
        ("engine_idle_min".to_string(), ParamValue::Int(2)),
    ]);
    let l = Limits::from_params(&values);
    assert_eq!(l.mem_bytes, 4 << 30);
    assert_eq!(l.infer_timeout, Duration::from_secs(120));
    assert_eq!(l.idle, Duration::from_secs(120));
}

fn queue(idle: Duration) -> Arc<Queue> {
    Arc::new(Queue {
        pending: Mutex::new(Pending::new(idle)),
        ready: Condvar::new(),
    })
}

fn job() -> Job {
    Job {
        req: Request::Restart,
        reply: mpsc::channel().0,
    }
}

#[test]
fn queue_idle_timeout() {
    // 推理进程在运行：空闲时限到期返回 Idle。
    let q = queue(Duration::from_millis(100));
    let t = Instant::now();
    assert!(matches!(q.next(true), Next::Idle));
    assert!(t.elapsed() >= Duration::from_millis(100));

    // 时限内来了请求：返回请求。
    let q2 = q.clone();
    let h = thread::spawn(move || matches!(q2.next(true), Next::Job(_)));
    thread::sleep(Duration::from_millis(30));
    lock(&q.pending).push(Priority::Index, job());
    q.ready.notify_all();
    assert!(h.join().unwrap());

    // 等待中改短时限（set_idle_timeout 的做法）立即生效。
    lock(&q.pending).idle = Duration::from_secs(3600);
    let q2 = q.clone();
    let h = thread::spawn(move || {
        let t = Instant::now();
        (matches!(q2.next(true), Next::Idle), t.elapsed())
    });
    thread::sleep(Duration::from_millis(100));
    lock(&q.pending).idle = Duration::from_millis(200);
    q.ready.notify_all();
    let (idle, took) = h.join().unwrap();
    assert!(idle && took < Duration::from_secs(5), "{took:?}");

    // 推理进程没有运行时不计空闲，只等请求或关闭。
    let q = queue(Duration::ZERO);
    let q2 = q.clone();
    let h = thread::spawn(move || matches!(q2.next(false), Next::Closed));
    thread::sleep(Duration::from_millis(50));
    lock(&q.pending).closed = true;
    q.ready.notify_all();
    assert!(h.join().unwrap());
}

#[test]
fn restart_clears_failures() {
    let mut sup = Supervisor {
        spec: test_spec(),
        limits: Limits::from_params(&BTreeMap::new()),
        proc: None,
        failures: 2,
        dead: Some(Dead::Unavailable("显卡后端失效".to_string())),
        status: Arc::new(Mutex::new(Status::default())),
        with_mmproj: true,
        kills: 0,
    };
    assert!(matches!(
        sup.ensure_started(false),
        Err(Error::Unavailable(_))
    ));
    sup.restart();
    assert!(sup.dead.is_none() && sup.failures == 0 && !sup.with_mmproj);
    // 之后照常拉起（不存在的）推理进程。
    assert!(matches!(sup.ensure_started(true), Err(Error::Spawn(_))));
    // 空闲卸载不计失败。
    sup.unload();
    assert_eq!(sup.failures, 1);
    // 找不到显卡：之后的请求都返回它，重启后再找。
    let err = sup.fail(Error::GpuNotFound("RTX 4080 SUPER".to_string()), true);
    assert!(matches!(err, Error::GpuNotFound(_)), "{err}");
    assert!(matches!(
        sup.ensure_started(false),
        Err(Error::GpuNotFound(_))
    ));
    sup.restart();
    assert!(sup.dead.is_none());
    // 设备丢失不清除，不会重新初始化已丢失的显卡。
    sup.dead = Some(Dead::DeviceLost("Vulkan0".to_string()));
    sup.restart();
    assert!(matches!(
        sup.ensure_started(false),
        Err(Error::DeviceLost(_))
    ));
    // 推理进程结束不了同样不清除，也不再拉起新的推理进程
    sup.dead = Some(Dead::Stuck("推理进程在 10 秒内没有结束。".to_string()));
    sup.restart();
    assert!(matches!(sup.ensure_started(false), Err(Error::Stuck(_))));
}

#[test]
fn worker_args() {
    let spec = WorkerSpec {
        exe: PathBuf::from("rummage-core.exe"),
        runtime_dir: PathBuf::from("rt"),
        model: PathBuf::from("m.gguf"),
        mmproj: Some(PathBuf::from("p.gguf")),
        threads: 6,
        gpu: Some(Gpu {
            description: "NVIDIA GeForce RTX 4080 SUPER".to_string(),
            pci_id: Some("0000:06:00.0".to_string()),
        }),
        litert: None,
    };
    let args = |mmproj| -> Vec<String> {
        spec.args(mmproj)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    };
    // 默认只加载主模型。
    assert!(!args(false).iter().any(|a| a == "--mmproj"));
    assert_eq!(
        args(true),
        [
            "worker",
            "--runtime-dir",
            "rt",
            "--model",
            "m.gguf",
            "--threads",
            "6",
            "--mmproj",
            "p.gguf",
            "--gpu",
            "NVIDIA GeForce RTX 4080 SUPER",
            "--gpu-pci",
            "0000:06:00.0",
        ]
    );
    // LiteRT 不传 mmproj 与显卡，NPU 带分发库目录
    let litert = WorkerSpec {
        model: PathBuf::from("m.litertlm"),
        mmproj: Some(PathBuf::from("m.litertlm")),
        litert: Some(LiteRtArgs {
            accel: Accel::Npu(PathBuf::from("qnn")),
            tokenizer: PathBuf::from("tokenizer.json"),
            cache_dir: Some(PathBuf::from("cache")),
        }),
        ..spec.clone()
    };
    let args: Vec<String> = litert
        .args(true)
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args,
        [
            "worker",
            "--runtime-dir",
            "rt",
            "--model",
            "m.litertlm",
            "--threads",
            "6",
            "--litert",
            "npu",
            "--tokenizer",
            "tokenizer.json",
            "--cache-dir",
            "cache",
            "--dispatch-dir",
            "qnn",
        ]
    );
    let parsed = WorkerArgs::parse(litert.args(true).into_iter().skip(1)).unwrap();
    assert!(matches!(
        parsed,
        WorkerArgs::Embed { litert: Some(l), mmproj: None, .. } if l == *litert.litert.as_ref().unwrap()
    ));
}

fn test_spec() -> WorkerSpec {
    WorkerSpec {
        exe: PathBuf::from("不存在的-rummage-core.exe"),
        runtime_dir: PathBuf::from("rt"),
        model: PathBuf::from("m.gguf"),
        mmproj: Some(PathBuf::from("p.gguf")),
        threads: 1,
        gpu: None,
        litert: None,
    }
}

#[test]
fn image_tier_checked() {
    let mut sup = Supervisor {
        spec: test_spec(),
        limits: Limits::from_params(&BTreeMap::new()),
        proc: None,
        failures: 0,
        dead: None,
        status: Arc::new(Mutex::new(Status::default())),
        with_mmproj: false,
        kills: 0,
    };
    let space = SpaceId("x".to_string());
    let cfg = SpaceConfig::current(Backend::Cpu);
    let image = |w: u32, h: u32, tokens| Input::Image {
        rgb: rummage_media::Rgb {
            width: w,
            height: h,
            data: vec![0; (w * h * 3) as usize],
        },
        tokens,
    };
    // 尺寸超过档位：12×12 = 144 个视觉 token。
    let err = sup.handle(image(576, 576, 140), &space, &cfg).unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err}");
    // 140 档的方图 11×11 = 121 个 token 通过检查，之后才去启动（不存在的）推理进程。
    let err = sup.handle(image(528, 528, 140), &space, &cfg).unwrap_err();
    assert!(matches!(err, Error::Spawn(_)), "{err}");
    assert!(sup.dead.is_none());
    // 带文字的以图搜图查询同样检查档位
    let query = |w: u32, text: &str| match image(w, w, 140) {
        Input::Image { rgb, tokens } => Input::ImageQuery {
            rgb,
            tokens,
            text: text.into(),
        },
        _ => unreachable!(),
    };
    let err = sup.handle(query(576, "红色"), &space, &cfg).unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err}");
    let err = sup.handle(query(528, "红色"), &space, &cfg).unwrap_err();
    assert!(matches!(err, Error::Spawn(_)), "{err}");
}

#[test]
fn count_text() {
    let mut sup = Supervisor {
        spec: test_spec(),
        limits: Limits::from_params(&BTreeMap::new()),
        proc: None,
        failures: 0,
        dead: None,
        status: Arc::new(Mutex::new(Status::default())),
        with_mmproj: false,
        kills: 0,
    };
    // 文本输入去启动（不存在的）推理进程。
    let err = sup.count(&Input::Query("猫".into())).unwrap_err();
    assert!(matches!(err, Error::Spawn(_)), "{err}");
    assert_eq!(
        prefixed(&Input::Document {
            title: Some("报告".into()),
            text: " 正文 ".into()
        })
        .unwrap(),
        "title: 报告 | text: 正文"
    );
}

#[test]
fn detects_device_lost() {
    // b11461 ggml-vulkan.cpp 256、260、14297 行的输出。
    assert!(device_lost(
        "ggml_vulkan: device lost on Vulkan0, likely caused by previous submission (nodes 3 to 9):"
    ));
    assert!(device_lost("ggml_vulkan: device lost on Vulkan0"));
    assert!(device_lost(
        "ggml_vulkan: device lost on Vulkan0 waiting for submission (nodes 0 to 12):"
    ));
    assert!(device_lost("vk::Queue::submit: ErrorDeviceLost"));
    assert!(device_lost("VK_ERROR_DEVICE_LOST"));
    assert!(!device_lost("ggml_vulkan: Found 1 Vulkan devices"));
}

#[test]
fn mismatch_retried_once() {
    // 依次返回给定的结果，记下启动次数
    let run = |results: Vec<Result<(), Error>>| {
        let mut it = results.into_iter();
        let mut n = 0;
        let res = retry_mismatch(|| {
            n += 1;
            it.next().expect("启动次数超出预期")
        });
        (res, n)
    };
    let mismatch = || Err(Error::GpuMismatch("可见的 GPU 不符".into()));
    assert!(matches!(run(vec![Ok(())]), (Ok(()), 1)));
    assert!(matches!(run(vec![mismatch(), Ok(())]), (Ok(()), 2)));
    // 第二次仍不符：按找不到处理（换下一个能用的设备、不屏蔽）
    assert!(matches!(
        run(vec![mismatch(), mismatch()]),
        (Err(Error::GpuNotFound(_)), 2)
    ));
    // 其他错误不重试
    let crashed = Err(Error::Crashed("x".into()));
    assert!(matches!(run(vec![crashed]), (Err(Error::Crashed(_)), 1)));
}

#[test]
fn detects_out_of_memory() {
    // 1120 档方图超出作业内存上限时的输出。
    assert!(out_of_memory(
        "ggml_vulkan: vk::Device::allocateMemory: ErrorOutOfDeviceMemory"
    ));
    assert!(out_of_memory("VK_ERROR_OUT_OF_HOST_MEMORY"));
    assert!(out_of_memory(
        "ggml_gallocr_reserve_n: failed to allocate Vulkan0 buffer of size 2318367744"
    ));
    assert!(!out_of_memory("mtmd_encode_chunk 失败"));
}

/// 已就绪的 GPU 推理进程，stderr 末尾为 `tail`（用一个立即退出的系统命令代替推理进程）。
fn gpu_sup_with_tail(tail: &[&str]) -> Supervisor {
    let (exe, args): (&str, Vec<OsString>) = if cfg!(windows) {
        ("cmd", vec!["/c".into(), "exit 0".into()])
    } else {
        ("true", Vec::new())
    };
    let proc = Proc::spawn(Path::new(exe), args, 1 << 30).unwrap();
    lock(&proc.tail).extend(tail.iter().map(|l| l.to_string()));
    let status = Arc::new(Mutex::new(Status {
        pid: Some(proc.child.id()),
        ..Status::default()
    }));
    Supervisor {
        spec: WorkerSpec {
            gpu: Some(Gpu {
                description: "NVIDIA GeForce RTX 4080 SUPER".to_string(),
                pci_id: Some("0000:06:00.0".to_string()),
            }),
            ..test_spec()
        },
        limits: Limits::from_params(&BTreeMap::new()),
        proc: Some(proc),
        failures: 0,
        dead: None,
        status,
        with_mmproj: true,
        kills: 0,
    }
}

#[test]
fn gpu_out_of_memory_fails_only_the_input() {
    let oom = [
        "ggml_vulkan: Device memory allocation of size 2318367744 failed.",
        "ggml_vulkan: vk::Device::allocateMemory: ErrorOutOfDeviceMemory",
        "mtmd_encode_chunk: error: vk::Device::allocateMemory: ErrorOutOfDeviceMemory",
    ];
    // 推理出错、stderr 中有显存分配失败：只这一项失败，结束推理进程但不判失效，计一次连续失败
    let mut sup = gpu_sup_with_tail(&oom);
    let err = sup.fail(
        Error::Inference("推理失败：mtmd_encode_chunk 失败".to_string()),
        false,
    );
    assert!(
        matches!(&err, Error::Inference(m) if m.contains("显存或内存不足")),
        "{err}"
    );
    assert!(sup.dead.is_none() && sup.proc.is_none() && sup.failures == 1);
    // 下一次请求在同一块卡上重新拉起（这里是不存在的可执行文件，启动失败才判失效）
    let err = sup.ensure_started(true).unwrap_err();
    assert!(err.to_string().contains("无法启动推理进程"), "{err}");

    // 推理进程因此退出：同样只这一项失败
    let mut sup = gpu_sup_with_tail(&oom);
    let err = sup.fail(Error::Crashed("推理进程关闭了输出".to_string()), true);
    assert!(matches!(err, Error::Inference(_)), "{err}");
    assert!(sup.dead.is_none());

    // 连续 MAX_FAILURES 次（如显存一直被占满）：判为显存不足（与驱动无关，上层换下一块显卡），之后的请求都返回它
    let mut sup = gpu_sup_with_tail(&oom);
    sup.failures = MAX_FAILURES - 1;
    let err = sup.fail(Error::Crashed("推理进程关闭了输出".to_string()), true);
    assert!(matches!(err, Error::OutOfMemory(_)), "{err}");
    assert!(matches!(
        sup.ensure_started(false),
        Err(Error::OutOfMemory(_))
    ));

    // 超时仍按 TDR 处理：判后端失效
    let mut sup = gpu_sup_with_tail(&oom);
    let err = sup.fail(Error::InferTimeout(Duration::from_secs(60)), true);
    assert!(matches!(err, Error::Unavailable(_)), "{err}");

    // 启动阶段（还没就绪）的分配失败与输入无关：判后端失效
    let mut sup = gpu_sup_with_tail(&oom);
    lock(&sup.status).pid = None;
    let err = sup.fail(Error::InitFailed("无法加载模型".to_string()), true);
    assert!(matches!(err, Error::Unavailable(_)), "{err}");

    // 其他推理错误照旧：判后端失效，由上层改用 CPU
    let mut sup = gpu_sup_with_tail(&["mtmd_encode_chunk: error: unknown"]);
    let err = sup.fail(
        Error::Inference("推理失败：mtmd_encode_chunk 失败".to_string()),
        false,
    );
    assert!(matches!(err, Error::Unavailable(_)), "{err}");
    assert!(matches!(
        sup.ensure_started(true),
        Err(Error::Unavailable(_))
    ));
}

/// 不会自己退出的子进程（代替挂起的推理进程）
fn long_running() -> Proc {
    let (exe, args): (&str, Vec<OsString>) = if cfg!(windows) {
        ("cmd", vec!["/c".into(), "ping -n 60 127.0.0.1 >nul".into()])
    } else {
        ("sleep", vec!["60".into()])
    };
    Proc::spawn(Path::new(exe), args, 1 << 30).unwrap()
}

#[test]
fn kill_wait_is_bounded() {
    let mut p = long_running();
    // 进程不退出（驱动挂死时结束不了）、stderr 也一直开着：最多等给定的时间就放弃，不会卡住
    let t = Instant::now();
    let (status, ended) = p.wait_exit(Duration::from_millis(200));
    assert!(status.is_none() && !ended);
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    // 能结束的照常结束
    let mut p = long_running();
    let (status, _, ended) = p.kill(KILL_GRACE);
    assert!(status.is_some() && ended);
}

#[test]
fn query_too_long_has_own_message() {
    let sup = |gpu: bool| Supervisor {
        spec: WorkerSpec {
            gpu: gpu.then(|| Gpu {
                description: "NVIDIA GeForce RTX 4080 SUPER".to_string(),
                pci_id: None,
            }),
            ..test_spec()
        },
        limits: Limits::from_params(&BTreeMap::new()),
        proc: Some(long_running()),
        failures: 0,
        dead: None,
        status: Arc::new(Mutex::new(Status::default())),
        with_mmproj: false,
        kills: 0,
    };
    let space = SpaceId("x".to_string());
    let cfg = SpaceConfig::current(Backend::Cpu);
    // 超过帧的上限，没有发出去，与推理进程报告输入无效走同一条路
    let long = "长".repeat(crate::frame::MAX_HEADER);
    let err = sup(false)
        .handle(Input::Query(long.clone()), &space, &cfg)
        .unwrap_err();
    assert!(matches!(err, Error::QueryTooLong(_)), "{err}");
    assert_eq!(err.user_message(), "搜索内容过长，请缩短后再试。");
    // 以图搜图的文字描述过长同样按查询说明
    let image = Input::ImageQuery {
        rgb: rummage_media::Rgb {
            width: 28,
            height: 28,
            data: vec![0; 28 * 28 * 3],
        },
        tokens: 140,
        text: long.clone(),
    };
    // 已加载 mmproj 的推理进程，不必重启
    let mut with_mmproj = sup(false);
    with_mmproj.with_mmproj = true;
    let err = with_mmproj.handle(image, &space, &cfg).unwrap_err();
    assert!(matches!(err, Error::QueryTooLong(_)), "{err}");
    let doc = Input::Document {
        title: None,
        text: long,
    };
    let err = sup(true).handle(doc, &space, &cfg).unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err}");
    assert_eq!(err.user_message(), "此文件的内容无法识别。");
}
