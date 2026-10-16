//! 索引作业：索引流水线对 `jobs` 及相关表的读写。
//! - 状态：`pending → preprocessing → embedding → stored`。失败时 `attempts + 1` 并回到 `pending`，累计
//!   `MAX_ATTEMPTS` 次后为 `failed`，同时以 `quick_hash` 为键写入黑名单；文件内容变化后 `quick_hash` 不同，
//!   自然不再命中。扫描在文件变化时把作业重置为 `pending`、`attempts = 0`。黑名单会清理：已没有文件使用的
//!   `quick_hash` 留到索引全部完成后由 `drop_unused_blacklist` 清理。不在文件删除或扫描清空 `quick_hash` 时立即删：
//!   只改了修改时间的文件、以及移动或改名后按“删旧行加新行”处理的文件，重新算出相同的 `quick_hash` 后仍要命中。
//! - `claim_job` 按 `priority` 从高到低取一个 `pending` 作业并置为 `preprocessing`；跳过离线根目录与占位文件，
//!   不处理图片、音频、视频时（没有 mmproj 或关闭了该类）跳过它们。
//! - 重建作业（设置改了以后按新参数重算的已完成文件，`rebuild`）：优先级在原值上减去 `REBUILD_SHIFT`，排在所有新增与
//!   内容变化的文件之后，同类内先后不变；扫描在文件变化时按正常优先级重新排队，标记随之消失。重建期间旧片段与向量
//!   保留、照常可搜，统计时算作已完成（`job_counts`），也不算未完成（`has_unfinished_jobs`）。
//! - 结果由写库线程批量交给 `apply`，一批一个事务。写入前核对文件仍是取作业时的那个（id、根目录、相对路径、
//!   大小、修改时间都相同）且作业仍在处理中：根目录被移除、文件被删除、处理期间文件变化（扫描已重新排队）
//!   或作业已被放回时，丢弃该结果。
//! - 崩溃恢复：核心启动时 `recover_jobs` 把停在 `preprocessing` / `embedding` 的作业计一次失败；
//!   正常退出时 `release_jobs` 把它们放回 `pending`，不计失败。`quick_hash` 在进入 `embedding` 时写入 `files`，
//!   在预处理阶段就崩溃的文件还没有它，失败满 3 次时只置为 `failed`、不进黑名单。
//! - 文件 ID（`Claimed::volume_file_id`，用于识别改名与移动）随任何写入的结果一起写回 `files`。
use std::collections::BTreeMap;
use std::path::PathBuf;

use rusqlite::{Connection, OptionalExtension, params};

use crate::db::{self, Db, DbError, Space, now_ms, vec_to_blob};
use crate::vindex::quantize;

/// 同一文件连续失败多少次后进入黑名单
pub const MAX_ATTEMPTS: i64 = 3;

/// 进入黑名单时记下的失败原因
pub fn blacklisted() -> String {
    format!("连续失败 {MAX_ATTEMPTS} 次，不再处理此文件；文件修改后会重新尝试。")
}
const CRASHED: &str = "上次处理此文件时 Rummage 意外退出。";
/// 出现在“失败文件”列表中、计入失败数的作业（`j` 为 `jobs`）：失败的，以及有内嵌媒体无法识别的已完成文档
pub(crate) const FAILED: &str =
    "(j.stage = 'failed' OR (j.stage = 'stored' AND j.media_failed > 0))";
pub const ENCRYPTED: &str = "此文档已加密或受密码保护，只能按文件名搜索。";
const ACTIVE: &str = "('preprocessing', 'embedding')";
/// 文档作业要算的部分（`jobs.parts`）：正文
pub const PART_TEXT: u8 = 1;
/// 文档作业要算的部分：内嵌媒体
pub const PART_MEDIA: u8 = 2;
/// 重建作业的优先级在原值上减去它。正常的优先级在 [−2^42, 3·2^42) 内（`scan::priority`），减去后都低于
/// `REBUILD_BELOW`，据此识别重建作业
pub(crate) const REBUILD_SHIFT: i64 = 1 << 45;
pub(crate) const REBUILD_BELOW: i64 = -(1 << 44);
/// 整图向量（`v` 为 `vectors`）：在 `?3` 空间中，参数版本即该空间的预处理版本
const WHOLE: &str =
    "v.space_id = ?3 AND v.params_version = (SELECT preprocess FROM spaces WHERE id = ?3)";

/// 取出的作业；`root_id`、`rel_path`、`size`、`mtime` 用于写入前核对文件没有变化
#[derive(Debug, Clone, PartialEq)]
pub struct Claimed {
    pub file_id: i64,
    pub root_id: i64,
    pub rel_path: String,
    /// 根目录与相对路径拼成的完整路径
    pub path: PathBuf,
    pub name: String,
    /// 小写、不含点
    pub ext: String,
    pub size: i64,
    pub mtime: i64,
    /// 预处理算 `quick_hash` 时顺便取得的文件 ID（`hash::quick_hash_with_id`），随结果写回 `files`；领取时为空
    pub volume_file_id: Option<String>,
    /// 文档要算的部分：`PART_TEXT`、`PART_MEDIA` 按位或；其他种类为两者都有
    pub parts: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewSegment {
    /// `text`、`image`、`audio`、`video_av`（带音频的视频窗口）或 `video_v`（纯画面）
    pub modality: &'static str,
    pub locator_json: String,
    /// 文本片段的正文；图片与音频为空
    pub text: Option<String>,
    /// 已归一化，以 f16 存储
    pub vector: Vec<f32>,
    /// 粗排向量：`vector` 经 `rummage_embed::vector::truncate` 截断并重新归一化，量化后存入 `vectors.coarse`（`vindex`）
    pub coarse: Vec<f32>,
    /// 片段自己的空间与参数版本：长图切片（参数版本带切分参数）、文本片段（参数版本带分段参数，
    /// `rummage_embed::space::text_version`）。为空时用 `Outcome::Stored` 的 `space`，参数版本即其 `preprocess`
    pub space: Option<Box<(Space, String)>>,
}

/// 一个作业的处理结果；除 `Embedding` 外都是最终结果
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// 预处理完成，开始推理
    Embedding { quick_hash: Vec<u8> },
    /// 写入片段与向量（先删除该文件的旧片段，同一事务内替换）；`space.preprocess` 即 `params_version`，长图切片与文本片段见
    /// `NewSegment::space`。
    /// `size` 为图片解码后的宽高（已按 EXIF 方向转正）或视频转正后的宽高，其他种类为空；`meta_json` 为音频、视频的元数据
    /// 或文档的内嵌媒体标记，其他种类为空、不改动库中的值。
    /// `replace` 为空时替换该文件的全部片段；只算了文档的一部分（正文或内嵌媒体，`Claimed::parts`）时只删除列出的模态的旧片段。
    /// `more` 为真时文档的内嵌媒体只处理了一批（资源不够时分批）：写入这一批后作业放回 `pending`、只算内嵌媒体，
    /// 从 `meta_json` 中记下的断点继续
    Stored {
        kind: &'static str,
        quick_hash: Vec<u8>,
        content_hash: Option<Vec<u8>>,
        space: Box<Space>,
        segments: Vec<NewSegment>,
        size: Option<(u32, u32)>,
        meta_json: Option<String>,
        replace: Option<Vec<&'static str>>,
        more: bool,
        /// 文档内嵌媒体中这一批无法识别的个数，与是否累加到前几批上（接着上一批时）；没算内嵌媒体时为空
        media_failed: Option<(u32, bool)>,
    },
    /// 内容相同的图片、音频或视频已有向量，复制文件 `from` 的片段、宽高、元数据与向量：`space_id` 空间中参数版本即该空间
    /// 预处理版本的向量（整图、音频段、视频窗口），以及参数版本为 `slice_version` 的长图切片（见 `Db::copy_source`；
    /// 音频与视频为空串）；
    /// 写入时 `from` 的内容哈希已变或已没有这样的向量，作业放回 `pending`
    Copied {
        kind: &'static str,
        quick_hash: Vec<u8>,
        content_hash: Vec<u8>,
        space_id: String,
        slice_version: String,
        from: i64,
    },
    /// `error` 是给用户看的中文整句
    Failed {
        quick_hash: Option<Vec<u8>>,
        error: String,
    },
    /// 命中黑名单，直接置为 `failed`
    Blacklisted { quick_hash: Vec<u8> },
    /// 放回 `pending`，不计失败（如推理后端已失效时排队中的文件）
    Released,
    /// 不做语义索引的种类（`other`）却有作业：删除作业，`status` 保持 `pending`
    Skipped,
    /// 加密或受密码保护的文档只保留元数据：作业直接置为 `failed` 并记下原因，不计失败次数、不进黑名单；
    /// 删除旧片段（级联删除向量），它们已不代表文件内容
    Encrypted { quick_hash: Vec<u8> },
    /// `.ts` / `.mts` 实为 MPEG-TS 视频、而当前不处理视频（关闭了视频或没有 mmproj）：种类改为 `video`，删除作业与旧片段；
    /// 开启后扫描按视频补建作业。处理视频时流水线直接按视频建索引（`Stored` 的种类为 `video`）
    Video,
    /// `.ts` / `.mts` 不是视频：种类为 `other`，记下 `quick_hash` 表示已判定（扫描据此不再排队，`scan::KINDS`），
    /// 删除作业与旧片段
    NotVideo { quick_hash: Vec<u8> },
    /// 音频、视频超过时长上限，或图片超过像素上限（`meta_json` 记下所用的上限 `pixel_limit`）：只记录元数据，
    /// `status = 'too_large'`，删除作业与旧片段；扫描按记下的时长或上限重新判定（`scan`）
    TooLong {
        quick_hash: Vec<u8>,
        meta_json: String,
    },
}

impl Outcome {
    pub fn is_final(&self) -> bool {
        !matches!(self, Outcome::Embedding { .. })
    }

    /// 结果中带的 `quick_hash`
    pub fn quick_hash(&self) -> Option<&[u8]> {
        match self {
            Outcome::Embedding { quick_hash }
            | Outcome::Stored { quick_hash, .. }
            | Outcome::Copied { quick_hash, .. }
            | Outcome::Blacklisted { quick_hash }
            | Outcome::Encrypted { quick_hash }
            | Outcome::NotVideo { quick_hash }
            | Outcome::TooLong { quick_hash, .. } => Some(quick_hash),
            Outcome::Failed { quick_hash, .. } => quick_hash.as_deref(),
            _ => None,
        }
    }
}

/// 按作业表统计（`scan` 模块文档）：`pending` 含正在处理的
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JobCounts {
    pub total: u64,
    pub done: u64,
    pub failed: u64,
    pub pending: u64,
}

/// 计一次失败；返回作业是否回到了 `pending`
fn fail(
    conn: &Connection,
    file_id: i64,
    quick_hash: Option<&[u8]>,
    error: &str,
    now: i64,
) -> rusqlite::Result<bool> {
    let attempts: i64 = conn.query_row(
        "SELECT attempts + 1 FROM jobs WHERE file_id = ?1",
        [file_id],
        |row| row.get(0),
    )?;
    let failed = attempts >= MAX_ATTEMPTS;
    conn.execute(
        "UPDATE jobs SET stage = ?2, attempts = ?3, last_error = ?4, updated_at = ?5 WHERE file_id = ?1",
        params![
            file_id,
            if failed { "failed" } else { "pending" },
            attempts,
            error,
            now
        ],
    )?;
    if let Some(hash) = quick_hash {
        conn.execute(
            "UPDATE files SET quick_hash = ?2 WHERE id = ?1",
            params![file_id, hash],
        )?;
        if failed {
            conn.execute(
                "INSERT INTO blacklist (quick_hash, reason, failures, updated_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (quick_hash) DO UPDATE SET reason = excluded.reason,
                   failures = excluded.failures, updated_at = excluded.updated_at",
                params![hash, error, attempts, now],
            )?;
        }
    }
    Ok(!failed)
}

/// 文件仍是取作业时的那个，且作业仍在处理中
fn current(conn: &Connection, job: &Claimed) -> rusqlite::Result<bool> {
    conn.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM files f JOIN jobs j ON j.file_id = f.id
               WHERE f.id = ?1 AND f.root_id = ?2 AND f.rel_path = ?3 AND f.size = ?4 AND f.mtime = ?5
                 AND j.stage IN {ACTIVE})"
        ),
        params![job.file_id, job.root_id, job.rel_path, job.size, job.mtime],
        |row| row.get(0),
    )
}

fn insert_segment(
    conn: &Connection,
    file_id: i64,
    modality: &str,
    locator: &str,
    text: Option<&str>,
    now: i64,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO segments (file_id, modality, locator_json, text, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![file_id, modality, locator, text, now],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 置为已完成：写入种类、哈希与状态，作业 `stored`
fn finish(
    conn: &Connection,
    file_id: i64,
    kind: &str,
    quick_hash: &[u8],
    content_hash: Option<&[u8]>,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE files SET kind = ?2, status = 'stored', quick_hash = ?3, content_hash = ?4, updated_at = ?5
         WHERE id = ?1",
        params![file_id, kind, quick_hash, content_hash, now],
    )?;
    conn.execute(
        "UPDATE jobs SET stage = 'stored', attempts = 0, last_error = NULL, updated_at = ?2
         WHERE file_id = ?1",
        params![file_id, now],
    )?;
    Ok(())
}

/// 写入一个结果；返回作业是否回到了 `pending`
fn apply_one(conn: &Connection, job: &Claimed, out: &Outcome, now: i64) -> rusqlite::Result<bool> {
    let id = job.file_id;
    match out {
        Outcome::Embedding { quick_hash } => {
            conn.execute(
                "UPDATE jobs SET stage = 'embedding', updated_at = ?2 WHERE file_id = ?1",
                params![id, now],
            )?;
            conn.execute(
                "UPDATE files SET quick_hash = ?2 WHERE id = ?1",
                params![id, quick_hash],
            )?;
        }
        Outcome::Stored {
            kind,
            quick_hash,
            content_hash,
            space,
            segments,
            size,
            meta_json,
            replace,
            more,
            media_failed,
        } => {
            db::insert_space(conn, space)?;
            // 级联删除旧向量；只算了文档的一部分时只删这部分的模态
            match replace {
                None => conn.execute("DELETE FROM segments WHERE file_id = ?1", [id])?,
                Some(mods) => conn.execute(
                    "DELETE FROM segments WHERE file_id = ?1
                       AND modality IN (SELECT value FROM json_each(?2))",
                    params![id, serde_json::to_string(mods).unwrap_or_default()],
                )?,
            };
            for seg in segments {
                let (sp, version) = match seg.space.as_deref() {
                    Some((sp, version)) => {
                        db::insert_space(conn, sp)?;
                        (sp, version)
                    }
                    None => (&**space, &space.preprocess),
                };
                let sid = insert_segment(
                    conn,
                    id,
                    seg.modality,
                    &seg.locator_json,
                    seg.text.as_deref(),
                    now,
                )?;
                conn.execute(
                    "INSERT INTO vectors (segment_id, space_id, dim, vec, params_version, coarse)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        sid,
                        sp.id,
                        seg.vector.len() as i64,
                        vec_to_blob(&seg.vector),
                        version,
                        quantize(&seg.coarse)
                    ],
                )?;
            }
            finish(conn, id, kind, quick_hash, content_hash.as_deref(), now)?;
            conn.execute(
                "UPDATE files SET width = ?2, height = ?3 WHERE id = ?1",
                params![id, size.map(|s| s.0), size.map(|s| s.1)],
            )?;
            if let Some(meta) = meta_json {
                conn.execute(
                    "UPDATE files SET meta_json = ?2 WHERE id = ?1",
                    params![id, meta],
                )?;
            }
            if let Some((n, resume)) = media_failed {
                conn.execute(
                    "UPDATE jobs SET media_failed = CASE WHEN ?2 THEN media_failed + ?3 ELSE ?3 END WHERE file_id = ?1",
                    params![id, resume, n],
                )?;
            }
            if *more {
                // 直接改为只算内嵌媒体，不保留 `requeue` 在处理期间并进来的正文位：这里依赖“正文参数变化时核心必定把
                // 正在处理的文档作废、放回”（`Pipeline::requeue` 的 `stale`），按旧正文参数算的结果不会走到这里
                conn.execute(
                    "UPDATE jobs SET stage = 'pending', parts = ?2, updated_at = ?3 WHERE file_id = ?1",
                    params![id, PART_MEDIA, now],
                )?;
                return Ok(true);
            }
        }
        Outcome::Copied {
            kind,
            quick_hash,
            content_hash,
            space_id,
            slice_version,
            from,
        } => {
            // 来源须仍是同一内容；第 5 列为是否整图片段
            let source: Vec<(i64, String, String, Option<String>, bool)> = conn
                .prepare(&format!(
                    "SELECT s.id, s.modality, s.locator_json, s.text,
                       EXISTS (SELECT 1 FROM vectors v WHERE v.segment_id = s.id AND {WHOLE})
                     FROM segments s JOIN files f ON f.id = s.file_id
                     WHERE s.file_id = ?1 AND f.content_hash = ?2 AND EXISTS (
                       SELECT 1 FROM vectors v WHERE v.segment_id = s.id AND ({WHOLE} OR v.params_version = ?4))
                     ORDER BY s.id"
                ))?
                .query_map(params![from, content_hash, space_id, slice_version], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            // 来源在这期间被删除、变化或已没有目标空间的整图向量：放回去重新计算，不计失败
            if !source.iter().any(|s| s.4) {
                conn.execute(
                    "UPDATE jobs SET stage = 'pending', updated_at = ?2 WHERE file_id = ?1",
                    params![id, now],
                )?;
                return Ok(true);
            }
            conn.execute("DELETE FROM segments WHERE file_id = ?1", [id])?;
            for (old, modality, locator, text, _) in source {
                let sid = insert_segment(conn, id, &modality, &locator, text.as_deref(), now)?;
                conn.execute(
                    &format!(
                        "INSERT INTO vectors (segment_id, space_id, dim, vec, params_version, coarse)
                         SELECT ?1, space_id, dim, vec, params_version, coarse FROM vectors v
                         WHERE segment_id = ?2 AND ({WHOLE} OR v.params_version = ?4)"
                    ),
                    params![sid, old, space_id, slice_version],
                )?;
            }
            finish(conn, id, kind, quick_hash, Some(content_hash), now)?;
            conn.execute(
                "UPDATE files SET (width, height, meta_json) = (SELECT width, height, meta_json FROM files WHERE id = ?2)
                 WHERE id = ?1",
                params![id, from],
            )?;
        }
        Outcome::Failed { quick_hash, error } => {
            return fail(conn, id, quick_hash.as_deref(), error, now);
        }
        Outcome::Blacklisted { quick_hash } => {
            // 显示黑名单中记下的失败原因，没有时才用笼统的说法
            conn.execute(
                "UPDATE jobs SET stage = 'failed', updated_at = ?3,
                   last_error = COALESCE((SELECT reason FROM blacklist WHERE quick_hash = ?4), ?2)
                 WHERE file_id = ?1",
                params![id, blacklisted(), now, quick_hash],
            )?;
            conn.execute(
                "UPDATE files SET quick_hash = ?2 WHERE id = ?1",
                params![id, quick_hash],
            )?;
        }
        Outcome::Released => {
            conn.execute(
                "UPDATE jobs SET stage = 'pending', updated_at = ?2 WHERE file_id = ?1",
                params![id, now],
            )?;
            return Ok(true);
        }
        Outcome::Skipped => {
            conn.execute("DELETE FROM jobs WHERE file_id = ?1", [id])?;
        }
        Outcome::Encrypted { quick_hash } => {
            conn.execute(
                "UPDATE jobs SET stage = 'failed', last_error = ?2, updated_at = ?3 WHERE file_id = ?1",
                params![id, ENCRYPTED, now],
            )?;
            conn.execute(
                "UPDATE files SET quick_hash = ?2 WHERE id = ?1",
                params![id, quick_hash],
            )?;
            conn.execute("DELETE FROM segments WHERE file_id = ?1", [id])?;
        }
        Outcome::Video => {
            conn.execute(
                "UPDATE files SET kind = 'video', updated_at = ?2 WHERE id = ?1",
                params![id, now],
            )?;
            conn.execute("DELETE FROM jobs WHERE file_id = ?1", [id])?;
            conn.execute("DELETE FROM segments WHERE file_id = ?1", [id])?;
        }
        Outcome::NotVideo { quick_hash } => {
            conn.execute(
                "UPDATE files SET kind = 'other', quick_hash = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, quick_hash, now],
            )?;
            conn.execute("DELETE FROM jobs WHERE file_id = ?1", [id])?;
            conn.execute("DELETE FROM segments WHERE file_id = ?1", [id])?;
        }
        Outcome::TooLong {
            quick_hash,
            meta_json,
        } => {
            conn.execute(
                "UPDATE files SET status = 'too_large', quick_hash = ?2, meta_json = ?3, updated_at = ?4
                 WHERE id = ?1",
                params![id, quick_hash, meta_json, now],
            )?;
            conn.execute("DELETE FROM jobs WHERE file_id = ?1", [id])?;
            conn.execute("DELETE FROM segments WHERE file_id = ?1", [id])?;
        }
    }
    Ok(false)
}

impl Db {
    /// 删除已没有文件使用的黑名单行（文件已删除，或内容变化后扫描清空了 `quick_hash`）。只在没有未完成的作业时调用
    /// （`thumbs_settled`）：重新排队的文件还没算出 `quick_hash`，内容可能没变、仍要命中黑名单
    pub fn drop_unused_blacklist(&self) -> Result<usize, DbError> {
        self.conn
            .execute(
                "DELETE FROM blacklist WHERE quick_hash NOT IN
                   (SELECT quick_hash FROM files WHERE quick_hash IS NOT NULL)",
                [],
            )
            .map_err(|err| self.err(err))
    }

    /// 核心启动时调用：停在处理中的作业计一次失败（核心崩溃时正在处理的文件）；返回作业数
    pub fn recover_jobs(&self) -> Result<usize, DbError> {
        let now = now_ms();
        let res = self.conn.unchecked_transaction().and_then(|tx| {
            let rows: Vec<(i64, Option<Vec<u8>>)> = tx
                .prepare(&format!(
                    "SELECT j.file_id, f.quick_hash FROM jobs j JOIN files f ON f.id = j.file_id
                     WHERE j.stage IN {ACTIVE}"
                ))?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (id, hash) in &rows {
                fail(&tx, *id, hash.as_deref(), CRASHED, now)?;
            }
            tx.commit()?;
            Ok(rows.len())
        });
        res.map_err(|err| self.err(err))
    }

    /// 正常退出时调用：处理中的作业放回 `pending`，不计失败
    pub fn release_jobs(&self) -> Result<(), DbError> {
        self.conn
            .execute(
                &format!("UPDATE jobs SET stage = 'pending' WHERE stage IN {ACTIVE}"),
                [],
            )
            .map_err(|err| self.err(err))?;
        Ok(())
    }

    /// 取优先级最高的 `pending` 作业并置为 `preprocessing`；`images`、`audio`、`video` 为否时（没有 mmproj 或关闭了该类）
    /// 不取图片、音频、视频
    pub fn claim_job(
        &self,
        images: bool,
        audio: bool,
        video: bool,
    ) -> Result<Option<Claimed>, DbError> {
        let row = self
            .conn
            .query_row(
                "SELECT j.file_id, f.root_id, r.path, f.rel_path, f.name, f.ext, f.size, f.mtime, j.parts
                 FROM jobs j JOIN files f ON f.id = j.file_id JOIN roots r ON r.id = f.root_id
                 WHERE j.stage = 'pending' AND r.offline = 0 AND f.cloud = 0
                   AND (?1 OR f.kind <> 'image') AND (?2 OR f.kind <> 'audio') AND (?3 OR f.kind <> 'video')
                 ORDER BY j.priority DESC LIMIT 1",
                [images, audio, video],
                |row| {
                    let root: String = row.get(2)?;
                    let rel: String = row.get(3)?;
                    Ok(Claimed {
                        file_id: row.get(0)?,
                        root_id: row.get(1)?,
                        path: PathBuf::from(root).join(&rel),
                        rel_path: rel,
                        name: row.get(4)?,
                        ext: row.get(5)?,
                        size: row.get(6)?,
                        mtime: row.get(7)?,
                        volume_file_id: None,
                        parts: row.get(8)?,
                    })
                },
            )
            .optional()
            .map_err(|err| self.err(err))?;
        if let Some(job) = &row {
            self.conn
                .execute(
                    "UPDATE jobs SET stage = 'preprocessing', updated_at = ?2 WHERE file_id = ?1",
                    params![job.file_id, now_ms()],
                )
                .map_err(|err| self.err(err))?;
        }
        Ok(row)
    }

    /// 文档的内嵌媒体分批处理到一半时记下的进度：当时所用设置的各项（`media_partial`）与下一个要处理的序号
    /// （`media_next`）；没有时为空
    pub fn media_progress(&self, file_id: i64) -> Result<Option<(Vec<String>, u32)>, DbError> {
        let row: Option<(Option<String>, Option<i64>)> = self
            .conn
            .query_row(
                "SELECT json_extract(meta_json, '$.media_partial'), json_extract(meta_json, '$.media_next')
                 FROM files WHERE id = ?1 AND json_valid(meta_json)",
                [file_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|err| self.err(err))?;
        let Some((Some(tokens), Some(next))) = row else {
            return Ok(None);
        };
        Ok(serde_json::from_str(&tokens)
            .ok()
            .zip(u32::try_from(next).ok()))
    }

    pub fn is_blacklisted(&self, quick_hash: &[u8]) -> Result<bool, DbError> {
        self.conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM blacklist WHERE quick_hash = ?1)",
                [quick_hash],
                |row| row.get(0),
            )
            .map_err(|err| self.err(err))
    }

    /// 另一个内容哈希相同、已有 `space_id` 空间整图向量的文件。按当前参数是长图（长边超过短边的 `max_ratio` 倍，
    /// 与 `rummage_media::slices` 的判定相同）时，还须有参数版本为 `slice_version` 的切片。没有记录宽高的来源无法判断，不用
    pub fn copy_source(
        &self,
        content_hash: &[u8],
        space_id: &str,
        slice_version: &str,
        max_ratio: f64,
        file_id: i64,
    ) -> Result<Option<i64>, DbError> {
        let has = |cond: &str| {
            format!(
                "EXISTS (SELECT 1 FROM segments s JOIN vectors v ON v.segment_id = s.id
                 WHERE s.file_id = f.id AND {cond})"
            )
        };
        self.conn
            .query_row(
                &format!(
                    "SELECT f.id FROM files f WHERE f.content_hash = ?1 AND f.id <> ?2
                       AND f.width > 0 AND f.height > 0 AND {}
                       AND (MAX(f.width, f.height) <= ?5 * MIN(f.width, f.height) OR {})
                     LIMIT 1",
                    has(WHOLE),
                    has("v.params_version = ?4")
                ),
                params![content_hash, file_id, space_id, slice_version, max_ratio],
                |row| row.get(0),
            )
            .optional()
            .map_err(|err| self.err(err))
    }

    /// 另一个内容哈希相同、已有 `space_id` 空间向量（参数版本即该空间的预处理版本）的音频或视频文件（做法同图片）。
    /// 全是静音、没有片段的文件不能作为来源
    pub fn media_copy_source(
        &self,
        content_hash: &[u8],
        space_id: &str,
        file_id: i64,
    ) -> Result<Option<i64>, DbError> {
        self.conn
            .query_row(
                &format!(
                    "SELECT f.id FROM files f WHERE f.content_hash = ?1 AND f.id <> ?2 AND EXISTS (
                       SELECT 1 FROM segments s JOIN vectors v ON v.segment_id = s.id
                       WHERE s.file_id = f.id AND {WHOLE})
                     LIMIT 1"
                ),
                params![content_hash, file_id, space_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|err| self.err(err))
    }

    /// 在一个事务中写入一批结果，跳过已不对应当前文件的（见模块文档）；返回是否有作业回到了 `pending`
    pub fn apply(&self, batch: &[(Claimed, Outcome)]) -> Result<bool, DbError> {
        let now = now_ms();
        let res = self.conn.unchecked_transaction().and_then(|tx| {
            let mut requeued = false;
            for (job, out) in batch {
                if current(&tx, job)? {
                    requeued |= apply_one(&tx, job, out, now)?;
                    if job.volume_file_id.is_some() {
                        tx.execute(
                            "UPDATE files SET volume_file_id = ?2 WHERE id = ?1",
                            params![job.file_id, job.volume_file_id],
                        )?;
                    }
                }
            }
            tx.commit()?;
            Ok(requeued)
        });
        res.map_err(|err| self.err(err))
    }

    /// 还有未完成（既不是 `stored` 也不是 `failed`）的作业；重建作业不算，它们的旧向量照常可搜
    pub fn has_unfinished_jobs(&self) -> Result<bool, DbError> {
        self.conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM jobs WHERE stage NOT IN ('stored', 'failed') AND priority >= ?1)",
                [REBUILD_BELOW],
                |row| row.get(0),
            )
            .map_err(|err| self.err(err))
    }

    /// 估算剩余时间用的剩余作业数：处理中的，加上 `claim_job(images, audio, video)` 能领取的 `pending` 作业。
    /// 条件与 `claim_job` 一致：所在根目录在线、文件不在云端、`images` / `audio` / `video` 为否时不含该类；
    /// 改动 `claim_job` 的筛选条件时同步修改这里
    pub fn claimable_jobs(&self, images: bool, audio: bool, video: bool) -> Result<u64, DbError> {
        self.conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM jobs j JOIN files f ON f.id = j.file_id JOIN roots r ON r.id = f.root_id
                     WHERE j.stage IN {ACTIVE}
                        OR (j.stage = 'pending' AND r.offline = 0 AND f.cloud = 0
                            AND (?1 OR f.kind <> 'image') AND (?2 OR f.kind <> 'audio')
                            AND (?3 OR f.kind <> 'video'))"
                ),
                [images, audio, video],
                |row| row.get::<_, i64>(0),
            )
            .map(|n| n as u64)
            .map_err(|err| self.err(err))
    }

    /// 这几类文件中正在处理（`preprocessing` / `embedding`）的作业
    pub fn active_jobs(&self, kinds: &[&str]) -> Result<Vec<i64>, DbError> {
        let marks = vec!["?"; kinds.len()].join(", ");
        self.conn
            .prepare(&format!(
                "SELECT j.file_id FROM jobs j JOIN files f ON f.id = j.file_id
                 WHERE j.stage IN {ACTIVE} AND f.kind IN ({marks})"
            ))
            .and_then(|mut stmt| {
                stmt.query_map(rusqlite::params_from_iter(kinds), |row| row.get(0))?
                    .collect()
            })
            .map_err(|err| self.err(err))
    }

    /// 按种类统计作业；等待重建的算作已完成（旧向量照常可搜），重建进度另见 `rebuild_counts`
    pub fn job_counts(&self) -> Result<BTreeMap<String, JobCounts>, DbError> {
        let rows: Vec<(String, String, i64)> = self
            .conn
            .prepare(&format!(
                "SELECT f.kind, CASE WHEN {FAILED} THEN 'failed' WHEN j.priority < ?1 THEN 'stored' ELSE j.stage END AS st,
                   COUNT(*)
                 FROM jobs j JOIN files f ON f.id = j.file_id
                 GROUP BY f.kind, st"
            ))
            .and_then(|mut stmt| {
                stmt.query_map([REBUILD_BELOW], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?
                .collect()
            })
            .map_err(|err| self.err(err))?;
        let mut out: BTreeMap<String, JobCounts> = BTreeMap::new();
        for (kind, stage, n) in rows {
            let n = n as u64;
            let c = out.entry(kind).or_default();
            c.total += n;
            match stage.as_str() {
                "stored" => c.done += n,
                "failed" => c.failed += n,
                _ => c.pending += n,
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests;
