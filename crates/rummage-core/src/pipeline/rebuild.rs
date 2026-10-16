//! 设置改了以后只重建受影响的文件：
//! - 由参数表算出各种类应有的参数版本（`wanted`），交给 `Db::requeue_stale` 把版本不符的已完成作业放回、标为重建作业。
//!   启动流水线时与核心修改影响索引的参数、开关文件类型后各做一次（`Pipeline::requeue`）。领取时的档位核对（`claim`）照旧。
//! - 重建与建索引同一套调度：排在新增与内容变化的文件之后（`rummage_index::jobs`），受同样的暂停条件约束。
//! - 进度：`total` 为这一批的文件数，新的修改加入的文件计入同一批，剩余为 0 时清零（`Shared::rebuild_status`）。
use std::collections::BTreeMap;

use rummage_embed::space;
use rummage_extract::ChunkOpts;
use rummage_index::{Db, DbError, Wanted};
use rummage_media::audio;
use rummage_proto::{ParamValue, RebuildStatus, params};

use super::{Pipeline, Shared, docmedia, float, int, lock, video_opts};

/// 按参数表各种类的向量应有的参数版本；`kinds` 为空，由调用方按开启的种类填写
pub(super) fn wanted(values: &BTreeMap<String, ParamValue>) -> Wanted {
    let tier = int(values, "image_tokens") as u32;
    let long_ratio = float(values, "long_image_ratio");
    let chunk = ChunkOpts::from_params(values);
    let seg_s = int(values, "audio_segment_s").max(1) as u32;
    let overlap_s = audio::effective_overlap(seg_s, int(values, "audio_overlap_s").max(0) as u32);
    let w = video_opts(values);
    let win = w.windows;
    Wanted {
        kinds: Vec::new(),
        text: space::text_version(chunk.target, chunk.overlap_pct, chunk.min),
        image: space::image_preprocess(tier),
        slice: space::image_slice_preprocess(
            tier,
            long_ratio,
            int(values, "long_image_max_slices").max(0) as u32,
        ),
        long_ratio,
        audio: space::audio_preprocess(seg_s, overlap_s, int(values, "audio_silence_db")),
        video: space::video_preprocess(win.window_s, win.overlap_s, win.frames, false),
        video_av: w
            .audio_tokens
            .map(|m| space::video_av_preprocess(win.window_s, win.overlap_s, win.frames, m)),
        // 由调用方按开启的模态填写（`docmedia::tokens`）
        doc_media: "[]".to_string(),
    }
}

impl Shared {
    /// 处理哪些媒体种类（图片、音频、视频）：mmproj 可用且对应的参数开启，与 `engine` 的判定相同
    fn media_on(&self, values: &BTreeMap<String, ParamValue>) -> (bool, bool, bool) {
        let mmproj = (self.opts.available)(&self.opts.mmproj, &space::MMPROJ);
        let on = |k: &str| mmproj && params::flag(values, k);
        (on("index_images"), on("index_audio"), on("index_video"))
    }

    /// 把需要重建的已完成作业放回（只看会建内容索引的种类：关闭的类型、缺 mmproj 时的媒体放回后不会被领取），
    /// 计入这一批的总数。调用方持有数据库锁
    pub(super) fn requeue_stale(&self, db: &Db) -> Result<BTreeMap<String, usize>, DbError> {
        let values = lock(&self.params).clone();
        let (images, audio, video) = self.media_on(&values);
        let mut w = wanted(&values);
        if params::flag(&values, "index_documents") {
            w.kinds.push("document");
        }
        w.doc_media = serde_json::to_string(&docmedia::tokens(&values, images, audio, video))
            .unwrap_or_else(|_| "[]".to_string());
        for (on, kind) in [(images, "image"), (audio, "audio"), (video, "video")] {
            if on {
                w.kinds.push(kind);
            }
        }
        let before: u64 = db.rebuild_counts(images, audio, video)?.values().sum();
        let n = db.requeue_stale(&w)?;
        let mut total = lock(&self.rebuild_total);
        *total = (*total).max(before) + n.values().sum::<usize>() as u64;
        Ok(n)
    }

    /// 重建进度；没有要重建的文件时为空，同时结束这一批（清零总数）
    pub(super) fn rebuild_status(&self) -> Result<Option<RebuildStatus>, DbError> {
        let (images, audio, video) = self.media_on(&lock(&self.params));
        let by_kind = lock(&self.db).rebuild_counts(images, audio, video)?;
        let left: u64 = by_kind.values().sum();
        let mut total = lock(&self.rebuild_total);
        if left == 0 {
            *total = 0;
            return Ok(None);
        }
        *total = (*total).max(left);
        Ok(Some(RebuildStatus {
            done: *total - left,
            total: *total,
            by_kind,
        }))
    }
}

impl Pipeline {
    /// 影响索引的参数修改、开关文件类型后调用：已完成、但不符合新设置的文件放回重建（旧向量保留到新结果写入）；
    /// `active` 种类中正在处理的作业按旧参数计算，记入 `stale`，结果写入时放回 `pending`。都在数据库锁内，与写库线程的
    /// 提交互斥：提交在前的已是 `stored`，会按版本判断；在后的会在提交时被改为放回。返回各种类放回的已完成作业数
    pub fn requeue(&self, active: &[&str]) -> Result<BTreeMap<String, usize>, DbError> {
        // 图片、音频、视频的设置也决定文档的内嵌媒体：正在处理的文档同样作废、按新设置重算
        let mut active = active.to_vec();
        if active
            .iter()
            .any(|k| ["image", "audio", "video"].contains(k))
            && !active.contains(&"document")
        {
            active.push("document");
        }
        let db = lock(&self.shared.db);
        let ids = db.active_jobs(&active)?;
        let n = self.shared.requeue_stale(&db)?;
        let mut stale = lock(&self.shared.stale);
        for id in ids {
            if !stale.contains(&id) {
                stale.push(id);
            }
        }
        drop(stale);
        drop(db);
        self.shared.wake();
        self.shared.broadcast();
        Ok(n)
    }
}
