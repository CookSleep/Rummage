//! 分数校准：按“查询类型 × 目标空间 × 模态”维护余弦的背景分布（均值、标准差），
//! 搜索时把语义候选的余弦换成 z 分数 `(余弦 − 均值) / 标准差`，用于匹配度门槛与匹配度，不同模态、不同空间用同一个门槛（`search`）。
//! - 查询类型（`QueryKind`）：文字查询 `text`；以图搜图与“找相似的” `similar`（查询本身是库中某一模态的向量）。
//! - 模态按片段的 `modality` 区分（文档、图片、音频、带音频的视频、纯画面的视频各自独立）；长图切片与整图同属图片模态，
//!   但切片与整图档位不同时在另一个空间中，按空间分开统计。
//! - `text` 的背景：随程序附带的通用查询表（`assets/calib/queries.txt`，约 200 条，中英混合、覆盖常见话题）用当前文本空间
//!   编码，向量缓存在 `calib_queries` 中，文本空间与查询表不变就不再算；对每个可比较的“空间 × 模态”随机抽取至多 2000 条
//!   向量，统计全部“查询 × 向量”余弦的均值与标准差。
//! - `similar` 的背景：同一“空间 × 模态”随机抽取的向量中取至多 200 条作查询，与抽样的向量两两打分（不含自身），
//!   不需要推理，也不联网。搜索时只在本次查询自身的分数分布算不出来（该空间向量太少）时才用它（`VIndex::coarse`）。
//! - 何时重算（`update`）：某个“空间 × 模态”还没有分布、向量数与上次计算时相差超过 20%，或查询来源（文本空间、查询表）变了。
//!   向量少于下限时不计算：`similar` 为 `MIN_VECTORS`；`text` 的样本是通用查询表乘以全部向量，下限低得多
//!   （`TEXT_MIN_VECTORS`），只有几段录音或几份文档的模态也有分布，其中无关的结果照样能按门槛折叠。新分布算出前旧分布一直有效；还没有分布的模态，其中的结果没有 z，不判定门槛（`search` 模块说明）。
//! - 读出的分布须是有限值且标准差为正，否则当作没有分布。
//! - 抽样从内存向量索引取片段 id，再按主键读 f16 向量；锁的顺序与检索相同（先内存索引、后数据库），推理与统计时不持有锁。
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{OptionalExtension, params};

use crate::db::{Db, DbError, Space, blob_to_vec, now_ms, vec_to_blob};
use crate::lock;
use crate::search::comparable;
use crate::vindex::VIndex;

/// 通用查询表
const QUERY_LIST: &str = include_str!("../../../assets/calib/queries.txt");
/// 每个“空间 × 模态”最多抽取的向量数
const SAMPLE: usize = 2000;
/// `similar` 从抽样中取作查询的向量数
const SIMILAR_QUERIES: usize = 200;
/// 向量数相对上次计算变化超过这个比例时重算
const RECOMPUTE_CHANGE: f64 = 0.2;
/// 抽到的向量少于这个数时不计算：样本太少，标准差不可靠，该模态按没有分布处理（已有的旧分布保留）
pub(crate) const MIN_VECTORS: usize = 20;
/// `text` 的下限：样本是通用查询表的约 200 条查询乘以全部向量，几个向量就有上千个余弦
const TEXT_MIN_VECTORS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueryKind {
    Text,
    Similar,
}

impl QueryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            QueryKind::Text => "text",
            QueryKind::Similar => "similar",
        }
    }
}

/// 一个“查询类型 × 空间 × 模态”的余弦分布
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Stat {
    pub mean: f64,
    pub std: f64,
}

impl Stat {
    pub fn z(&self, cosine: f32) -> f32 {
        ((f64::from(cosine) - self.mean) / self.std) as f32
    }
}

/// 某个查询类型的全部分布，键为（空间 id, 模态）
pub(crate) type Stats = HashMap<(String, String), Stat>;

/// xorshift64*，只用于抽样，免得引入随机数依赖
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }

    /// 以当前时间为种子
    pub fn from_time() -> Rng {
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        Rng::new(t)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// `[0, n)` 中的整数，`n` 须大于 0
    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CalibError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("计算通用查询的向量失败：{0}")]
    Embed(String),
}

/// 通用查询表中的查询，去掉注释与空行
pub fn queries() -> Vec<&'static str> {
    QUERY_LIST
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

/// 某个查询类型的全部分布
pub(crate) fn load(db: &Db, kind: QueryKind) -> Result<Stats, DbError> {
    let rows: Vec<((String, String), Stat)> = db
        .conn
        .prepare_cached("SELECT space_id, modality, mean, std FROM calib WHERE kind = ?1")
        .and_then(|mut stmt| {
            stmt.query_map([kind.as_str()], |row| {
                Ok((
                    (row.get(0)?, row.get(1)?),
                    Stat {
                        mean: row.get(2)?,
                        std: row.get(3)?,
                    },
                ))
            })?
            .collect()
        })
        .map_err(|err| db.err(err))?;
    Ok(rows
        .into_iter()
        .filter(|(_, s)| s.mean.is_finite() && s.std.is_finite() && s.std > 0.0)
        .collect())
}

/// 已有分布的（来源, 当时的向量数），键为（类型, 空间 id, 模态）
type Sources = HashMap<(String, String, String), (String, i64)>;

fn sources(db: &Db) -> Result<Sources, DbError> {
    db.conn
        .prepare("SELECT kind, space_id, modality, source, vectors FROM calib")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| {
                Ok((
                    (row.get(0)?, row.get(1)?, row.get(2)?),
                    (row.get(3)?, row.get(4)?),
                ))
            })?
            .collect()
        })
        .map_err(|err| db.err(err))
}

/// 需要（重新）计算：还没有分布、来源变了，或向量数相对上次变化超过 `RECOMPUTE_CHANGE`
fn stale(prev: Option<&(String, i64)>, source: &str, n: usize) -> bool {
    let Some((src, m)) = prev else {
        return true;
    };
    src != source || (n as f64 - *m as f64).abs() > RECOMPUTE_CHANGE * *m as f64
}

/// 全部“查询 × 向量”余弦的均值与总体标准差；`skip_self` 时第 i 条查询不与第 i 个向量配对（查询取自向量本身）。
/// 配对不足两组或分布退化（标准差近乎为 0）时为空
pub(crate) fn stat(queries: &[Vec<f32>], targets: &[Vec<f32>], skip_self: bool) -> Option<Stat> {
    let (mut sum, mut sq, mut n) = (0f64, 0f64, 0u64);
    for (i, q) in queries.iter().enumerate() {
        for (j, t) in targets.iter().enumerate() {
            if skip_self && i == j {
                continue;
            }
            let c = f64::from(q.iter().zip(t).map(|(a, b)| a * b).sum::<f32>());
            sum += c;
            sq += c * c;
            n += 1;
        }
    }
    if n < 2 {
        return None;
    }
    let mean = sum / n as f64;
    let std = (sq / n as f64 - mean * mean).max(0.0).sqrt();
    (std > 1e-6).then_some(Stat { mean, std })
}

/// 按片段 id 读出某个空间的 768 维向量；已删除或已损坏的跳过
fn read_vectors(db: &Db, space_id: &str, ids: &[i64]) -> Result<Vec<Vec<f32>>, DbError> {
    let mut stmt = db
        .conn
        .prepare_cached("SELECT vec FROM vectors WHERE segment_id = ?1 AND space_id = ?2")
        .map_err(|err| db.err(err))?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let blob: Option<Vec<u8>> = stmt
            .query_row(params![id, space_id], |row| row.get(0))
            .optional()
            .map_err(|err| db.err(err))?;
        if let Some(v) = blob.and_then(|b| blob_to_vec(&b)) {
            out.push(v);
        }
    }
    Ok(out)
}

fn save(
    db: &Db,
    kind: QueryKind,
    group: &(Space, String, usize),
    source: &str,
    stat: Stat,
) -> Result<(), DbError> {
    db.conn
        .execute(
            "INSERT OR REPLACE INTO calib (kind, space_id, modality, source, mean, std, vectors, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                kind.as_str(),
                group.0.id,
                group.1,
                source,
                stat.mean,
                stat.std,
                group.2 as i64,
                now_ms()
            ],
        )
        .map_err(|err| db.err(err))?;
    Ok(())
}

/// 计算一条查询加查询前缀后的已归一化向量
pub type Embed<'a> = dyn FnMut(&str) -> Result<Vec<f32>, String> + 'a;

/// 通用查询表在文本空间 `space` 中编码的向量：先取缓存，缺的用 `embed` 计算后写回；
/// 缓存只保留这个空间、这份查询表的
fn query_vectors(
    db: &Mutex<Db>,
    space: &Space,
    embed: &mut Embed,
) -> Result<Vec<Vec<f32>>, CalibError> {
    let list = queries();
    let key = &space.id;
    let cached: HashMap<String, Vec<u8>> = {
        let db = lock(db);
        db.conn
            .prepare("SELECT query, vec FROM calib_queries WHERE space_id = ?1")
            .and_then(|mut stmt| {
                stmt.query_map([key], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect()
            })
            .map_err(|err| db.err(err))?
    };
    let mut out = Vec::with_capacity(list.len());
    let mut fresh = false;
    for q in &list {
        match cached.get(*q).and_then(|b| blob_to_vec(b)) {
            Some(v) => out.push(v),
            None => {
                out.push(embed(q).map_err(CalibError::Embed)?);
                fresh = true;
            }
        }
    }
    if fresh || cached.len() != list.len() {
        let db = lock(db);
        let res = db.conn.unchecked_transaction().and_then(|tx| {
            // 别的文本空间的缓存一并删掉
            tx.execute("DELETE FROM calib_queries", [])?;
            for (q, v) in list.iter().zip(&out) {
                tx.execute(
                    "INSERT INTO calib_queries (space_id, query, vec) VALUES (?1, ?2, ?3)",
                    params![key, q, vec_to_blob(v)],
                )?;
            }
            tx.commit()
        });
        res.map_err(|err| db.err(err))?;
    }
    Ok(out)
}

/// 文字查询的背景分布所用的文本空间与编码方式
pub struct TextQueries<'a> {
    pub space: &'a Space,
    pub embed: &'a mut Embed<'a>,
}

/// 算出需要（重新）计算的分布并写库，返回更新的个数。`text` 为空时只算 `similar`（没有可用的推理引擎）
pub fn update(
    db: &Mutex<Db>,
    index: &Mutex<VIndex>,
    text: Option<TextQueries>,
    rng: &mut Rng,
) -> Result<usize, CalibError> {
    let groups = {
        let mut vi = lock(index);
        vi.sync(&lock(db))?;
        vi.groups()
    };
    let prev = sources(&lock(db))?;
    let key = |kind: QueryKind, g: &(Space, String, usize)| {
        (kind.as_str().to_string(), g.0.id.clone(), g.1.clone())
    };
    let mut done = 0;

    if let Some(t) = text {
        let fingerprint = &blake3::hash(QUERY_LIST.as_bytes()).to_hex()[..16];
        // 查询表改了也要重算：来源带上查询表的指纹
        let source = format!("{}#{fingerprint}", t.space.id);
        let kind = QueryKind::Text;
        let targets: Vec<&(Space, String, usize)> = groups
            .iter()
            .filter(|g| comparable(t.space, &g.0))
            .filter(|g| g.2 >= TEXT_MIN_VECTORS && stale(prev.get(&key(kind, g)), &source, g.2))
            .collect();
        if !targets.is_empty() {
            let qv = query_vectors(db, t.space, t.embed)?;
            for g in targets {
                let ids = lock(index).sample(&g.0.id, &g.1, SAMPLE, rng);
                let vecs = read_vectors(&lock(db), &g.0.id, &ids)?;
                if vecs.len() < TEXT_MIN_VECTORS {
                    continue;
                }
                if let Some(s) = stat(&qv, &vecs, false) {
                    save(&lock(db), kind, g, &source, s)?;
                    done += 1;
                }
            }
        }
    }

    for g in &groups {
        if !stale(prev.get(&key(QueryKind::Similar, g)), &g.0.id, g.2) {
            continue;
        }
        let ids = lock(index).sample(&g.0.id, &g.1, SAMPLE, rng);
        let vecs = read_vectors(&lock(db), &g.0.id, &ids)?;
        if vecs.len() < MIN_VECTORS {
            continue;
        }
        let queries = &vecs[..vecs.len().min(SIMILAR_QUERIES)];
        if let Some(s) = stat(queries, &vecs, true) {
            save(&lock(db), QueryKind::Similar, g, &g.0.id, s)?;
            done += 1;
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::tests::{Fixture, space, text_space, unit};

    /// 与 `unit(0)` 的余弦为 `cos`、另一分量落在第 `k` 维的单位向量
    fn at(cos: f32, k: usize) -> Vec<f32> {
        let mut v = unit(0);
        v[0] = cos;
        v[k] = (1.0 - cos * cos).sqrt();
        v
    }

    #[test]
    fn query_list_is_large_and_unique() {
        let q = queries();
        assert!((180..=260).contains(&q.len()), "{}", q.len());
        let mut sorted = q.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), q.len());
        // 中英混合
        assert!(q.iter().filter(|s| s.is_ascii()).count() >= 30);
    }

    #[test]
    fn stat_and_z() {
        // 余弦为 0.1、0.3 两种，均值 0.2，标准差 0.1
        let q = vec![unit(0)];
        let t = vec![at(0.1, 1), at(0.3, 2)];
        let s = stat(&q, &t, false).unwrap();
        assert!(
            (s.mean - 0.2).abs() < 1e-6 && (s.std - 0.1).abs() < 1e-6,
            "{s:?}"
        );
        assert!((s.z(0.5) - 3.0).abs() < 1e-4);
        assert!((s.z(0.2)).abs() < 1e-4);
        // 不与自身配对
        let v = vec![unit(0), at(0.5, 1), at(0.3, 2)];
        let s = stat(&v[..1], &v, true).unwrap();
        assert!((s.mean - 0.4).abs() < 1e-6, "{s:?}");
        // 配对太少或分布退化
        assert!(stat(&v[..1], &v[..1], true).is_none());
        assert!(stat(&q, &[at(0.2, 1), at(0.2, 2)], false).is_none());
    }

    #[test]
    fn load_skips_invalid_rows() {
        let f = Fixture::new();
        crate::db::insert_space(&f.db.lock().unwrap().conn, &text_space()).unwrap();
        for (modality, std) in [("text", 0.1), ("image", 0.0), ("audio", -1.0)] {
            f.db.lock()
                .unwrap()
                .conn
                .execute(
                    "INSERT INTO calib (kind, space_id, modality, source, mean, std, vectors, updated_at)
                     VALUES ('text', 't', ?1, '', 0.2, ?2, 100, 0)",
                    params![modality, std],
                )
                .unwrap();
        }
        let stats = load(&f.db.lock().unwrap(), QueryKind::Text).unwrap();
        assert_eq!(stats.len(), 1);
        assert!(stats.contains_key(&("t".to_string(), "text".to_string())));
    }

    #[test]
    fn recompute_rules() {
        let prev = ("t".to_string(), 100);
        assert!(stale(None, "t", 1));
        assert!(!stale(Some(&prev), "t", 100));
        assert!(!stale(Some(&prev), "t", 120));
        assert!(!stale(Some(&prev), "t", 80));
        assert!(stale(Some(&prev), "t", 121));
        assert!(stale(Some(&prev), "t", 79));
        // 来源变了（换了文本空间或查询表）
        assert!(stale(Some(&prev), "u", 100));
    }

    #[test]
    fn sample_is_uniform_subset() {
        let f = Fixture::new();
        let r = f.root("D:\\图");
        let img = space("i", true);
        let mut ids = Vec::new();
        for i in 0..50 {
            let file = f.file(r, &format!("{i}.jpg"), "image", 0);
            ids.push(f.segment(file, "image", &unit(i % 768), &img));
        }
        let index = f.index();
        let vi = index.lock().unwrap();
        let mut rng = Rng::new(7);
        let s = vi.sample("i", "image", 10, &mut rng);
        assert_eq!(s.len(), 10);
        let mut uniq = s.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), 10);
        assert!(s.iter().all(|id| ids.contains(id)));
        // 不足时全取，顺序打乱
        let all = vi.sample("i", "image", 100, &mut rng);
        assert_eq!(all.len(), 50);
        assert_ne!(all, ids);
        assert!(vi.sample("i", "text", 10, &mut rng).is_empty());
        assert!(vi.sample("x", "image", 10, &mut rng).is_empty());
        // 多次抽样覆盖到全部记录
        let mut seen = std::collections::HashSet::new();
        for _ in 0..50 {
            seen.extend(vi.sample("i", "image", 5, &mut rng));
        }
        assert_eq!(seen.len(), 50);
    }

    #[test]
    fn update_computes_and_recomputes() {
        let f = Fixture::new();
        let r = f.root("D:\\资料");
        let t = text_space();
        let img = space("i", true);
        // 向量不够时不计算：文字查询的下限是 `TEXT_MIN_VECTORS`，similar 是 `MIN_VECTORS`
        let add_docs = |range: std::ops::Range<usize>| {
            for i in range {
                let doc = f.file(r, &format!("{i}.md"), "document", 0);
                f.segment(doc, "text", &at(0.4 + 0.01 * i as f32, i + 1), &t);
                let pic = f.file(r, &format!("{i}.jpg"), "image", 0);
                f.segment(pic, "image", &at(0.1 + 0.005 * i as f32, i + 1), &img);
            }
        };
        add_docs(0..TEXT_MIN_VECTORS - 1);
        let index = f.index();
        let calls = std::cell::Cell::new(0);
        let mut embed = |_: &str| {
            calls.set(calls.get() + 1);
            Ok(unit(0))
        };
        let mut rng = Rng::new(1);
        let run = |embed: &mut Embed, rng: &mut Rng| {
            update(&f.db, &index, Some(TextQueries { space: &t, embed }), rng).unwrap()
        };
        assert_eq!(run(&mut embed, &mut rng), 0);
        assert!(
            load(&f.db.lock().unwrap(), QueryKind::Text)
                .unwrap()
                .is_empty()
        );
        add_docs(TEXT_MIN_VECTORS - 1..MIN_VECTORS - 1);
        // 文字查询：文档、图片各一个；similar 还不够
        assert_eq!(run(&mut embed, &mut rng), 2);
        assert_eq!(calls.get(), queries().len());
        assert!(
            load(&f.db.lock().unwrap(), QueryKind::Similar)
                .unwrap()
                .is_empty()
        );
        let text = load(&f.db.lock().unwrap(), QueryKind::Text).unwrap();
        let doc = text[&("t".to_string(), "text".to_string())];
        assert!((doc.mean - 0.49).abs() < 1e-3, "{doc:?}");
        let pic = text[&("i".to_string(), "image".to_string())];
        assert!((pic.mean - 0.145).abs() < 1e-3, "{pic:?}");
        // similar：两个空间各一个；文字查询的向量数只多了约 5%，不重算
        add_docs(MIN_VECTORS - 1..MIN_VECTORS);
        assert_eq!(run(&mut embed, &mut rng), 2);
        assert_eq!(
            load(&f.db.lock().unwrap(), QueryKind::Similar)
                .unwrap()
                .len(),
            2
        );
        // 没有变化时不重算，查询向量取缓存
        assert_eq!(run(&mut embed, &mut rng), 0);
        assert_eq!(calls.get(), queries().len());
        // 图片增加 2 张（10%）不重算，再加 3 张（25%）重算
        let add = |n: usize| {
            for i in 0..n {
                let pic = f.file(r, &format!("new{n}-{i}.jpg"), "image", 0);
                f.segment(pic, "image", &at(0.9, 100 + i), &img);
            }
        };
        add(2);
        assert_eq!(run(&mut embed, &mut rng), 0);
        add(3);
        assert_eq!(run(&mut embed, &mut rng), 2);
        let pic2 = load(&f.db.lock().unwrap(), QueryKind::Text).unwrap()
            [&("i".to_string(), "image".to_string())];
        assert!(pic2.mean > pic.mean);
        assert_eq!(calls.get(), queries().len());
        // 推理出错时报错，不写入
        let other = Space {
            id: "t2".into(),
            backend: "vulkan".into(),
            ..t.clone()
        };
        let mut fail = |_: &str| Err("设备丢失".to_string());
        let err = update(
            &f.db,
            &index,
            Some(TextQueries {
                space: &other,
                embed: &mut fail,
            }),
            &mut rng,
        )
        .unwrap_err();
        assert!(matches!(err, CalibError::Embed(_)), "{err}");
        // 没有推理引擎时只算 similar
        assert_eq!(update(&f.db, &index, None, &mut rng).unwrap(), 0);
    }
}
