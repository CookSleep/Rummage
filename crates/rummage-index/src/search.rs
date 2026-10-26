//! 检索：语义、文件名、正文三个通道各得到按文件排好的列表，用 RRF 融合。
//! - 语义通道按模态分别处理：过滤条件与可比较的空间（`comparable`）先筛出候选，内存索引做 int8 粗排取前
//!   `candidates` 名，再从库中读 768 维 f16 向量精排（余弦）；按文件聚合，取最高分并保留该文件命中的全部片段。
//!   文本查询检索文本、图片、音频与视频（带音频的 `video_av`、纯画面的 `video_v` 两个子空间分开排名），
//!   以图搜图只检索图片。视频只保留 z 最高的 `video_windows` 个窗口，相邻的合并成一段（`merge_windows`）。
//! - 分数校准（`calib`）：按查询类型（`Query::kind`）与候选所在“空间 × 模态”的背景分布，把余弦换成 z 分数，
//!   只用于匹配度门槛与界面上的匹配度，同一文件的片段按 z 排序；某个模态的候选中有空间还没有背景分布时，其中的文件没有 z。
//!   以图搜图与“找相似的”改用本次查询自身的分数分布（粗排时统计，`VIndex::coarse`），该空间向量太少时才退回背景分布。
//!   排序不用 z：每个模态按余弦单独排名、各作为一个列表参加融合。评测中按 z 跨模态合成一个列表
//!   会让相关时 z 幅度大的视频、文档压过照片与音频，混合库 R@1 与 MRR 不如按模态交替。
//! - 文档的内嵌媒体（种类为文档的图片、音视频片段）：文本查询时不进各自模态的列表，而是并入文档列表，每份文档按正文与
//!   内嵌片段中较高的 z 排名（`doc_list`）；同一文档只占一个名次，不会因为同时出现在正文与图片两个列表而在 RRF 中重复得分。
//!   依据评测（MMLongBench-Doc 863 条英文问题）：内嵌片段单独参加图片列表时 R@1 0.477 → 0.321，
//!   并入文档列表后为 0.641（R@10 0.604 → 0.798）；降低内嵌列表的权重或只让 z 达到门槛的参加都不如合并。
//!   文档列表按 z 排名（多个文本空间时与按余弦排名不同），依据同上。
//!   正文还没校准，或某个模态的内嵌片段还没有 z 时（小库、首次建立索引期间），这些内嵌片段照旧留在所属模态的列表中，
//!   只靠内嵌媒体命中的文档（扫描件）仍能搜到。以图搜图与“找相似的”只检索一个模态，内嵌图片照旧与独立图片一起排名。
//! - 文件名通道：查询按空白拆成若干词，全部命中才算。每个词都不少于 3 个字符时用 `fts_names` 的 trigram 匹配
//!   （每个词一个短语，只匹配 `name` 列，按 bm25 排序），否则退回每个词一个 `files.name LIKE`（按修改时间从新到旧逐个文件查找，
//!   取够即停）；同样应用过滤条件，最多取 200 条。
//! - 正文通道：同样的规则作用于片段正文（`fts_text` / `segments.text LIKE`），按文件聚合：文件的名次取它最好的片段，
//!   并按匹配度记下全部命中片段；最多取 200 个文件。LIKE 按修改时间从新到旧逐个文件查找，取够即停，
//!   常见的短词不必扫完全部片段。只有 1 个字符的查询不走正文通道。以图搜图时没有文字查询，
//!   文件名与正文通道都不参加。
//! - 取代：带 `cancel` 的检索在之后的同源搜索到来时中止（SQLite 的进度回调，从精排起），回 `DbError::Cancelled`；
//!   界面边输入边搜索时，慢的旧查询不会一直占着数据库让新查询排队。
//! - 融合：`score = Σ w / (k + rank)`，rank 从 1 起；各模态的语义列表、文件名、正文的权重分别为
//!   `weight_semantic`、`weight_name`、`weight_text`。同分时依次比较最高余弦、文件 id。
//! - 匹配度门槛：最高 z 低于 `min_relevance`、文件名与正文都没有命中的结果标为 `low`，排在其余结果之后
//!   （各自保持融合的顺序），再截取前 `limit` 个；没有 z 的结果（只有关键词命中、所在模态还没校准）不判定。
//!   只用于文字查询：以图搜图与“找相似的”（`QueryKind::Similar`）不判定，按融合的顺序排列。
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rusqlite::types::Value;
use rusqlite::{OptionalExtension, params, params_from_iter};

use crate::calib::{self, QueryKind, Stats};
use crate::db::{Db, DbError, Space, blob_to_vec};
use crate::lock;
use crate::vindex::VIndex;

/// 文件名与正文通道最多取的文件数
const KEYWORD_LIMIT: usize = 200;

/// 为空的列表表示不限
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filter {
    pub kinds: Vec<String>,
    /// 搜索范围，取各项的并集：(根目录 id, 相对根目录的路径)，路径为空表示整个根目录，
    /// 否则用本机分隔符、前后不带分隔符（`rummage_proto::FolderScope`）
    pub folders: Vec<(i64, String)>,
    /// 修改时间的起止（Unix 毫秒，含两端）
    pub since: Option<i64>,
    pub until: Option<i64>,
    /// 不出现在结果中的文件（“找相似的”中被拿来查的文件）
    pub exclude_file: Option<i64>,
}

/// 取自参数表
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchParams {
    pub candidates: usize,
    pub limit: usize,
    pub rrf_k: f64,
    pub weight_semantic: f64,
    pub weight_name: f64,
    pub weight_text: f64,
    /// 每个视频保留的命中窗口数（`video_top_windows`）
    pub video_windows: usize,
    /// 相邻命中窗口合并的最大间隔（`video_merge_gap_ms`）
    pub video_gap_ms: u64,
    /// 匹配度门槛，z 分数（`min_relevance`）
    pub min_relevance: f64,
}

/// 搜索被取代的判定：`latest` 是同一来源最新一次可取代的搜索的序号，与本次的 `ticket` 不同时本次已被取代
#[derive(Debug, Clone)]
pub struct Cancel {
    pub latest: Arc<AtomicU64>,
    pub ticket: u64,
}

/// 检索期间装在连接上的进度回调，被取代时让正在执行的语句中止；离开时卸下，连接交还给其他用途
struct Interruptible<'a>(&'a Db);

impl Cancel {
    fn superseded(&self) -> bool {
        self.latest.load(AtomicOrdering::Relaxed) != self.ticket
    }
}

impl Drop for Interruptible<'_> {
    fn drop(&mut self) {
        if let Err(err) = self.0.conn.progress_handler(0, None::<fn() -> bool>) {
            log::warn!("无法卸下搜索的进度回调：{err}");
        }
    }
}

pub struct Query<'a> {
    /// 已归一化的查询向量及其空间：文本查询为文本空间，以图搜图为图像空间，“找相似的”为被拿来查的向量所在的空间
    pub vector: Option<(&'a [f32], &'a Space)>,
    /// `vector` 的粗排向量（`rummage_embed::vector::truncate` 的结果）；没有 `vector` 时不用
    pub coarse: &'a [f32],
    /// 查询类型，决定用哪一组背景分布：文字查询为 `Text`，以图搜图与“找相似的”为 `Similar`
    pub kind: QueryKind,
    /// 只检索这一模态（“找相似的”：与被拿来查的片段同一模态）；为空时按查询空间决定（`modalities`）
    pub modality: Option<&'a str>,
    /// 文件名与正文通道的文字查询，已去掉首尾空白；以图搜图时为空
    pub text: Option<&'a str>,
    pub filter: &'a Filter,
    pub params: SearchParams,
    /// 可被之后的搜索取代；被取代时返回 `DbError::Cancelled`
    pub cancel: Option<Cancel>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SegmentHit {
    pub segment_id: i64,
    pub locator_json: String,
    pub cosine: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub file_id: i64,
    /// 根目录路径与相对路径拼成的完整路径
    pub path: String,
    pub name: String,
    pub kind: String,
    pub size: i64,
    pub mtime: i64,
    pub cloud: bool,
    /// 所在根目录不可访问
    pub offline: bool,
    /// 所在根目录不存在，它的磁盘或存储已连接
    pub missing: bool,
    pub score: f64,
    /// 最高余弦；没有语义命中时为空
    pub cosine: Option<f32>,
    /// 校准后的最高分数（z 分数）；没有语义命中或所在模态还没有背景分布时为空
    pub z: Option<f32>,
    /// 匹配度较低：z 低于门槛，文件名与正文也没有命中（界面折叠显示）
    pub low: bool,
    /// 按 z（没有时按余弦）从高到低
    pub segments: Vec<SegmentHit>,
    pub name_hit: bool,
    /// 正文关键词命中的片段 id，最匹配的在前；为空表示正文没有命中
    pub text_segments: Vec<i64>,
    /// 图片的宽高，未记录时为空
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// 目标空间的向量能否与查询向量直接比较：
/// - 模型家族、主模型、运行库、精度、前缀方案都要相同；
/// - `backend` 不参与：CPU 与经自检的 Vulkan 视为兼容（实测评测集子集最小余弦 ≥ 0.9976，前 10 重合平均 ≥ 96.8%）；
/// - 文本查询比较文本空间时预处理版本要相同；比较图像、音频空间（`mmproj_sha256` 不为空）时不看 mmproj、档位与切段参数；
/// - 以图搜图只比较图像空间，mmproj 与预处理版本（含视觉 token 档位）都要相同。
pub fn comparable(query: &Space, target: &Space) -> bool {
    let same = query.family == target.family
        && query.model_sha256 == target.model_sha256
        && query.runtime == target.runtime
        && query.precision == target.precision
        && query.prefix_scheme == target.prefix_scheme;
    if !same {
        return false;
    }
    match (&query.mmproj_sha256, &target.mmproj_sha256) {
        (None, None) => query.preprocess == target.preprocess,
        (None, Some(_)) => true,
        (Some(_), None) => false,
        (Some(a), Some(b)) => a == b && query.preprocess == target.preprocess,
    }
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// 两个可能缺失的分数中较高的
fn higher(a: Option<f32>, b: Option<f32>) -> Option<f32> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

/// 一个文件在某个模态（合并后为全部模态）中的命中
struct FileHits {
    /// 最高余弦
    best: f32,
    /// 校准过的模态中的最高 z
    z: Option<f32>,
    /// 有命中来自还没校准的模态，不判定匹配度门槛
    uncalibrated: bool,
    /// (segment_id, 余弦, z)
    segments: Vec<(i64, f32, Option<f32>)>,
}

impl FileHits {
    /// 片段按 z 从高到低，没有 z 的按余弦排在后面
    fn sort(&mut self) {
        let key = |s: &(i64, f32, Option<f32>)| s.2.unwrap_or(f32::NEG_INFINITY);
        self.segments
            .sort_by(|a, b| key(b).total_cmp(&key(a)).then(b.1.total_cmp(&a.1)));
    }
}

/// 文件夹范围的条件 `(… OR …)`（不含前面的 `AND`），参数依次放进 `args`；`folders` 不能为空。
/// 子文件夹用 `LIKE` 有意取它的 ASCII 不区分大小写：Windows 路径不区分大小写，命令行输入的大小写可能与磁盘不同
pub(crate) fn folders_sql(folders: &[(i64, String)], args: &mut Vec<Value>) -> String {
    let parts: Vec<&str> = folders
        .iter()
        .map(|(root, rel)| {
            args.push(Value::Integer(*root));
            if rel.is_empty() {
                return "f.root_id = ?";
            }
            let escaped = rel.replace('!', "!!").replace('%', "!%").replace('_', "!_");
            args.push(Value::Text(format!(
                "{escaped}{}%",
                std::path::MAIN_SEPARATOR
            )));
            "(f.root_id = ? AND f.rel_path LIKE ? ESCAPE '!')"
        })
        .collect();
    format!("({})", parts.join(" OR "))
}

/// 追加过滤条件，参数依次放进 `args`。文件夹的子文件夹按 `LIKE` 匹配（`folders_sql`），有意 ASCII 不区分大小写
fn filter_sql(filter: &Filter, args: &mut Vec<Value>) -> String {
    let mut sql = String::new();
    if !filter.kinds.is_empty() {
        sql += &format!(
            " AND f.kind IN ({})",
            vec!["?"; filter.kinds.len()].join(",")
        );
        args.extend(filter.kinds.iter().map(|k| Value::Text(k.clone())));
    }
    if !filter.folders.is_empty() {
        sql += " AND ";
        sql += &folders_sql(&filter.folders, args);
    }
    if let Some(t) = filter.since {
        sql += " AND f.mtime >= ?";
        args.push(Value::Integer(t));
    }
    if let Some(t) = filter.until {
        sql += " AND f.mtime <= ?";
        args.push(Value::Integer(t));
    }
    if let Some(id) = filter.exclude_file {
        sql += " AND f.id <> ?";
        args.push(Value::Integer(id));
    }
    sql
}

/// 查询按空白拆成的词全部命中才算。每个词都不少于 3 个字符时返回 FTS 表达式：每个词一个短语，用 AND 连接，
/// 内部的双引号写两遍，FTS 的运算符和特殊字符都按普通文本处理。trigram 匹配不到更短的词，这时返回空，整体退回 LIKE
fn fts_query(words: &[&str]) -> Option<String> {
    if words.iter().any(|w| w.chars().count() < 3) {
        return None;
    }
    let phrases: Vec<String> = words
        .iter()
        .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
        .collect();
    Some(phrases.join(" AND "))
}

/// 每个词一个 `col LIKE ? ESCAPE '\'`，用 AND 连接；参数中的 `%`、`_` 按字面匹配
fn likes(col: &str, words: &[&str], args: &mut Vec<Value>) -> String {
    for w in words {
        let escaped = w
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        args.push(Value::Text(format!("%{escaped}%")));
    }
    vec![format!("{col} LIKE ? ESCAPE '\\'"); words.len()].join(" AND ")
}

/// 文件名通道的查询与参数
fn names_sql(words: &[&str], filter: &Filter) -> (String, Vec<Value>) {
    let mut args = Vec::new();
    let (head, order) = match fts_query(words) {
        Some(expr) => {
            args.push(Value::Text(format!("name : ({expr})")));
            (
                "SELECT f.id FROM fts_names JOIN files f ON f.id = fts_names.rowid WHERE fts_names MATCH ?".to_string(),
                "bm25(fts_names)",
            )
        }
        // 固定沿 files_mtime 从新到旧逐个文件（按根目录等过滤时也是：不符合的文件只比较一行），
        // 不用把全部命中排一遍序，取够即停
        None => (
            format!(
                "SELECT f.id FROM files f INDEXED BY files_mtime WHERE {}",
                likes("f.name", words, &mut args)
            ),
            "f.mtime DESC, f.id DESC",
        ),
    };
    let sql = format!(
        "{head}{} ORDER BY {order} LIMIT {KEYWORD_LIMIT}",
        filter_sql(filter, &mut args)
    );
    (sql, args)
}

/// 文件名通道，返回按匹配度排好的文件 id
fn names(db: &Db, query: &str, filter: &Filter) -> rusqlite::Result<Vec<i64>> {
    let words: Vec<&str> = query.split_whitespace().collect();
    let (sql, args) = names_sql(&words, filter);
    db.conn
        .prepare(&sql)?
        .query_map(params_from_iter(args), |row| row.get(0))?
        .collect()
}

/// 正文通道的查询与参数；第三项为是否走 FTS
fn texts_sql(words: &[&str], filter: &Filter) -> (String, Vec<Value>, bool) {
    let mut args = Vec::new();
    let expr = fts_query(words);
    let fts = expr.is_some();
    let (head, order) = match expr {
        Some(expr) => {
            args.push(Value::Text(expr));
            (
                "SELECT s.file_id, s.id FROM fts_text JOIN segments s ON s.id = fts_text.rowid
                 JOIN files f ON f.id = s.file_id WHERE fts_text MATCH ?"
                    .to_string(),
                "bm25(fts_text)",
            )
        }
        // 同文件名通道先沿 files_mtime 逐个文件，再按 segments_file 查它的片段，取够文件后停下读取即结束查询
        None => (
            format!(
                "SELECT s.file_id, s.id FROM files f INDEXED BY files_mtime CROSS JOIN segments s ON s.file_id = f.id
                 WHERE {}",
                likes("s.text", words, &mut args)
            ),
            "f.mtime DESC, f.id DESC",
        ),
    };
    let sql = format!(
        "{head}{} ORDER BY {order}, s.id",
        filter_sql(filter, &mut args)
    );
    (sql, args, fts)
}

/// 正文通道，返回按最好的片段排好的文件及其命中的片段 id（最匹配的在前）
fn texts(db: &Db, query: &str, filter: &Filter) -> rusqlite::Result<Vec<(i64, Vec<i64>)>> {
    let words: Vec<&str> = query.split_whitespace().collect();
    let (sql, args, fts) = texts_sql(&words, filter);
    let mut stmt = db.conn.prepare(&sql)?;
    let mut rows = stmt.query(params_from_iter(args))?;
    let mut out: Vec<(i64, Vec<i64>)> = Vec::new();
    let mut pos: HashMap<i64, usize> = HashMap::new();
    while let Some(row) = rows.next()? {
        let (file, segment): (i64, i64) = (row.get(0)?, row.get(1)?);
        if let Some(&i) = pos.get(&file) {
            out[i].1.push(segment);
            continue;
        }
        if out.len() == KEYWORD_LIMIT {
            // LIKE 的结果按文件排序，之后不会再有已取文件的片段
            if !fts {
                break;
            }
            continue;
        }
        pos.insert(file, out.len());
        out.push((file, vec![segment]));
    }
    Ok(out)
}

/// 精排 `modality` 模态的候选并按文件聚合，返回按最高余弦排好的文件；`stats` 不为空时同时给出 z：
/// 候选自带的（粗排按本次查询的分数分布算出）优先，否则用背景分布换算（调用方已确认每个候选都有其一）
fn rerank(
    db: &Db,
    query: &[f32],
    cands: &[crate::vindex::Candidate],
    modality: &str,
    stats: Option<&Stats>,
) -> rusqlite::Result<Vec<(i64, FileHits)>> {
    let mut stmt = db
        .conn
        .prepare_cached("SELECT vec FROM vectors WHERE segment_id = ?1 AND space_id = ?2")?;
    let mut files: HashMap<i64, FileHits> = HashMap::new();
    for c in cands {
        // 粗排之后被删除的片段跳过
        let Some(blob) = stmt
            .query_row(params![c.segment_id, c.space_id], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .optional()?
        else {
            continue;
        };
        let Some(v) = blob_to_vec(&blob) else {
            log::warn!("片段 {} 的向量数据已损坏，已跳过", c.segment_id);
            continue;
        };
        let cos: f32 = query.iter().zip(&v).map(|(a, b)| a * b).sum();
        let z = stats.and_then(|s| {
            c.z.or_else(|| {
                s.get(&(c.space_id.clone(), modality.to_string()))
                    .map(|s| s.z(cos))
            })
        });
        let f = files.entry(c.file_id).or_insert(FileHits {
            best: f32::MIN,
            z: None,
            uncalibrated: stats.is_none(),
            segments: Vec::new(),
        });
        f.best = f.best.max(cos);
        f.z = higher(f.z, z);
        // 同一片段在两个兼容空间都有向量时只保留较高的一个
        match f.segments.iter_mut().find(|s| s.0 == c.segment_id) {
            Some(s) => {
                s.1 = s.1.max(cos);
                s.2 = higher(s.2, z);
            }
            None => f.segments.push((c.segment_id, cos, z)),
        }
    }
    let mut out: Vec<(i64, FileHits)> = files.into_iter().collect();
    for (_, f) in &mut out {
        f.sort();
    }
    out.sort_by(|a, b| b.1.best.total_cmp(&a.1.best).then(a.0.cmp(&b.0)));
    Ok(out)
}

/// RRF 融合：`lists` 为（权重, 按名次排好的文件 id），返回全部文件，分数从高到低。
/// 同分时按 `cosine` 给出的最高余弦（没有的排后），再按文件 id：不同模态的同一名次分数相同，用余弦决定先后比按 id
/// 更接近匹配度。不用 z：评测中按 z 决定先后，混合库 R@1 更低
fn fuse(lists: &[(f64, Vec<i64>)], k: f64, cosine: impl Fn(i64) -> Option<f32>) -> Vec<(i64, f64)> {
    let mut scores: HashMap<i64, f64> = HashMap::new();
    // 权重为 0 的列表不参与，只在其中出现的文件不进结果
    for (w, ids) in lists.iter().filter(|(w, _)| *w > 0.0) {
        for (rank, id) in ids.iter().enumerate() {
            *scores.entry(*id).or_default() += w / (k + rank as f64 + 1.0);
        }
    }
    let mut out: Vec<(i64, f64)> = scores.into_iter().collect();
    out.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| {
                cosine(b.0)
                    .partial_cmp(&cosine(a.0))
                    .unwrap_or(Ordering::Equal)
            })
            .then(a.0.cmp(&b.0))
    });
    out
}

/// 执行一次检索；先取内存索引的锁再取数据库的锁。从同步内存索引到精排、组装结果一直持有数据库锁，
/// 粗排与精排之间不会有写入插进来。在阻塞线程中调用
pub fn search(db: &Mutex<Db>, index: &Mutex<VIndex>, q: &Query) -> Result<Vec<Hit>, DbError> {
    let p = q.params;
    let t = Instant::now();
    let mut vi = lock(index);
    let db = lock(db);
    // 等锁期间已被取代的不再开始
    if q.cancel.as_ref().is_some_and(Cancel::superseded) {
        return Err(DbError::Cancelled);
    }
    let mods: Vec<&str> = match q.vector {
        Some((_, space)) => q
            .modality
            .map_or_else(|| modalities(space).to_vec(), |m| vec![m]),
        None => Vec::new(),
    };
    let cands: Vec<Vec<crate::vindex::Candidate>> = match q.vector {
        Some((_, space)) => {
            vi.sync(&db)?;
            let adaptive = q.kind == QueryKind::Similar;
            // 有子文件夹时内存索引中没有路径可比，先从库中取出范围内的文件，各模态共用
            let files = if q.filter.folders.iter().any(|(_, rel)| !rel.is_empty()) {
                let mut args = Vec::new();
                let sql = format!(
                    "SELECT f.id FROM files f WHERE {}",
                    folders_sql(&q.filter.folders, &mut args)
                );
                let ids: HashSet<i64> = db
                    .conn
                    .prepare(&sql)
                    .and_then(|mut stmt| {
                        stmt.query_map(params_from_iter(args), |row| row.get(0))?
                            .collect()
                    })
                    .map_err(|err| db.err(err))?;
                Some(ids)
            } else {
                None
            };
            mods.iter()
                .map(|m| {
                    vi.coarse(
                        q.coarse,
                        space,
                        m,
                        q.filter,
                        files.as_ref(),
                        p.candidates,
                        adaptive,
                    )
                })
                .collect()
        }
        None => Vec::new(),
    };
    drop(vi);
    log::debug!("粗排用时 {:.1} 毫秒", ms(t));

    // 只从精排起中止：同步内存索引中途中止会留下不完整的状态。之后都是只读查询，中止不影响数据
    let _interruptible = match &q.cancel {
        Some(c) => {
            let c = c.clone();
            db.conn
                .progress_handler(1000, Some(move || c.superseded()))
                .map_err(|err| db.err(err))?;
            Some(Interruptible(&db))
        }
        None => None,
    };

    let t = Instant::now();
    let mut lists: Vec<(f64, Vec<i64>)> = Vec::new();
    let mut semantic: HashMap<i64, FileHits> = HashMap::new();
    if let Some((v, _)) = q.vector {
        let stats = calib::load(&db, q.kind)?;
        let calibrated = |m: &str, c: &crate::vindex::Candidate| {
            c.z.is_some() || stats.contains_key(&(c.space_id.clone(), m.to_string()))
        };
        // 文本查询、正文已校准时，有 z 的内嵌媒体并入文档列表（模块说明）
        let merge = q.modality.is_none()
            && mods
                .iter()
                .zip(&cands)
                .any(|(m, cs)| *m == "text" && cs.iter().all(|c| calibrated(m, c)));
        // 文档列表：正文的（文件, z, 余弦），内嵌媒体的（文件 → 最高 z）
        let mut text_ranked: Vec<(i64, Option<f32>, f32)> = Vec::new();
        let mut embedded: HashMap<i64, Option<f32>> = HashMap::new();
        for (m, cands) in mods.iter().zip(&cands) {
            // 内嵌媒体还没校准时照旧留在所属模态的列表中
            let split = merge
                && *m != "text"
                && cands.iter().any(|c| c.embedded)
                && cands
                    .iter()
                    .filter(|c| c.embedded)
                    .all(|c| calibrated(m, c));
            let groups: Vec<(bool, Cow<[crate::vindex::Candidate]>)> = if split {
                let (embeds, own): (Vec<_>, Vec<_>) =
                    cands.iter().cloned().partition(|c| c.embedded);
                vec![(false, own.into()), (true, embeds.into())]
            } else {
                vec![(false, cands.as_slice().into())]
            };
            for (embed, cands) in groups {
                let calibrated = cands.iter().all(|c| calibrated(m, c));
                let ranked = rerank(&db, v, &cands, m, calibrated.then_some(&stats))
                    .map_err(|err| db.err(err))?;
                if embed {
                    for (id, f) in &ranked {
                        let z = embedded.entry(*id).or_default();
                        *z = higher(*z, f.z);
                    }
                } else if merge && *m == "text" {
                    text_ranked = ranked.iter().map(|(id, f)| (*id, f.z, f.best)).collect();
                } else {
                    lists.push((
                        p.weight_semantic,
                        ranked.iter().map(|(id, _)| *id).collect(),
                    ));
                }
                // 一个文件在多个模态都有命中时合并
                for (id, f) in ranked {
                    match semantic.get_mut(&id) {
                        Some(old) => {
                            old.best = old.best.max(f.best);
                            old.z = higher(old.z, f.z);
                            old.uncalibrated |= f.uncalibrated;
                            old.segments.extend(f.segments);
                            old.sort();
                        }
                        None => {
                            semantic.insert(id, f);
                        }
                    }
                }
            }
        }
        if merge {
            lists.push((p.weight_semantic, doc_list(&text_ranked, &embedded)));
        }
    }
    log::debug!("精排用时 {:.1} 毫秒", ms(t));

    let text = q.text.filter(|s| !s.is_empty());
    let t = Instant::now();
    let name_ids = match text {
        Some(text) => names(&db, text, q.filter).map_err(|err| db.err(err))?,
        None => Vec::new(),
    };
    log::debug!("文件名通道用时 {:.1} 毫秒", ms(t));

    let t = Instant::now();
    // 只有 1 个字时几乎每段正文都含有它，既要扫描全部片段，结果也没有意义
    let text_hits = match text.filter(|s| s.chars().count() >= 2) {
        Some(text) => texts(&db, text, q.filter).map_err(|err| db.err(err))?,
        None => Vec::new(),
    };
    log::debug!("正文通道用时 {:.1} 毫秒", ms(t));

    let t = Instant::now();
    lists.push((p.weight_name, name_ids.clone()));
    lists.push((p.weight_text, text_hits.iter().map(|(id, _)| *id).collect()));
    let fused = fuse(&lists, p.rrf_k, |id| semantic.get(&id).map(|f| f.best));
    let mut text_hits: HashMap<i64, Vec<i64>> = text_hits.into_iter().collect();
    // 以图搜图与找相似的不折叠
    let is_low = |id: i64| {
        if q.kind == QueryKind::Similar {
            return false;
        }
        let weak = semantic.get(&id).is_some_and(|f| {
            !f.uncalibrated && f.z.is_some_and(|z| f64::from(z) < p.min_relevance)
        });
        weak && !name_ids.contains(&id) && !text_hits.contains_key(&id)
    };
    let (high, low): (Vec<_>, Vec<_>) = fused
        .into_iter()
        .map(|(id, score)| (id, score, is_low(id)))
        .partition(|f| !f.2);
    let fused: Vec<(i64, f64, bool)> = high.into_iter().chain(low).take(p.limit).collect();
    let res =
        build(&db, &fused, &mut semantic, &name_ids, &mut text_hits, &p).map_err(|err| db.err(err));
    log::debug!("融合与组装结果用时 {:.1} 毫秒", ms(t));
    res
}

/// 文档列表（模块说明）：已校准的正文 `text`（文件, z, 余弦）与已校准的内嵌媒体 `embedded`（文件 → 最高 z）合并，
/// 每份文档按两者中较高的 z 排名，同 z 时按正文余弦（只有内嵌命中的排后）、文件 id。
/// 正文或某组内嵌媒体还没校准时不经过这里：它们照旧作为各自模态的列表参加融合（`search`）
fn doc_list(text: &[(i64, Option<f32>, f32)], embedded: &HashMap<i64, Option<f32>>) -> Vec<i64> {
    let in_text: HashSet<i64> = text.iter().map(|t| t.0).collect();
    let mut docs: Vec<(i64, f32, f32)> = text
        .iter()
        .map(|&(id, z, cos)| {
            let z = higher(z, embedded.get(&id).copied().flatten()).unwrap_or(f32::NEG_INFINITY);
            (id, z, cos)
        })
        .collect();
    for (&id, &z) in embedded {
        if let Some(z) = z
            && !in_text.contains(&id)
        {
            docs.push((id, z, f32::NEG_INFINITY));
        }
    }
    docs.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then(b.2.total_cmp(&a.2))
            .then(a.0.cmp(&b.0))
    });
    docs.into_iter().map(|d| d.0).collect()
}

/// 查询向量所在的空间要检索的模态：文本查询检索全部模态，以图搜图只检索图片
fn modalities(space: &Space) -> &'static [&'static str] {
    if space.mmproj_sha256.is_some() {
        &["image"]
    } else {
        &["text", "image", "audio", "video_av", "video_v"]
    }
}

/// “找相似的”的查询：文件 `file_id` 中片段 `segment_id`（为空时取该文件第一个有向量的片段）已存的向量、
/// 所在空间与片段的模态。片段有多个空间的向量（重建期间）时取最新写入的，即当前空间。片段不属于该文件、没有向量或向量损坏时为空。
/// 长图切片（locator 带区域）与整图档位不同、空间不可比较时改用同一文件的整图向量：切片的空间里只有其他长图的切片；
/// 档位相同时仍用切片本身，找与这一段相似的
pub fn stored_vector(
    db: &Db,
    file_id: i64,
    segment_id: Option<i64>,
) -> Result<Option<(Vec<f32>, Space, String)>, DbError> {
    let found = segment_vector(db, file_id, segment_id)?;
    let (Some(id), Some((_, slice, _))) = (segment_id, &found) else {
        return Ok(found);
    };
    let whole: Option<i64> = db
        .conn
        .query_row(
            "SELECT w.id FROM segments s JOIN segments w ON w.file_id = s.file_id AND w.modality = 'image'
             AND json_extract(w.locator_json, '$.region') IS NULL
             WHERE s.id = ?1 AND s.modality = 'image' AND json_extract(s.locator_json, '$.region') IS NOT NULL
             ORDER BY w.id LIMIT 1",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|err| db.err(err))?;
    let Some(whole) = whole else {
        return Ok(found);
    };
    match segment_vector(db, file_id, Some(whole))? {
        Some(w) if !comparable(slice, &w.1) => Ok(Some(w)),
        _ => Ok(found),
    }
}

/// 片段已存的向量、空间与模态（见 `stored_vector`）
fn segment_vector(
    db: &Db,
    file_id: i64,
    segment_id: Option<i64>,
) -> Result<Option<(Vec<f32>, Space, String)>, DbError> {
    let row: Option<(Vec<u8>, String, String)> = db
        .conn
        .query_row(
            "SELECT v.vec, v.space_id, s.modality FROM segments s JOIN vectors v ON v.segment_id = s.id
             WHERE s.file_id = ?1 AND (?2 IS NULL OR s.id = ?2) ORDER BY s.id, v.rowid DESC LIMIT 1",
            params![file_id, segment_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|err| db.err(err))?;
    let Some((blob, space_id, modality)) = row else {
        return Ok(None);
    };
    let Some(v) = blob_to_vec(&blob) else {
        log::warn!("文件 {file_id} 的向量数据已损坏，无法找相似");
        return Ok(None);
    };
    let space = crate::vindex::space_row(&db.conn, &space_id).map_err(|err| db.err(err))?;
    Ok(space.map(|s| (v, s, modality)))
}

fn build(
    db: &Db,
    fused: &[(i64, f64, bool)],
    semantic: &mut HashMap<i64, FileHits>,
    name_ids: &[i64],
    text_hits: &mut HashMap<i64, Vec<i64>>,
    p: &SearchParams,
) -> rusqlite::Result<Vec<Hit>> {
    let mut file = db.conn.prepare_cached(
        "SELECT f.rel_path, f.name, f.kind, f.size, f.mtime, f.cloud, r.path, r.offline, f.width, f.height, r.offline = 2
         FROM files f JOIN roots r ON r.id = f.root_id WHERE f.id = ?1",
    )?;
    let mut locator = db
        .conn
        .prepare_cached("SELECT locator_json FROM segments WHERE id = ?1")?;
    let mut hits = Vec::new();
    for &(id, score, low) in fused {
        let Some(mut hit) = file
            .query_row([id], |row| {
                let rel: String = row.get(0)?;
                let root: String = row.get(6)?;
                Ok(Hit {
                    file_id: id,
                    path: PathBuf::from(root).join(rel).to_string_lossy().into_owned(),
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    size: row.get(3)?,
                    mtime: row.get(4)?,
                    cloud: row.get(5)?,
                    offline: row.get(7)?,
                    missing: row.get(10)?,
                    score,
                    cosine: None,
                    z: None,
                    low,
                    segments: Vec::new(),
                    name_hit: name_ids.contains(&id),
                    text_segments: text_hits.remove(&id).unwrap_or_default(),
                    // 库中的值不合法时当作未记录，不让整次搜索失败
                    width: row
                        .get::<_, Option<i64>>(8)?
                        .and_then(|v| u32::try_from(v).ok()),
                    height: row
                        .get::<_, Option<i64>>(9)?
                        .and_then(|v| u32::try_from(v).ok()),
                })
            })
            .optional()?
        else {
            continue;
        };
        if let Some(f) = semantic.remove(&id) {
            hit.cosine = Some(f.best);
            hit.z = f.z;
            for (segment_id, cosine, _) in f.segments {
                if let Some(locator_json) = locator
                    .query_row([segment_id], |row| row.get(0))
                    .optional()?
                {
                    hit.segments.push(SegmentHit {
                        segment_id,
                        locator_json,
                        cosine,
                    });
                }
            }
        }
        if hit.kind == "video" {
            hit.segments = merge_windows(
                std::mem::take(&mut hit.segments),
                p.video_windows,
                p.video_gap_ms,
            );
        }
        hits.push(hit);
    }
    Ok(hits)
}

/// 视频的命中窗口：`segments` 已按 z 从高到低（`FileHits::sort`；带音频与纯画面的窗口在不同向量空间，余弦不能直接比），
/// 取前 `k` 个，按时间排序后把间隔不超过 `gap_ms` 的相邻窗口合并成一段（定位为合并后的起止），`segment_id` 与余弦取其中
/// z 最高的窗口；结果仍按 z 从高到低。没有时间定位的片段原样保留
fn merge_windows(segments: Vec<SegmentHit>, k: usize, gap_ms: u64) -> Vec<SegmentHit> {
    let span = |s: &SegmentHit| {
        let v: serde_json::Value = serde_json::from_str(&s.locator_json).ok()?;
        Some((v["start_ms"].as_u64()?, v["end_ms"].as_u64()?))
    };
    // 名次即 z 的高低：越小越好
    let mut timed: Vec<(u64, u64, usize, SegmentHit)> = Vec::new();
    let mut out = Vec::new();
    for (rank, s) in segments.into_iter().take(k).enumerate() {
        match span(&s) {
            Some((a, b)) => timed.push((a, b, rank, s)),
            None => out.push((rank, s)),
        }
    }
    timed.sort_by_key(|t| t.0);
    let mut merged: Vec<(u64, u64, usize, SegmentHit)> = Vec::new();
    for (a, b, rank, s) in timed {
        if let Some(last) = merged.last_mut()
            && a <= last.1.saturating_add(gap_ms)
        {
            last.1 = last.1.max(b);
            if rank < last.2 {
                last.2 = rank;
                last.3.segment_id = s.segment_id;
                last.3.cosine = s.cosine;
            }
            continue;
        }
        merged.push((a, b, rank, s));
    }
    out.extend(merged.into_iter().map(|(a, b, rank, mut s)| {
        s.locator_json = serde_json::json!({ "start_ms": a, "end_ms": b }).to_string();
        (rank, s)
    }));
    out.sort_by_key(|o| o.0);
    out.into_iter().map(|o| o.1).collect()
}

#[cfg(test)]
pub(crate) mod tests;
