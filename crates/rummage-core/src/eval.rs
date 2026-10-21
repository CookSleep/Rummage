//! 开发用的评测：`rummage-core eval <评测集目录>`，评测集格式见 `docs/eval/FORMAT.md`。
//! 评模态为 `image`、`document`、`audio`、`video` 的子集；每个子集的 `files/<子集>/` 作为一个根目录，在临时数据目录
//! （`--work-dir`，必须是不存在的目录，结束后删除）中用正式的扫描与索引流水线建索引，
//! 再用正式的搜索请求逐条查询：语义两路与文件名、正文通道 RRF 融合，参数取参数表的默认值，`--param 名称=值` 可改
//! （对比参数时用它，不改默认值）。`--subset <名称>`（可重复）只评这几个子集。
//! 两组指标（FORMAT.md）：全部根目录混合检索，以及只在查询所属子集的根目录内检索（根目录过滤在粗排之前生效，
//! 与只索引该子集等价）。相关文件都不能建内容索引的查询不计入，数量单独列出。
//! 指标：Recall@1、Recall@10（前 k 名中有任一相关文件）与 MRR@10（第一个相关文件名次的倒数，前 10 名之外记 0）；
//! 音频与视频的相关时间段只用于文件级判定（FORMAT.md 的文件级指标）。
//! 分数校准：建好索引后先算好背景分布（`State::calibrate`）再跑查询。每条查询取前 100 个结果，
//! 名次指标只看前 10；另记前 100 名中第一个相关文件的校准分数（z）。负样本查询（`assets/calib/negatives.txt`，
//! 评测集中没有相关内容）在混合库中检索，记最高的 z。据此输出匹配度门槛取不同值时，相关文件（全部、排第 1 的）仍在门槛以上
//! 的比例，以及负样本仍有结果在门槛以上的比例（`threshold_table`）。
//! `--out` 给出时把逐条结果写成 CSV，只含查询编号、指标与 z，不含素材内容。
//! 找相似的：每个子集取其查询在子集内检索的第一名作种子（不重复），用它最匹配的片段在混合库中找相似的，
//! 记前 10 个结果中与种子同一子集的比例（P@1、P@10）以及种子本身是否出现（应为 0 次）；另取前 100 个结果，
//! 按与种子同一子集（视为相关）、其他子集（视为不相关）分别统计 z 的分位数，供匹配度的换算锚点参考。
//! 设备（`--device`）：默认按数据目录中的自检记录选择（与核心相同）；`cpu` 只用 CPU；其他值在临时数据目录中只对描述
//! 包含该字符串的显卡和 CPU 重新自检，这块显卡通过检测就指定用它（CPU 更快也不改用）。
//! 运行库（`--runtime`）：默认 `llama`（llama.cpp 与 GGUF）；`litert` 改用 LiteRT-LM 的 CPU 后端与 `.litertlm` 模型
//! （`pipeline::Options::litert`），只用 CPU、忽略 `--device`，图片档位须为 70 或 140（如 `--param image_tokens=140`）。
//! 无界面运行：直接在本进程中创建核心的 `State`，不开管道；暂停条件（全屏、电池）在临时数据库中关闭，不让检测打断评测。
//! 本模块的 `Env`、`start`、`wait_indexed` 也供 `bench` 使用。
mod set;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rummage_core::State;
use rummage_core::pipeline::{Config, Options};
use rummage_embed::selftest::{self, Candidate, Modality};
use rummage_embed::space::{LITERT_MODEL, LITERT_MODEL_DIR, MMPROJ, MODEL, MODEL_DIR, TOKENIZER};
use rummage_embed::{Backend, Limits};
use rummage_index::{Db, kind_of, normalize_root, wants_job};
use rummage_proto::{IndexState, IndexStatus, ParamValue, Reply, Request, SearchHit};

use set::{Query, Subset, load};

/// 运行库、黄金样本与模型的位置（`main` 按开发环境给出）
pub struct Env {
    pub exe: PathBuf,
    pub cpu_runtime: PathBuf,
    pub vulkan_runtime: PathBuf,
    pub golden: PathBuf,
    /// ffmpeg.exe 与 ffprobe.exe 所在的目录
    pub ffmpeg: PathBuf,
    /// 读取模型与自检记录的数据目录
    pub data_dir: PathBuf,
    /// LiteRT-LM 运行库（`litert-lm.dll`）所在的目录
    pub litert_runtime: PathBuf,
}

impl Env {
    pub fn models(&self) -> PathBuf {
        self.data_dir.join("models").join(MODEL_DIR)
    }

    /// LiteRT 模型与分词器所在的目录
    pub fn litert_models(&self) -> PathBuf {
        self.data_dir.join("models").join(LITERT_MODEL_DIR)
    }

    /// 核心在临时数据目录 `work` 中运行时的配置：模型从 `data_dir` 读取，自检记录写在 `work`
    pub fn config(&self, work: &Path) -> Config {
        Config {
            exe: self.exe.clone(),
            cpu_runtime: self.cpu_runtime.clone(),
            vulkan_runtime: self.vulkan_runtime.clone(),
            model: self.models().join(MODEL.name),
            mmproj: self.models().join(MMPROJ.name),
            selftest_cache: work.join("cache").join("selftest.json"),
            golden: self.golden.clone(),
            ffmpeg: self.ffmpeg.clone(),
            embed_tmp: Config::embed_tmp_dir(work),
            gpus: rummage_embed::selftest::gpu_adapters,
            litert: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Device {
    /// 按自检记录
    Auto,
    Cpu,
    /// 描述中包含该字符串的显卡
    Gpu(String),
}

impl Device {
    pub fn parse(s: &str) -> Device {
        if s.eq_ignore_ascii_case("cpu") {
            return Device::Cpu;
        }
        Device::Gpu(s.to_string())
    }
}

#[derive(Debug, PartialEq)]
pub struct Args {
    pub set: PathBuf,
    pub out: Option<PathBuf>,
    pub device: Device,
    pub work: PathBuf,
    /// 为空时评全部子集
    pub subsets: Vec<String>,
    /// 覆盖参数表默认值的参数
    pub params: Vec<(String, ParamValue)>,
    /// 用 LiteRT-LM 的 CPU 后端（见模块说明）
    pub litert: bool,
}

/// 新建工作目录；已存在时报错，避免结束时删掉别的内容
pub fn new_work(work: &Path) -> Result<(), String> {
    if work.exists() {
        return Err(format!(
            "工作目录 \"{}\" 已存在，请换一个不存在的目录。",
            work.display()
        ));
    }
    fs::create_dir_all(work).map_err(|err| format!("无法创建 \"{}\"：{err}", work.display()))
}

/// 删除工作目录；数据库与推理进程可能还要片刻才释放文件，失败时重试
pub fn remove_work(work: &Path) {
    for _ in 0..20 {
        if fs::remove_dir_all(work).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(500));
    }
    emitln!("无法删除工作目录 \"{}\"，请手动删除。", work.display());
}

/// 按 `device` 写出自检记录 `record`（见模块说明），返回要指定的设备（`CandidateReport::device_id`）：只用 CPU 时为 `cpu`、
/// 不写记录；指定显卡时为这块显卡，只要它通过检测就用它，CPU 更快也不改用；按自检记录时为空
pub fn prepare_device(env: &Env, device: &Device, record: &Path) -> Result<Option<String>, String> {
    let name = match device {
        Device::Cpu => return Ok(Some(rummage_proto::CPU_DEVICE.into())),
        Device::Auto => {
            let src = env.data_dir.join("cache").join("selftest.json");
            if let Err(err) = fs::copy(&src, record) {
                emitln!(
                    "没有可用的自检记录（\"{}\"：{err}），只用 CPU。",
                    src.display()
                );
            }
            return Ok(None);
        }
        Device::Gpu(name) => name.to_lowercase(),
    };
    let values = BTreeMap::new();
    let gpus =
        rummage_embed::list_devices(&env.exe, &env.vulkan_runtime, &Limits::from_params(&values))
            .map_err(|err| format!("无法列出显卡：{err}"))?;
    let Some(gpu) = gpus
        .into_iter()
        .find(|g| g.description.to_lowercase().contains(&name))
    else {
        return Err(format!("没有找到描述中包含「{name}」的显卡。"));
    };
    emitln!("自检：CPU 与 {}", gpu.description);
    let opts = selftest::Options {
        exe: env.exe.clone(),
        golden_dir: env.golden.clone(),
        cpu_runtime: env.cpu_runtime.clone(),
        vulkan_runtime: Some(env.vulkan_runtime.clone()),
        model: env.models().join(MODEL.name),
        mmproj: env.models().join(MMPROJ.name),
        cache_file: record.to_path_buf(),
        use_cache: false,
        save: true,
        auto: false,
        cancel: Default::default(),
        images: true,
        only: None,
        video: rummage_core::pipeline::speed_video(&values),
        litert: None,
    };
    let report = selftest::run(
        &opts,
        &[Candidate::Cpu, Candidate::Gpu(gpu)],
        &values,
        &[],
        &mut |_, _| {},
    )
    .map_err(|err| err.to_string())?;
    for c in &report.candidates {
        // 各模态与黄金向量的最小余弦
        let min: Vec<String> = [
            (Modality::Text, "文本"),
            (Modality::Image, "图像"),
            (Modality::Audio, "音频"),
            (Modality::Video, "视频"),
        ]
        .iter()
        .filter_map(|(m, label)| {
            c.samples
                .iter()
                .filter(|s| s.modality == *m)
                .map(|s| s.cosine)
                .reduce(f64::min)
                .map(|v| format!("{label} {v:.5}"))
        })
        .collect();
        emitln!(
            "  {}：{:?}{}{}",
            c.description,
            c.status,
            if min.is_empty() {
                String::new()
            } else {
                format!("，最小余弦 {}", min.join("、"))
            },
            c.reason
                .as_deref()
                .map_or(String::new(), |r| format!("（{r}）"))
        );
    }
    report
        .candidates
        .iter()
        .find(|c| c.backend == Backend::Vulkan && c.status == selftest::Status::Passed)
        .map(|c| Some(c.device_id()))
        .ok_or_else(|| "指定的显卡没有通过自检。".into())
}

/// 把自检记录放进数据目录 `data` 的 `cache\`，核心据此选择设备；记录不存在时不放（用 CPU）
pub fn place_record(record: &Path, data: &Path) -> Result<(), String> {
    let dir = data.join("cache");
    fs::create_dir_all(&dir).map_err(|err| format!("无法创建 \"{}\"：{err}", dir.display()))?;
    if record.exists() {
        fs::copy(record, dir.join("selftest.json"))
            .map_err(|err| format!("无法复制自检记录：{err}"))?;
    }
    Ok(())
}

fn request(state: &Arc<State>, body: Request) -> Result<Reply, String> {
    state.request(body).map_err(|err| err.message)
}

/// 在数据目录 `data` 中建立核心状态：放入自检记录 `record`、加入根目录、关闭暂停条件并设置 `params`，
/// 指定设备 `preferred`（`prepare_device` 的结果），然后启动流水线（`litert` 时用 LiteRT-LM 的 CPU 后端）。
/// 扫描线程由调用方在需要时启动（`start_scanner`）
pub fn start(
    env: &Env,
    preferred: Option<&str>,
    record: &Path,
    data: &Path,
    roots: &[PathBuf],
    params: &[(String, ParamValue)],
    litert: bool,
) -> Result<Arc<State>, String> {
    place_record(record, data)?;
    let state = State::new(String::new(), Db::open(data));
    // 引导记为已完成，流水线不等引导
    request(&state, Request::FinishSetup)?;
    for r in roots {
        request(
            &state,
            Request::AddRoot {
                path: r.to_string_lossy().into_owned(),
                excludes: Vec::new(),
            },
        )?;
    }
    let off = ["pause_on_battery", "pause_on_fullscreen"]
        .map(|k| (k.to_string(), ParamValue::Bool(false)));
    for (key, value) in off.iter().chain(params) {
        request(
            &state,
            Request::SetParam {
                key: key.clone(),
                value: *value,
            },
        )?;
    }
    // 指定显卡时要核对自检记录，先交给核心模型与记录的位置
    let mut cfg = env.config(data);
    if litert {
        cfg.model = env.litert_models().join(LITERT_MODEL.name);
        cfg.mmproj = cfg.model.clone();
        cfg.litert = Some(selftest::LiteRtFiles {
            runtime_dir: env.litert_runtime.clone(),
            tokenizer: env.litert_models().join(TOKENIZER.name),
            cache_dir: data.join("cache").join("litert"),
            npu: None,
        });
    }
    state.configure(cfg.clone());
    if let Some(device) = preferred {
        request(
            &state,
            Request::SetPreferredDevice {
                device: device.into(),
            },
        )?;
    }
    state.start_pipeline(if litert {
        Options::litert(cfg)
    } else {
        Options::local(cfg)
    });
    Ok(state)
}

pub fn status(state: &Arc<State>) -> Result<IndexStatus, String> {
    match request(state, Request::IndexStatus)? {
        Reply::Index(s) => Ok(s),
        other => Err(format!("意外的回复：{other:?}")),
    }
}

/// 等扫描与索引（含按参数重建的文件）全部完成，每 10 秒打印一次进度
pub fn wait_indexed(state: &Arc<State>) -> Result<IndexStatus, String> {
    let mut shown = Instant::now();
    loop {
        thread::sleep(Duration::from_millis(500));
        let s = status(state)?;
        match s.state {
            IndexState::NoModel => return Err("找不到主模型文件。".into()),
            IndexState::NoMmproj => return Err("找不到多模态模型文件（mmproj）。".into()),
            IndexState::Error => return Err(s.error.unwrap_or_default()),
            IndexState::Idle if !state.scan_busy() && s.pending == 0 && s.rebuild.is_none() => {
                return Ok(s);
            }
            _ => {}
        }
        if shown.elapsed() >= Duration::from_secs(10) {
            shown = Instant::now();
            emitln!(
                "  已处理 {}/{}，失败 {}{}",
                s.done + s.failed,
                s.total,
                s.failed,
                if s.paused.is_some() {
                    "（暂停中）"
                } else {
                    ""
                }
            );
        }
    }
}

/// 停止流水线并释放核心状态，推理进程随之结束
pub fn release(state: Arc<State>) {
    state.stop_pipeline();
    drop(state);
}

/// 统一大小写与分隔符后比较路径
fn path_key(p: &str) -> String {
    p.replace('/', "\\").to_lowercase()
}

/// 能建内容索引的文件（图片、文档、音频、视频，以及待判定是否为视频的 `.ts` / `.mts`）
fn indexable(path: &Path) -> bool {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    path.is_file() && wants_job(kind_of(&ext), &ext)
}

/// 一条查询在一种范围内的结果
struct Row<'a> {
    query: &'a Query,
    /// `mixed` 或 `subset`
    scope: &'static str,
    /// 第一个相关文件的名次（从 1 起），前 10 名之外为空
    rank: Option<usize>,
    /// 前 100 名中第一个相关文件的校准分数；不在前 100 名或没有校准分数时为空
    z: Option<f32>,
    took_ms: u64,
}

/// 评测用的负样本查询（模块说明）
const NEGATIVES: &str = include_str!("../../../assets/calib/negatives.txt");
/// 每条查询取的结果数：名次只看前 10，更多的结果用来找相关文件的 z
const DEPTH: u32 = 100;

fn search(
    state: &Arc<State>,
    q: &Query,
    root_ids: Vec<i64>,
) -> Result<(Vec<SearchHit>, u64), String> {
    let body = Request::Search {
        text: Some(q.text.clone()),
        image_path: q.image.as_ref().map(|p| p.to_string_lossy().into_owned()),
        similar: None,
        kinds: Vec::new(),
        root_ids,
        since: None,
        until: None,
        limit: Some(DEPTH),
        snippets: false,
        supersede: false,
    };
    results(state, body)
}

fn results(state: &Arc<State>, body: Request) -> Result<(Vec<SearchHit>, u64), String> {
    match request(state, body)? {
        Reply::Results {
            semantic_unavailable: true,
            unavailable,
            ..
        } => Err(format!("语义检索不可用：{unavailable:?}")),
        Reply::Results { hits, took_ms, .. } => Ok((hits, took_ms)),
        other => Err(format!("意外的回复：{other:?}")),
    }
}

/// 升序排好的数中第 `p` 分位（最近秩）
fn percentile(sorted: &[f32], p: f64) -> Option<f32> {
    if sorted.is_empty() {
        return None;
    }
    let i = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len()) - 1;
    Some(sorted[i])
}

/// 门槛取不同值时：相关文件的 z（`rel`）、排第 1 的相关文件的 z（`first`）不低于门槛的比例，负样本最高 z（`neg`）
/// 不低于门槛的比例
fn threshold_table(rel: &[f32], first: &[f32], neg: &[f32]) -> String {
    let at =
        |v: &[f32], t: f32| v.iter().filter(|z| **z >= t).count() as f64 / v.len().max(1) as f64;
    let mut out = String::from("  门槛    相关文件  排第 1 的相关  负样本\n");
    for i in 0..=24 {
        let t = i as f32 * 0.25;
        let _ = writeln!(
            out,
            "  {t:<6.2}  {:>8.3}  {:>13.3}  {:>6.3}",
            at(rel, t),
            at(first, t),
            at(neg, t)
        );
    }
    out
}

fn summary(rows: &[&Row]) -> String {
    let n = rows.len().max(1) as f64;
    let r1 = rows.iter().filter(|r| r.rank == Some(1)).count() as f64 / n;
    let r10 = rows.iter().filter(|r| r.rank.is_some()).count() as f64 / n;
    let mrr = rows
        .iter()
        .map(|r| r.rank.map_or(0.0, |k| 1.0 / k as f64))
        .sum::<f64>()
        / n;
    let mut took: Vec<u64> = rows.iter().map(|r| r.took_ms).collect();
    took.sort_unstable();
    let median = took.get(took.len() / 2).copied().unwrap_or(0);
    format!(
        "R@1 {r1:.3}  R@10 {r10:.3}  MRR@10 {mrr:.3}  n={}  耗时中位数 {median} 毫秒",
        rows.len()
    )
}

pub fn run(env: &Env, args: &Args) -> Result<(), String> {
    let (subsets, queries) = load(&args.set)?;
    if let Some(name) = args
        .subsets
        .iter()
        .find(|n| !subsets.iter().any(|s| &s.name == *n))
    {
        return Err(format!("评测集中没有子集“{name}”。"));
    }
    let subsets: Vec<Subset> = subsets
        .into_iter()
        .filter(|s| s.dir.is_dir() && (args.subsets.is_empty() || args.subsets.contains(&s.name)))
        .collect();
    if subsets.is_empty() {
        return Err("评测集中没有可评测的子集的语料目录。".into());
    }
    let queries: Vec<Query> = queries
        .into_iter()
        .filter(|q| subsets.iter().any(|s| s.name == q.subset))
        .collect();
    new_work(&args.work)?;
    let res = (|| {
        let record = args.work.join("selftest.json");
        let preferred = if args.litert {
            emitln!("运行库：LiteRT-LM（只用 CPU）");
            Some(rummage_proto::CPU_DEVICE.to_string())
        } else {
            prepare_device(env, &args.device, &record)?
        };
        let roots: Vec<PathBuf> = subsets.iter().map(|s| s.dir.clone()).collect();
        let state = start(
            env,
            preferred.as_deref(),
            &record,
            &args.work.join("data"),
            &roots,
            &args.params,
            args.litert,
        )?;
        let res = evaluate(&state, &subsets, &queries, args);
        release(state);
        res
    })();
    remove_work(&args.work);
    res
}

fn evaluate(
    state: &Arc<State>,
    subsets: &[Subset],
    queries: &[Query],
    args: &Args,
) -> Result<(), String> {
    // 根目录在库中存的是规范化后的路径，按它拼出相关文件的路径与结果比较
    let Reply::Roots { roots } = request(state, Request::ListRoots)? else {
        return Err("无法读取根目录列表。".into());
    };
    let mut bases: Vec<(&Subset, i64, PathBuf)> = Vec::new();
    for s in subsets {
        let norm = normalize_root(&s.dir).map_err(|err| err.to_string())?;
        let Some(r) = roots.iter().find(|r| path_key(&r.path) == path_key(&norm)) else {
            return Err(format!("根目录 \"{norm}\" 没有加入索引。"));
        };
        bases.push((s, r.id, PathBuf::from(norm)));
    }
    // 相关文件在库中的路径；不在任何根目录下或不能建内容索引的不算
    let in_index = |p: &Path| -> Option<String> {
        if !indexable(p) {
            return None;
        }
        bases.iter().find_map(|(s, _, base)| {
            let rest = p.strip_prefix(&s.dir).ok()?;
            Some(path_key(&base.join(rest).to_string_lossy()))
        })
    };

    emitln!(
        "建索引：{} 个根目录（{}）",
        subsets.len(),
        subsets
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("、")
    );
    let t = Instant::now();
    state.start_scanner();
    let s = wait_indexed(state)?;
    let secs = t.elapsed().as_secs_f64();
    emitln!(
        "索引完成：{:.1} 秒，设备 {}",
        secs,
        s.backend.as_deref().unwrap_or("—")
    );
    for (kind, c) in &s.by_kind {
        emitln!("  {kind}：完成 {}，失败 {}", c.done, c.failed);
    }
    let t = Instant::now();
    let n = state.calibrate()?;
    emitln!(
        "分数校准：更新 {n} 组背景分布，{:.1} 秒",
        t.elapsed().as_secs_f64()
    );

    let mut rows = Vec::new();
    // 找相似的种子：（子集, 子集内检索的第一名）
    let mut seeds: Vec<(&str, SearchHit)> = Vec::new();
    let mut skipped = 0;
    for q in queries {
        let relevant: Vec<String> = q.relevant.iter().filter_map(|p| in_index(p)).collect();
        if relevant.is_empty() {
            skipped += 1;
            continue;
        }
        let own = bases
            .iter()
            .find(|(s, _, _)| s.name == q.subset)
            .map(|b| b.1)
            .expect("只保留了有语料目录的子集的查询");
        for (scope, ids) in [("mixed", Vec::new()), ("subset", vec![own])] {
            let (hits, took_ms) = search(state, q, ids)?;
            if scope == "subset"
                && let Some(h) = hits.first()
                && !seeds.iter().any(|(_, s)| s.file_id == h.file_id)
            {
                seeds.push((&q.subset, h.clone()));
            }
            let first = hits
                .iter()
                .position(|h| relevant.contains(&path_key(&h.path)));
            rows.push(Row {
                query: q,
                scope,
                rank: first.filter(|i| *i < 10).map(|i| i + 1),
                z: first.and_then(|i| hits[i].z),
                took_ms,
            });
        }
    }
    emitln!(
        "查询 {} 条（另有 {skipped} 条的相关文件都不能建内容索引，不计入）",
        rows.len() / 2
    );
    for (scope, label) in [("mixed", "混合库"), ("subset", "各子集单独")] {
        let all: Vec<&Row> = rows.iter().filter(|r| r.scope == scope).collect();
        emitln!("{label}：");
        emitln!("  {:<20}{}", "全部", summary(&all));
        for s in subsets {
            let part: Vec<&Row> = all
                .iter()
                .copied()
                .filter(|r| r.query.subset == s.name)
                .collect();
            if !part.is_empty() {
                emitln!("  {:<20}{}", s.name, summary(&part));
            }
        }
    }

    similar(state, subsets, &bases, &seeds)?;

    // 匹配度门槛：混合库中相关文件的 z 与负样本查询的最高 z
    let mut negatives: Vec<(String, Option<f32>)> = Vec::new();
    for (i, text) in NEGATIVES
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .enumerate()
    {
        let q = Query {
            id: format!("negative-{}", i + 1),
            subset: String::new(),
            text: text.to_string(),
            image: None,
            relevant: Vec::new(),
        };
        let (hits, _) = search(state, &q, Vec::new())?;
        let top = hits.iter().filter_map(|h| h.z).reduce(f32::max);
        negatives.push((q.id, top));
    }
    let mixed: Vec<&Row> = rows.iter().filter(|r| r.scope == "mixed").collect();
    let sorted = |mut v: Vec<f32>| {
        v.sort_by(f32::total_cmp);
        v
    };
    let rel = sorted(mixed.iter().filter_map(|r| r.z).collect());
    let first = sorted(
        mixed
            .iter()
            .filter(|r| r.rank == Some(1))
            .filter_map(|r| r.z)
            .collect(),
    );
    let neg = sorted(negatives.iter().filter_map(|n| n.1).collect());
    let show =
        |v: &[f32], p: f64| percentile(v, p).map_or("—".to_string(), |z| format!("{z:.2}"));
    emitln!(
        "校准分数（混合库）：相关文件 {} 条，5% 分位 {}、中位数 {}；排第 1 的相关文件 {} 条，5% 分位 {}、中位数 {}",
        rel.len(),
        show(&rel, 0.05),
        show(&rel, 0.5),
        first.len(),
        show(&first, 0.05),
        show(&first, 0.5)
    );
    emitln!(
        "负样本 {} 条（有 z 的 {}）：最高 z 的中位数 {}、95% 分位 {}",
        negatives.len(),
        neg.len(),
        show(&neg, 0.5),
        show(&neg, 0.95)
    );
    for sub in subsets {
        let v = sorted(
            mixed
                .iter()
                .filter(|r| r.query.subset == sub.name)
                .filter_map(|r| r.z)
                .collect(),
        );
        if !v.is_empty() {
            emitln!(
                "  {:<20}相关文件的 z：5% 分位 {}、中位数 {}",
                sub.name,
                show(&v, 0.05),
                show(&v, 0.5)
            );
        }
    }
    emitln!("{}", threshold_table(&rel, &first, &neg).trim_end());

    if let Some(out) = &args.out {
        let mut csv = String::from("scope,query_id,subset,rank,hit_at_10,rr_at_10,took_ms,z\n");
        let z = |v: Option<f32>| v.map_or(String::new(), |z| format!("{z:.4}"));
        for r in &rows {
            let _ = writeln!(
                csv,
                "{},{},{},{},{},{:.4},{},{}",
                r.scope,
                r.query.id,
                r.query.subset,
                r.rank.map_or(String::new(), |k| k.to_string()),
                u8::from(r.rank.is_some()),
                r.rank.map_or(0.0, |k| 1.0 / k as f64),
                r.took_ms,
                z(r.z)
            );
        }
        for (id, top) in &negatives {
            let _ = writeln!(csv, "negative,{id},,,0,0.0000,0,{}", z(*top));
        }
        if let Some(dir) = out.parent() {
            fs::create_dir_all(dir)
                .map_err(|err| format!("无法创建 \"{}\"：{err}", dir.display()))?;
        }
        fs::write(out, csv).map_err(|err| format!("无法写入 \"{}\"：{err}", out.display()))?;
        emitln!("逐条结果：{}", out.display());
    }
    Ok(())
}

/// z 的 10%、50%、90% 分位，`v` 须已升序排好
fn spread(v: &[f32]) -> String {
    let show = |p: f64| percentile(v, p).map_or("—".to_string(), |z| format!("{z:.2}"));
    format!(
        "10% {}、中位数 {}、90% {}（{} 条）",
        show(0.1),
        show(0.5),
        show(0.9),
        v.len()
    )
}

/// 找相似的（见模块说明）：按子集输出 P@1、P@10、种子本身出现的次数与耗时中位数，以及同子集、其他子集结果的 z 分位数
fn similar(
    state: &Arc<State>,
    subsets: &[Subset],
    bases: &[(&Subset, i64, PathBuf)],
    seeds: &[(&str, SearchHit)],
) -> Result<(), String> {
    // 结果所在的子集，按根目录路径判断
    let subset_of = |path: &str| {
        let key = path_key(path);
        bases
            .iter()
            .find(|(_, _, base)| key.starts_with(&(path_key(&base.to_string_lossy()) + "\\")))
            .map(|(s, _, _)| s.name.as_str())
    };
    emitln!("找相似的（混合库，种子为各查询在子集内的第一名）：");
    let (mut all_same, mut all_other, mut all_top) = (Vec::new(), Vec::new(), Vec::new());
    for sub in subsets {
        let (mut p1, mut p10, mut n, mut own, mut took) = (0.0, 0.0, 0, 0, Vec::new());
        let (mut same_z, mut other_z, mut top_z) = (Vec::new(), Vec::new(), Vec::new());
        for (_, seed) in seeds.iter().filter(|(s, _)| *s == sub.name) {
            let body = Request::Search {
                text: None,
                image_path: None,
                similar: Some(rummage_proto::SimilarTo {
                    file_id: seed.file_id,
                    segment_id: seed.segments.first().map(|s| s.segment_id),
                }),
                kinds: Vec::new(),
                root_ids: Vec::new(),
                since: None,
                until: None,
                limit: Some(DEPTH),
                snippets: false,
                supersede: false,
            };
            let (hits, ms) = results(state, body)?;
            took.push(ms);
            n += 1;
            own += hits.iter().filter(|h| h.file_id == seed.file_id).count();
            let same = |h: &SearchHit| subset_of(&h.path) == Some(sub.name.as_str());
            p1 += f64::from(u8::from(hits.first().is_some_and(same)));
            p10 += hits.iter().take(10).filter(|h| same(h)).count() as f64 / 10.0;
            top_z.extend(hits.first().and_then(|h| h.z));
            for h in &hits {
                let Some(z) = h.z else { continue };
                if same(h) {
                    same_z.push(z);
                } else {
                    other_z.push(z);
                }
            }
        }
        if n == 0 {
            continue;
        }
        took.sort_unstable();
        emitln!(
            "  {:<20}P@1 {:.3}  P@10 {:.3}  n={n}  种子本身出现 {own} 次  耗时中位数 {} 毫秒",
            sub.name,
            p1 / n as f64,
            p10 / n as f64,
            took[took.len() / 2]
        );
        for v in [&mut same_z, &mut other_z, &mut top_z] {
            v.sort_by(f32::total_cmp);
        }
        emitln!("    z：同子集 {}", spread(&same_z));
        emitln!("       其他子集 {}", spread(&other_z));
        emitln!("       第 1 名 {}", spread(&top_z));
        all_same.extend(same_z);
        all_other.extend(other_z);
        all_top.extend(top_z);
    }
    for v in [&mut all_same, &mut all_other, &mut all_top] {
        v.sort_by(f32::total_cmp);
    }
    emitln!("  全部 z：同子集 {}", spread(&all_same));
    emitln!("         其他子集 {}", spread(&all_other));
    emitln!("         第 1 名 {}", spread(&all_top));
    Ok(())
}
