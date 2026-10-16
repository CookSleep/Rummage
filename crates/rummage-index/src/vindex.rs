//! 内存向量索引：只常驻粗排数据，768 维 f16 向量精排时再按片段从库中读取。
//! - 粗排数据：调用方用 `rummage_embed::vector::truncate` 把向量截断到 256 维、重新 L2 归一化（`NewSegment::coarse`、
//!   `Query::coarse`），这里按 `s = max|u| / 127`、`q = round(u / s)` 对称量化为 int8，与 f32 缩放系数一起写在 `vectors.coarse`（实测：粗排取前 200 名再 f16 精排，评测指标不变）。
//! - 每条记录带片段、文件、空间、模态，以及文件的过滤字段（种类、根目录、修改时间），过滤在粗排之前进行。
//!   `cloud` 不参与过滤，结果中的 `cloud`、`offline` 在组装结果时从库中读取。
//! - 同步：`load` 在连接上建临时表 `vindex_touched` 和临时触发器，`files` 的过滤字段变化、文件删除（含删除根目录的级联）、
//!   片段与向量的增删都把文件 id 记进去；`sync` 取出这些文件，删掉内存中它们的全部记录后按库中现状重新载入。
//!   临时表随事务回滚，只记录已提交的变化；所有写入都经由同一个 `Db` 连接，不会漏记。
//! - 同一个 `Db` 上只能有一个 `VIndex`：`load` 会清空变化记录，另一个实例此前未同步的变化就丢了。
//! - 锁的顺序：先取内存索引的锁，再取数据库的锁。
use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension};

use crate::db::{Db, DbError, Space};
use crate::search::Filter;

/// 粗排向量的维数，与 `rummage_embed::vector::COARSE_DIM` 相同（核心中有编译期检查）
pub const COARSE_DIM: usize = 256;

/// 把截断并重新归一化后的粗排向量 `u`（`rummage_embed::vector::truncate` 的结果）量化为 int8，后接 f32 小端缩放系数，
/// 共 260 字节。不足 256 维的补 0，超出的不用；全零向量的缩放系数为 0
pub fn quantize(u: &[f32]) -> Vec<u8> {
    let (code, scale) = quantize_code(u);
    let mut out: Vec<u8> = code.iter().map(|&c| c as u8).collect();
    out.extend_from_slice(&scale.to_le_bytes());
    out
}

/// `quantize` 的码与缩放系数
fn quantize_code(u: &[f32]) -> ([i8; COARSE_DIM], f32) {
    let u = &u[..u.len().min(COARSE_DIM)];
    let max = u.iter().fold(0f32, |m, x| m.max(x.abs()));
    let scale = max / 127.0;
    let mut code = [0i8; COARSE_DIM];
    if scale > 0.0 {
        for (o, x) in code.iter_mut().zip(u) {
            *o = (x / scale).round().clamp(-127.0, 127.0) as i8;
        }
    }
    (code, scale)
}

fn dequantize(blob: &[u8]) -> Option<([i8; COARSE_DIM], f32)> {
    if blob.len() != COARSE_DIM + 4 {
        return None;
    }
    let code: [i8; COARSE_DIM] = std::array::from_fn(|i| blob[i] as i8);
    let scale = f32::from_le_bytes(blob[COARSE_DIM..].try_into().ok()?);
    Some((code, scale))
}

fn dot(a: &[i8; COARSE_DIM], b: &[i8; COARSE_DIM]) -> i32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| i32::from(*x) * i32::from(*y))
        .sum()
}

struct Entry {
    segment_id: i64,
    file_id: i64,
    root_id: i64,
    mtime: i64,
    scale: f32,
    /// `VIndex::spaces` 的下标
    space: u16,
    /// `VIndex::kinds` 的下标
    kind: u8,
    /// `VIndex::modalities` 的下标
    modality: u8,
    code: [i8; COARSE_DIM],
}

/// 粗排选出的候选片段
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Candidate {
    pub segment_id: i64,
    pub file_id: i64,
    pub space_id: String,
    /// 文档的内嵌媒体：所属文件是文档、模态不是 `text`（检索中并入文档，`search` 模块说明）
    pub embedded: bool,
    /// 按本次查询自身的分数分布换算的 z（`coarse` 的 `adaptive`）；没有要求或该空间向量太少时为空
    pub z: Option<f32>,
}

#[derive(Default)]
pub struct VIndex {
    entries: Vec<Entry>,
    spaces: Vec<Space>,
    kinds: Vec<String>,
    modalities: Vec<String>,
}

const TRACK: &str = "
CREATE TEMP TABLE IF NOT EXISTS vindex_touched (file_id INTEGER PRIMARY KEY);
CREATE TEMP TRIGGER IF NOT EXISTS vindex_files_update AFTER UPDATE OF kind, root_id, mtime ON files BEGIN
  INSERT OR IGNORE INTO vindex_touched VALUES (new.id);
END;
CREATE TEMP TRIGGER IF NOT EXISTS vindex_files_delete AFTER DELETE ON files BEGIN
  INSERT OR IGNORE INTO vindex_touched VALUES (old.id);
END;
CREATE TEMP TRIGGER IF NOT EXISTS vindex_segments_insert AFTER INSERT ON segments BEGIN
  INSERT OR IGNORE INTO vindex_touched VALUES (new.file_id);
END;
CREATE TEMP TRIGGER IF NOT EXISTS vindex_segments_delete AFTER DELETE ON segments BEGIN
  INSERT OR IGNORE INTO vindex_touched VALUES (old.file_id);
END;
CREATE TEMP TRIGGER IF NOT EXISTS vindex_vectors_insert AFTER INSERT ON vectors BEGIN
  INSERT OR IGNORE INTO vindex_touched SELECT file_id FROM segments WHERE id = new.segment_id;
END;
CREATE TEMP TRIGGER IF NOT EXISTS vindex_vectors_delete AFTER DELETE ON vectors BEGIN
  INSERT OR IGNORE INTO vindex_touched SELECT file_id FROM segments WHERE id = old.segment_id;
END;
";

const ROWS: &str =
    "SELECT s.id, s.file_id, s.modality, v.space_id, v.coarse, f.kind, f.root_id, f.mtime
  FROM segments s JOIN vectors v ON v.segment_id = s.id JOIN files f ON f.id = s.file_id";

fn intern(list: &mut Vec<String>, value: &str) -> usize {
    match list.iter().position(|x| x == value) {
        Some(idx) => idx,
        None => {
            list.push(value.to_string());
            list.len() - 1
        }
    }
}

pub(crate) fn space_row(conn: &Connection, id: &str) -> rusqlite::Result<Option<Space>> {
    conn.query_row(
        "SELECT id, family, model_sha256, mmproj_sha256, runtime, backend, precision, prefix_scheme, preprocess
         FROM spaces WHERE id = ?1",
        [id],
        |row| {
            Ok(Space {
                id: row.get(0)?,
                family: row.get(1)?,
                model_sha256: row.get(2)?,
                mmproj_sha256: row.get(3)?,
                runtime: row.get(4)?,
                backend: row.get(5)?,
                precision: row.get(6)?,
                prefix_scheme: row.get(7)?,
                preprocess: row.get(8)?,
            })
        },
    )
    .optional()
}

impl VIndex {
    /// 开始记录变化并载入全部粗排数据；在数据库锁内、流水线启动前调用
    pub fn load(db: &Db) -> Result<VIndex, DbError> {
        let mut vi = VIndex::default();
        let res = db.conn.execute_batch(TRACK).and_then(|()| {
            let tx = db.conn.unchecked_transaction()?;
            tx.execute("DELETE FROM vindex_touched", [])?;
            vi.read(&tx, ROWS)?;
            tx.commit()
        });
        res.map_err(|err| db.err(err))?;
        Ok(vi)
    }

    /// 载入上次同步之后有变化的文件
    pub fn sync(&mut self, db: &Db) -> Result<(), DbError> {
        let res = db.conn.unchecked_transaction().and_then(|tx| {
            let ids: HashSet<i64> = tx
                .prepare("SELECT file_id FROM vindex_touched")?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            if ids.is_empty() {
                return Ok(());
            }
            self.entries.retain(|e| !ids.contains(&e.file_id));
            self.read(
                &tx,
                &format!("{ROWS} WHERE s.file_id IN (SELECT file_id FROM vindex_touched)"),
            )?;
            tx.execute("DELETE FROM vindex_touched", [])?;
            tx.commit()
        });
        res.map_err(|err| db.err(err))
    }

    fn read(&mut self, conn: &Connection, sql: &str) -> rusqlite::Result<()> {
        let mut stmt = conn.prepare(sql)?;
        let mut rows = stmt.query([])?;
        let mut bad = 0;
        while let Some(row) = rows.next()? {
            let space_id: String = row.get(3)?;
            let blob: Vec<u8> = row.get(4)?;
            let Some((code, scale)) = dequantize(&blob) else {
                bad += 1;
                continue;
            };
            let space = match self.spaces.iter().position(|s| s.id == space_id) {
                Some(idx) => idx,
                None => {
                    // vectors.space_id 有外键约束，空间行总在
                    let Some(s) = space_row(conn, &space_id)? else {
                        bad += 1;
                        continue;
                    };
                    self.spaces.push(s);
                    self.spaces.len() - 1
                }
            };
            let modality: String = row.get(2)?;
            let kind: String = row.get(5)?;
            self.entries.push(Entry {
                segment_id: row.get(0)?,
                file_id: row.get(1)?,
                root_id: row.get(6)?,
                mtime: row.get(7)?,
                scale,
                space: space as u16,
                kind: intern(&mut self.kinds, &kind) as u8,
                modality: intern(&mut self.modalities, &modality) as u8,
                code,
            });
        }
        if bad > 0 {
            log::warn!("{bad} 条向量的粗排数据无效，已跳过");
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 各“空间 × 模态”的向量数，分数校准据此决定何时重算（`calib`）
    pub(crate) fn groups(&self) -> Vec<(Space, String, usize)> {
        let mut counts: Vec<Vec<usize>> = vec![vec![0; self.modalities.len()]; self.spaces.len()];
        for e in &self.entries {
            counts[usize::from(e.space)][usize::from(e.modality)] += 1;
        }
        let mut out = Vec::new();
        for (s, row) in counts.iter().enumerate() {
            for (m, &n) in row.iter().enumerate() {
                if n > 0 {
                    out.push((self.spaces[s].clone(), self.modalities[m].clone(), n));
                }
            }
        }
        out
    }

    /// 从 `space_id` 空间、`modality` 模态的记录中均匀随机抽取至多 `n` 个片段 id
    /// （蓄水池抽样），结果顺序也是随机的
    pub(crate) fn sample(
        &self,
        space_id: &str,
        modality: &str,
        n: usize,
        rng: &mut crate::calib::Rng,
    ) -> Vec<i64> {
        let s = self.spaces.iter().position(|x| x.id == space_id);
        let m = self.modalities.iter().position(|x| x == modality);
        let (Some(s), Some(m)) = (s, m) else {
            return Vec::new();
        };
        let wanted = |e: &Entry| usize::from(e.space) == s && usize::from(e.modality) == m;
        let mut out: Vec<i64> = Vec::new();
        let mut seen = 0usize;
        for e in &self.entries {
            if !wanted(e) {
                continue;
            }
            seen += 1;
            if out.len() < n {
                out.push(e.segment_id);
                continue;
            }
            let j = rng.below(seen);
            if j < n {
                out[j] = e.segment_id;
            }
        }
        // 填满之前按库中顺序放入，打乱后取前若干条作查询（`similar`）才不偏向先入库的文件
        for i in (1..out.len()).rev() {
            out.swap(i, rng.below(i + 1));
        }
        out
    }

    /// 在 `modality` 模态、与查询可比较的空间（`search::comparable`）、满足过滤条件的记录中做 int8 粗排，取前 `k` 名。
    /// `adaptive` 时（以图搜图与“找相似的”）顺带统计查询与每个空间中该模态全部记录
    /// 粗排分数的均值、标准差，把候选的粗排分数换成 z。统计不受种类、文件夹、时间筛选影响（同一文件的匹配度不随筛选变化，
    /// 筛选后剩下的记录也可能太少），但不含被拿来查的文件；记录少于 `calib::MIN_VECTORS` 的空间不算，由调用方退回背景分布。
    /// z 用粗排分数而不用精排余弦：截断到 256 维后余弦的分布与 768 维不同，两者混用会有系统偏差。
    /// `query` 为查询的粗排向量（`Query::coarse`）
    pub(crate) fn coarse(
        &self,
        query: &[f32],
        space: &Space,
        modality: &str,
        filter: &Filter,
        k: usize,
        adaptive: bool,
    ) -> Vec<Candidate> {
        let Some(m) = self.modalities.iter().position(|x| x == modality) else {
            return Vec::new();
        };
        let spaces: Vec<bool> = self
            .spaces
            .iter()
            .map(|s| crate::search::comparable(space, s))
            .collect();
        let kinds: Vec<bool> = self
            .kinds
            .iter()
            .map(|x| filter.kinds.is_empty() || filter.kinds.contains(x))
            .collect();
        let (q, _) = quantize_code(query);
        let mut scored: Vec<(f32, u32)> = Vec::new();
        // 每个空间的（记录数, 分数和, 分数平方和）
        let mut sums = vec![(0usize, 0f64, 0f64); self.spaces.len()];
        for (idx, e) in self.entries.iter().enumerate() {
            if usize::from(e.modality) != m
                || !spaces[usize::from(e.space)]
                || filter.exclude_file == Some(e.file_id)
            {
                continue;
            }
            let wanted = kinds[usize::from(e.kind)]
                && (filter.root_ids.is_empty() || filter.root_ids.contains(&e.root_id))
                && filter.since.is_none_or(|t| e.mtime >= t)
                && filter.until.is_none_or(|t| e.mtime <= t);
            if !wanted && !adaptive {
                continue;
            }
            // 查询的缩放系数对所有记录相同，不影响排序，也不影响 z
            let score = dot(&q, &e.code) as f32 * e.scale;
            if adaptive {
                let acc = &mut sums[usize::from(e.space)];
                acc.0 += 1;
                acc.1 += f64::from(score);
                acc.2 += f64::from(score) * f64::from(score);
            }
            if wanted {
                scored.push((score, idx as u32));
            }
        }
        let stats: Vec<Option<crate::calib::Stat>> = sums
            .iter()
            .map(|&(n, sum, sq)| {
                if n < crate::calib::MIN_VECTORS {
                    return None;
                }
                let mean = sum / n as f64;
                let std = (sq / n as f64 - mean * mean).max(0.0).sqrt();
                (mean.is_finite() && std.is_finite() && std > 0.0)
                    .then_some(crate::calib::Stat { mean, std })
            })
            .collect();
        let by_score = |a: &(f32, u32), b: &(f32, u32)| b.0.total_cmp(&a.0);
        if scored.len() > k && k > 0 {
            scored.select_nth_unstable_by(k - 1, by_score);
        }
        scored.truncate(k);
        scored.sort_unstable_by(by_score);
        scored
            .iter()
            .map(|&(score, idx)| {
                let e = &self.entries[idx as usize];
                Candidate {
                    segment_id: e.segment_id,
                    file_id: e.file_id,
                    space_id: self.spaces[usize::from(e.space)].id.clone(),
                    embedded: self.kinds[usize::from(e.kind)] == "document" && modality != "text",
                    z: stats[usize::from(e.space)].map(|s| s.z(score)),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::tests::{Fixture, space, text_space, unit};

    #[test]
    fn quantize_matches_p1_1() {
        // 已截断归一化的粗排向量 (0.8, -0.6)，第 256 维之后的值不用
        let mut v = vec![0f32; 768];
        v[0] = 0.8;
        v[1] = -0.6;
        v[300] = 0.8;
        let blob = quantize(&v);
        assert_eq!(blob.len(), 260);
        let (code, scale) = dequantize(&blob).unwrap();
        // s = 0.8 / 127
        assert!((scale - 0.8 / 127.0).abs() < 1e-7);
        assert_eq!(code[0], 127);
        assert_eq!(code[1], (-0.6f32 / scale).round() as i8);
        assert_eq!(code[1], -95);
        assert!(code[2..].iter().all(|&c| c == 0));
        // 第 256 维之后不参与
        v[300] = -5.0;
        assert_eq!(quantize(&v), blob);
        // 反量化后与原向量很接近
        let r: Vec<f32> = (0..256).map(|i| ((i as f32) * 0.37).sin()).collect();
        let n = r.iter().map(|x| x * x).sum::<f32>().sqrt();
        let r: Vec<f32> = r.iter().map(|x| x / n).collect();
        let (code, scale) = dequantize(&quantize(&r)).unwrap();
        for (c, x) in code.iter().zip(&r) {
            assert!((f32::from(*c) * scale - x).abs() <= scale / 2.0 + 1e-6);
        }
        assert!(code.iter().all(|&c| c >= -127));
        // 短向量补 0，全零向量缩放系数为 0
        assert_eq!(dequantize(&quantize(&[0.0, 2.0])).unwrap().0[1], 127);
        let (code, scale) = dequantize(&quantize(&[0.0; 4])).unwrap();
        assert_eq!(scale, 0.0);
        assert!(code.iter().all(|&c| c == 0));
        assert!(dequantize(&[0; 10]).is_none());
    }

    fn ids(vi: &VIndex) -> Vec<i64> {
        let mut v: Vec<i64> = vi.entries.iter().map(|e| e.segment_id).collect();
        v.sort();
        v
    }

    fn reload(f: &Fixture) -> Vec<i64> {
        ids(&VIndex::load(&f.db.lock().unwrap()).unwrap())
    }

    #[test]
    fn stays_in_sync() {
        let f = Fixture::new();
        let r1 = f.root("D:\\一");
        let r2 = f.root("D:\\二");
        let a = f.file(r1, "a.txt", "document", 100);
        let b = f.file(r1, "b.jpg", "image", 200);
        let c = f.file(r2, "c.md", "document", 300);
        f.segment(a, "text", &unit(0), &text_space());
        f.segment(a, "text", &unit(1), &text_space());
        let mut vi = VIndex::load(&f.db.lock().unwrap()).unwrap();
        assert_eq!(vi.len(), 2);

        // 写入新文件、复制、改过滤字段
        f.segment(b, "image", &unit(2), &space("img", true));
        let sc = f.segment(c, "text", &unit(3), &text_space());
        let db = f.db.lock().unwrap();
        db.conn
            .execute("UPDATE files SET mtime = 999 WHERE id = ?1", [a])
            .unwrap();
        vi.sync(&db).unwrap();
        drop(db);
        assert_eq!(ids(&vi), reload(&f));
        assert_eq!(vi.len(), 4);
        assert!(
            vi.entries
                .iter()
                .filter(|e| e.file_id == a)
                .all(|e| e.mtime == 999)
        );

        // 重新建索引（先删旧片段）、删除文件
        let db = f.db.lock().unwrap();
        db.conn
            .execute("DELETE FROM segments WHERE file_id = ?1", [a])
            .unwrap();
        db.conn
            .execute("DELETE FROM files WHERE id = ?1", [b])
            .unwrap();
        drop(db);
        let sa = f.segment(a, "text", &unit(4), &text_space());
        vi.sync(&f.db.lock().unwrap()).unwrap();
        assert_eq!(ids(&vi), [sc, sa]);
        assert_eq!(ids(&vi), reload(&f));

        // 回滚的事务不留下记录
        let db = f.db.lock().unwrap();
        {
            let tx = db.conn.unchecked_transaction().unwrap();
            tx.execute("DELETE FROM files WHERE id = ?1", [c]).unwrap();
        }
        let n: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM vindex_touched", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);

        // 删除根目录
        db.remove_root(r2).unwrap();
        vi.sync(&db).unwrap();
        drop(db);
        assert_eq!(ids(&vi), [sa]);
        assert_eq!(ids(&vi), reload(&f));
        // 没有变化时什么也不做
        vi.sync(&f.db.lock().unwrap()).unwrap();
        assert_eq!(ids(&vi), [sa]);
    }

    /// 30 万条随机粗排向量的粗排耗时：
    ///   cargo test -p rummage-index --release coarse_bench -- --ignored --nocapture
    #[test]
    #[ignore = "基准测试，耗时较长"]
    fn coarse_bench() {
        let mut vi = VIndex {
            spaces: vec![text_space(), space("img", true)],
            kinds: vec!["document".into(), "image".into()],
            modalities: vec!["text".into(), "image".into()],
            ..VIndex::default()
        };
        // 线性同余生成器，免得引入随机数依赖
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rand = move || {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((seed >> 33) as f32 / (1u64 << 31) as f32) - 0.5
        };
        let n = 300_000;
        for i in 0..n {
            let v: Vec<f32> = (0..COARSE_DIM).map(|_| rand()).collect();
            let (code, scale) = dequantize(&quantize(&v)).unwrap();
            let image = i % 3 == 0;
            vi.entries.push(Entry {
                segment_id: i as i64,
                file_id: i as i64,
                root_id: 1,
                mtime: i as i64,
                scale,
                space: u16::from(image),
                kind: u8::from(image),
                modality: u8::from(image),
                code,
            });
        }
        let query: Vec<f32> = (0..768).map(|_| rand()).collect();
        let filter = Filter::default();
        let ts = text_space();
        for round in 0..3 {
            let t = std::time::Instant::now();
            let a = vi.coarse(&query, &ts, "text", &filter, 200, false);
            let b = vi.coarse(&query, &ts, "image", &filter, 200, false);
            println!(
                "第 {round} 轮：{n} 条，文本 + 图片两次粗排共 {:.1} 毫秒（候选 {} + {}）",
                t.elapsed().as_secs_f64() * 1000.0,
                a.len(),
                b.len()
            );
        }
        println!(
            "每条记录 {} 字节，粗排数据常驻约 {:.1} MB",
            std::mem::size_of::<Entry>(),
            (std::mem::size_of::<Entry>() * n) as f64 / 1e6
        );
    }
}
