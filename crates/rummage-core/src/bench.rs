//! 开发用的性能测试：`rummage-core bench`。工作目录（`--work-dir`）必须不存在，结束后删除。
//! 依次测：
//! 1. 建索引吞吐（给出 `--samples` 时）：在样本目录的真实图片上，先逐条计算已预处理图片的向量得到单条推理延迟
//!    （理论吞吐 = 1 / 中位数），再用正式的扫描与索引流水线（预处理与推理并行）为整个目录建索引，比较两者的吞吐。
//!    只统计图片：建索引的主要开销在图片上。
//! 2. 合成索引：按 `--vectors` 条向量（默认 30 万）直接写入数据库，三分之一为图片（每个文件 1 条），其余为文档
//!    （每个文件 `DOC_SEGMENTS` 条）。向量是聚类分布：每 `CLUSTER` 条左右共用一个随机中心，加独立高斯噪声后归一化，
//!    与中心的余弦约 0.75；查询向量按同样方式生成。根目录指向不存在的路径，核心启动时标为离线、保留索引。
//! 3. 用合成索引启动真正的核心进程（`rummage-core --data-dir`，模型以硬链接放进数据目录）：
//!    - 冷启动：从启动进程到第一次搜索有回复，以及到第一次语义搜索成功。推理引擎建好后第一次文本搜索会等推理进程启动、
//!      加载模型，所以两者通常相同，都含模型加载；
//!    - 搜索延迟：`--queries` 条互不相同的文本查询经管道逐条发出，往返时间含查询编码、粗排、精排、文件名通道与融合；
//!    - 空闲内存：核心进程的工作集与专用提交，分别在推理进程加载时、空闲卸载后（`engine_idle_min` 改为 1 分钟再等待）测。
//!      推理在核心的子进程中，不计入核心进程的内存。界面进程的内存不在这里测，需要另外测。
//! 4. 粗排余量复核：核心退出后在本进程载入合成索引，对图片、文档各 20 条合成查询，比较正式检索的前 10 个文件与
//!    768 维 f32 暴力检索（文件分数取各条最大值）的前 10 个文件的重合率。
//!
//! 设备（`--device`）同 `eval`。
use std::collections::{BTreeMap, HashMap};
use std::f64::consts::PI;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use rummage_core::client::{self, Client};
use rummage_core::pipeline::space_row;
use rummage_embed::space::{MMPROJ, MODEL, MODEL_DIR};
use rummage_embed::{Backend, Input, SpaceConfig};
use rummage_index::{Db, Filter, Query, SearchParams, VIndex, kind_of, quantize, vec_to_blob};
use rummage_proto::{ParamValue, Reply, Request, params};

use crate::eval::{self, Device, Env};

const DIM: usize = 768;
/// 合成文档每个文件的片段数
const DOC_SEGMENTS: usize = 10;
/// 平均每个聚类中心的向量数
const CLUSTER: usize = 150;
/// 噪声每维的标准差：与中心的余弦约为 1 / sqrt(1 + DIM·σ²) ≈ 0.75
const SIGMA: f32 = 0.032;
const SEED: u64 = 0x5275_6d6d_6167_6521;
/// 粗排余量复核每种文件的查询数
const CHECK_QUERIES: usize = 20;

#[derive(Debug, PartialEq)]
pub struct Args {
    pub vectors: usize,
    pub queries: usize,
    pub samples: Option<PathBuf>,
    pub device: Device,
    pub work: PathBuf,
}

/// SplitMix64：合成数据只需可复现，不需要密码学强度
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// (0, 1) 上的均匀分布
    fn unit(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }

    /// 标准正态分布（Box–Muller）
    fn normal(&mut self) -> f32 {
        let (u, v) = (self.unit(), self.unit());
        ((-2.0 * u.ln()).sqrt() * (2.0 * PI * v).cos()) as f32
    }
}

fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    for x in v {
        *x /= n;
    }
}

/// 合成数据的生成规则；同样的参数总生成同样的向量，粗排余量复核时据此重新生成
struct Synth {
    centers: Vec<Vec<f32>>,
    images: usize,
    vectors: usize,
}

impl Synth {
    fn new(vectors: usize) -> Synth {
        let mut r = Rng(SEED);
        let centers = (0..(vectors / CLUSTER).max(1))
            .map(|_| {
                let mut c: Vec<f32> = (0..DIM).map(|_| r.normal()).collect();
                normalize(&mut c);
                c
            })
            .collect();
        Synth {
            centers,
            images: vectors / 3,
            vectors,
        }
    }

    /// 中心 `c` 附近的一条向量，`key` 决定噪声
    fn near(&self, c: usize, key: u64) -> Vec<f32> {
        let mut r = Rng(key);
        let mut v: Vec<f32> = self.centers[c]
            .iter()
            .map(|x| x + SIGMA * r.normal())
            .collect();
        normalize(&mut v);
        v
    }

    /// 第 `i` 条库向量
    fn vector(&self, i: usize) -> Vec<f32> {
        let key = SEED ^ (i as u64 + 1).wrapping_mul(0x2545_f491_4f6c_dd1d);
        let c = Rng(key).next() as usize % self.centers.len();
        self.near(c, key.rotate_left(17))
    }

    /// 第 `i` 条向量所属文件的序号（从 0 起，图片在前）
    fn file(&self, i: usize) -> usize {
        if i < self.images {
            return i;
        }
        self.images + (i - self.images) / DOC_SEGMENTS
    }

    fn files(&self) -> usize {
        self.file(self.vectors - 1) + 1
    }
}

fn median_p95(mut v: Vec<f64>) -> (f64, f64) {
    v.sort_by(f64::total_cmp);
    let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
    (at(0.5), at(0.95))
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0
}

pub fn run(env: &Env, args: &Args) -> Result<(), String> {
    if args.vectors == 0 || args.queries == 0 {
        return Err("--vectors 与 --queries 都应大于 0。".into());
    }
    eval::new_work(&args.work)?;
    let res = (|| {
        let record = args.work.join("selftest.json");
        let preferred = eval::prepare_device(env, &args.device, &record)?;
        if let Some(samples) = &args.samples {
            throughput(env, args, &record, preferred.as_deref(), samples)?;
        }
        let data = args.work.join("synthetic");
        let synth = Synth::new(args.vectors);
        generate(&synth, &data, preferred.as_deref())?;
        eval::place_record(&record, &data)?;
        serve(env, args, &data)?;
        check(&synth, &data)
    })();
    eval::remove_work(&args.work);
    res
}

fn images_in(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            images_in(&p, out);
            continue;
        }
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if kind_of(&ext) == "image" {
            out.push(p);
        }
    }
}

fn throughput(
    env: &Env,
    args: &Args,
    record: &Path,
    preferred: Option<&str>,
    samples: &Path,
) -> Result<(), String> {
    let mut images = Vec::new();
    images_in(samples, &mut images);
    images.sort();
    if images.is_empty() {
        return Err(format!("\"{}\" 中没有图片。", samples.display()));
    }
    emitln!(
        "建索引吞吐：{} 张图片（\"{}\"）",
        images.len(),
        samples.display()
    );
    let state = eval::start(
        env,
        preferred,
        record,
        &args.work.join("throughput"),
        &[samples.to_path_buf()],
        &[],
        false,
    )?;
    let res = (|| {
        let p = state.pipeline().ok_or("索引流水线没有启动。")?;
        let t = Instant::now();
        let mut engine = loop {
            if let Some(e) = p.query_engine() {
                break e;
            }
            if t.elapsed() > Duration::from_secs(60) {
                return Err("推理引擎 60 秒内没有就绪。".to_string());
            }
            thread::sleep(Duration::from_millis(100));
        };
        let values = state.params();
        let max_pixels = params::max_image_pixels(&values);
        let tier = params::int(&values, "image_tokens") as u32;
        // 单条推理：前 100 张已预处理的图片逐条计算，第一张加载 mmproj，不计时
        let mut ms = Vec::new();
        for (i, path) in images.iter().take(101).enumerate() {
            let rgb = rummage_media::prepare(path, tier, max_pixels)
                .map_err(|err| format!("\"{}\"：{err}", path.display()))?;
            let input = Input::Image { rgb, tokens: tier };
            let t = Instant::now();
            p.embed_query(&mut engine, input)
                .map_err(|err| format!("推理失败：{err}"))?;
            if i > 0 {
                ms.push(t.elapsed().as_secs_f64() * 1000.0);
            }
        }
        drop(engine);
        let (single, single_p95) = median_p95(ms);
        let ideal = 1000.0 / single;
        emitln!(
            "  单条推理：中位数 {single:.1} 毫秒，P95 {single_p95:.1} 毫秒，理论吞吐 {ideal:.1} 张/秒"
        );

        let t = Instant::now();
        state.start_scanner();
        let s = eval::wait_indexed(&state)?;
        let secs = t.elapsed().as_secs_f64();
        let done = s.by_kind.get("image").map_or(0, |c| c.done);
        let rate = done as f64 / secs;
        emitln!(
            "  流水线：{done} 张，失败 {}，{secs:.1} 秒（含扫描），{rate:.1} 张/秒，为理论吞吐的 {:.0}%（设备 {}）",
            s.failed,
            rate / ideal * 100.0,
            s.backend.as_deref().unwrap_or("—")
        );
        Ok(())
    })();
    eval::release(state);
    res
}

/// 把合成数据写进 `data` 中新建的索引数据库
fn generate(synth: &Synth, data: &Path, preferred: Option<&str>) -> Result<(), String> {
    emitln!(
        "合成索引：{} 条向量，{} 个文件（图片 {}）",
        synth.vectors,
        synth.files(),
        synth.images
    );
    let t = Instant::now();
    fs::create_dir_all(data).map_err(|err| format!("无法创建 \"{}\"：{err}", data.display()))?;
    let tier = params::int(&params::defaults(), "image_tokens") as u32;
    let cfg = SpaceConfig::current(Backend::Cpu);
    let (text, image) = (space_row(&cfg, None), space_row(&cfg, Some(tier)));
    {
        let db = Db::open(data).map_err(|err| err.to_string())?;
        for s in [&text, &image] {
            db.insert_space(s).map_err(|err| err.to_string())?;
        }
        if let Some(device) = preferred {
            db.meta_set(rummage_core::pipeline::PREFERRED_DEVICE_KEY, device)
                .map_err(|err| err.to_string())?;
        }
    }
    let sql = |err: rusqlite::Error| format!("写入合成数据时出错：{err}");
    let mut conn = rusqlite::Connection::open(data.join("index.db")).map_err(sql)?;
    let tx = conn.transaction().map_err(sql)?;
    // 不存在的根目录：核心启动时标为离线，保留索引
    let root = data.join("synthetic-root");
    tx.execute(
        "INSERT INTO roots (path, settings_json, settings_version, added_at) VALUES (?1, '{}', 1, 0)",
        [root.to_string_lossy()],
    )
    .map_err(sql)?;
    let root_id = tx.last_insert_rowid();
    {
        let mut file = tx
            .prepare(
                "INSERT INTO files (root_id, rel_path, name, ext, kind, size, mtime, status, updated_at, width, height)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'stored', 0, ?8, ?9)",
            )
            .map_err(sql)?;
        let mut segment = tx
            .prepare(
                "INSERT INTO segments (file_id, modality, locator_json, created_at) VALUES (?1, ?2, '{}', 0)",
            )
            .map_err(sql)?;
        let mut vector = tx
            .prepare(
                "INSERT INTO vectors (segment_id, space_id, dim, vec, params_version, coarse)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .map_err(sql)?;
        let mut file_id = 0;
        let mut last = usize::MAX;
        for i in 0..synth.vectors {
            let f = synth.file(i);
            let img = i < synth.images;
            if f != last {
                last = f;
                let (name, ext, kind, size) = if img {
                    (format!("IMG_{f:06}.jpg"), "jpg", "image", Some(1024))
                } else {
                    (format!("笔记_{f:06}.md"), "md", "document", None)
                };
                // 修改时间按序号铺开在约 3 年里，时间筛选与排序有真实的分布
                let mtime = 1_700_000_000_000 + f as i64 * 300_000;
                file.execute(rusqlite::params![
                    root_id,
                    format!("{kind}\\{name}"),
                    name,
                    ext,
                    kind,
                    200_000,
                    mtime,
                    size,
                    size
                ])
                .map_err(sql)?;
                file_id = tx.last_insert_rowid();
            }
            segment
                .execute(rusqlite::params![
                    file_id,
                    if img { "image" } else { "text" }
                ])
                .map_err(sql)?;
            let seg = tx.last_insert_rowid();
            let v = synth.vector(i);
            let space = if img { &image } else { &text };
            vector
                .execute(rusqlite::params![
                    seg,
                    space.id,
                    DIM as i64,
                    vec_to_blob(&v),
                    space.preprocess,
                    quantize(&rummage_embed::vector::truncate(&v))
                ])
                .map_err(sql)?;
        }
    }
    tx.commit().map_err(sql)?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(sql)?;
    drop(conn);
    let size = fs::metadata(data.join("index.db")).map_or(0, |m| m.len());
    emitln!(
        "  写入用时 {:.1} 秒，index.db {:.0} MB",
        t.elapsed().as_secs_f64(),
        mb(size)
    );
    Ok(())
}

/// 互不相同的中文查询；数量超过组合数时加编号
fn query_text(i: usize) -> String {
    const A: [&str; 10] = [
        "海边的日落",
        "上周的会议纪要",
        "猫在沙发上睡觉",
        "发票截图",
        "去年夏天的旅行照片",
        "合同里的违约条款",
        "孩子的生日聚会",
        "项目进度表",
        "雪山和湖泊",
        "报销流程说明",
    ];
    const B: [&str; 10] = [
        "",
        "的照片",
        "相关的文档",
        "截图",
        "在哪里",
        "最新版本",
        "手机拍的",
        "整理的笔记",
        "原图",
        "那张",
    ];
    let n = A.len() * B.len();
    let base = format!("{}{}", A[i % A.len()], B[(i / A.len()) % B.len()]);
    if i < n {
        return base;
    }
    format!("{base} {}", i / n)
}

fn search(text: String) -> Request {
    Request::Search {
        text: Some(text),
        image_path: None,
        similar: None,
        kinds: Vec::new(),
        root_ids: Vec::new(),
        since: None,
        until: None,
        limit: None,
        snippets: false,
        supersede: false,
    }
}

/// 发出请求并取回复；`id` 每次加一
async fn ask(client: &mut Client, id: &mut u64, body: Request) -> Result<Reply, String> {
    *id += 1;
    client::request(client, *id, body)
        .await
        .map_err(|err| format!("与核心进程通信失败：{err}"))?
        .map_err(|err| err.message)
}

fn serve(env: &Env, args: &Args, data: &Path) -> Result<(), String> {
    // 核心从 <数据目录>\models 读模型：用硬链接，不复制也不改动原文件
    let models = data.join("models").join(MODEL_DIR);
    fs::create_dir_all(&models)
        .map_err(|err| format!("无法创建 \"{}\"：{err}", models.display()))?;
    for f in [MODEL.name, MMPROJ.name] {
        fs::hard_link(env.models().join(f), models.join(f)).map_err(|err| {
            format!("无法为 \"{f}\" 建立硬链接（工作目录须与模型在同一个磁盘上）：{err}")
        })?;
    }
    let rt = tokio::runtime::Runtime::new().map_err(|err| format!("无法启动异步运行时：{err}"))?;
    emitln!("启动核心进程（合成索引）");
    let t = Instant::now();
    let mut child = Command::new(&env.exe)
        .arg("--data-dir")
        .arg(data)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("无法启动核心进程：{err}"))?;
    let res = rt.block_on(measure(args, data, t));
    if res.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    res
}

async fn measure(args: &Args, data: &Path, t: Instant) -> Result<(), String> {
    let pipe = rummage_core::pipe_name(data).map_err(|err| err.to_string())?;
    let mut client = loop {
        if t.elapsed() > Duration::from_secs(60) {
            return Err("核心进程 60 秒内没有就绪。".into());
        }
        if let Ok(token) = rummage_core::read_token(data)
            && let Ok(c) = client::connect(&pipe, &token).await
        {
            break c;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let mut id = 0;
    // 全新的数据目录记为未完成引导，不建内容索引（与评测相同）
    ask(&mut client, &mut id, Request::FinishSetup).await?;
    let mut first = None;
    let semantic = loop {
        if t.elapsed() > Duration::from_secs(120) {
            return Err("语义检索 120 秒内没有就绪。".into());
        }
        let reply = ask(&mut client, &mut id, search("冷启动".into())).await?;
        first.get_or_insert(t.elapsed());
        if let Reply::Results {
            semantic_unavailable: false,
            ..
        } = reply
        {
            break t.elapsed();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let first = first.expect("循环至少收到一次回复");
    emitln!(
        "  冷启动：第一次搜索有回复 {:.2} 秒，第一次语义搜索成功 {:.2} 秒",
        first.as_secs_f64(),
        semantic.as_secs_f64()
    );

    let (mut rtt, mut took) = (Vec::new(), Vec::new());
    for i in 0..args.queries {
        let t = Instant::now();
        let reply = ask(&mut client, &mut id, search(query_text(i))).await?;
        rtt.push(t.elapsed().as_secs_f64() * 1000.0);
        match reply {
            Reply::Results {
                semantic_unavailable: false,
                took_ms,
                ..
            } => took.push(took_ms as f64),
            _ => return Err("搜索时语义检索不可用。".into()),
        }
    }
    let (m, p) = median_p95(rtt);
    let (tm, tp) = median_p95(took);
    emitln!(
        "  搜索延迟（{} 条，经管道往返）：中位数 {m:.1} 毫秒，P95 {p:.1} 毫秒；核心内耗时中位数 {tm:.0} 毫秒，P95 {tp:.0} 毫秒",
        args.queries
    );

    let pid = client.server_pid;
    let mem = |label: &str| -> Result<(), String> {
        let (ws, private) = rummage_core::process_memory(pid)
            .map_err(|err| format!("无法读取核心进程的内存：{err}"))?;
        emitln!(
            "  核心进程内存（{label}）：工作集 {:.0} MB，专用提交 {:.0} MB",
            mb(ws),
            mb(private)
        );
        Ok(())
    };
    mem("推理进程已加载")?;
    ask(
        &mut client,
        &mut id,
        Request::SetParam {
            key: "engine_idle_min".into(),
            value: ParamValue::Int(1),
        },
    )
    .await?;
    emitln!("  等推理进程空闲卸载（约 75 秒）");
    tokio::time::sleep(Duration::from_secs(75)).await;
    mem("推理进程已卸载")?;
    let exited = client::shutdown(client)
        .await
        .map_err(|err| format!("请核心进程退出时出错：{err}"))?;
    if !exited {
        return Err("核心进程 10 秒内没有退出。".into());
    }
    Ok(())
}

/// 粗排余量复核（见模块说明）
fn check(synth: &Synth, data: &Path) -> Result<(), String> {
    let db = Db::open(data).map_err(|err| err.to_string())?;
    let vi = VIndex::load(&db).map_err(|err| err.to_string())?;
    let (db, vi) = (Mutex::new(db), Mutex::new(vi));
    let space = space_row(&SpaceConfig::current(Backend::Cpu), None);
    let values = params::defaults();
    let (int, float) = (
        |key: &str| params::int(&values, key),
        |key: &str| params::float(&values, key),
    );
    let sp = SearchParams {
        candidates: int("semantic_candidates") as usize,
        limit: 10,
        rrf_k: int("rrf_k") as f64,
        weight_semantic: float("weight_semantic"),
        weight_name: float("weight_name"),
        weight_text: float("weight_text"),
        video_windows: int("video_top_windows") as usize,
        video_gap_ms: int("video_merge_gap_ms") as u64,
        min_relevance: float("min_relevance"),
    };
    // 每种文件 CHECK_QUERIES 条查询，取不同的中心
    let mut r = Rng(SEED.rotate_left(7));
    let queries: Vec<Vec<f32>> = (0..2 * CHECK_QUERIES)
        .map(|j| {
            let c = r.next() as usize % synth.centers.len();
            synth.near(c, SEED ^ (j as u64 + 1) << 40)
        })
        .collect();
    // 暴力检索：每个查询对每个文件取各条向量的最高余弦
    let mut best: Vec<HashMap<usize, f32>> = vec![HashMap::new(); queries.len()];
    for i in 0..synth.vectors {
        let img = i < synth.images;
        let v = synth.vector(i);
        let range = if img {
            0..CHECK_QUERIES
        } else {
            CHECK_QUERIES..2 * CHECK_QUERIES
        };
        for j in range {
            let cos: f32 = queries[j].iter().zip(&v).map(|(a, b)| a * b).sum();
            let e = best[j].entry(synth.file(i)).or_insert(f32::MIN);
            *e = e.max(cos);
        }
    }
    let mut out: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for (j, q) in queries.iter().enumerate() {
        let kind = if j < CHECK_QUERIES {
            "image"
        } else {
            "document"
        };
        let mut exact: Vec<(usize, f32)> = best[j].iter().map(|(&f, &c)| (f, c)).collect();
        exact.sort_by(|a, b| b.1.total_cmp(&a.1));
        // 文件按序号写入，id 从 1 起
        let exact: Vec<i64> = exact.iter().take(10).map(|(f, _)| *f as i64 + 1).collect();
        let filter = Filter {
            kinds: vec![kind.to_string()],
            ..Filter::default()
        };
        let coarse = rummage_embed::vector::truncate(q);
        let query = Query {
            vector: Some((q, &space)),
            coarse: &coarse,
            kind: rummage_index::QueryKind::Text,
            modality: None,
            text: None,
            filter: &filter,
            params: sp,
            cancel: None,
        };
        let hits = rummage_index::search(&db, &vi, &query).map_err(|err| err.to_string())?;
        let same = hits.iter().filter(|h| exact.contains(&h.file_id)).count();
        out.entry(kind).or_default().push(same as f64 / 10.0);
    }
    emitln!(
        "粗排余量复核（{} 条向量，粗排取前 {} 名）：",
        synth.vectors,
        sp.candidates
    );
    for (kind, v) in out {
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        let min = v.iter().copied().fold(1.0, f64::min);
        emitln!(
            "  {kind}：前 10 重合平均 {:.1}%，最差 {:.0}/10",
            mean * 100.0,
            min * 10.0
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synth_is_reproducible() {
        let s = Synth::new(3000);
        assert_eq!(s.vector(5), Synth::new(3000).vector(5));
        assert_ne!(s.vector(5), s.vector(6));
        let n: f32 = s.vector(7).iter().map(|x| x * x).sum();
        assert!((n - 1.0).abs() < 1e-4);
        // 图片在前，每个文件 1 条；之后每个文档 DOC_SEGMENTS 条
        assert_eq!(s.images, 1000);
        assert_eq!(s.file(999), 999);
        assert_eq!(s.file(1000), 1000);
        assert_eq!(s.file(1000 + DOC_SEGMENTS), 1001);
        assert_eq!(s.files(), 1000 + 2000 / DOC_SEGMENTS);
    }

    #[test]
    fn queries_are_distinct() {
        let q: std::collections::HashSet<String> = (0..250).map(query_text).collect();
        assert_eq!(q.len(), 250);
    }
}
