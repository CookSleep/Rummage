//! 设置改了以后只重建受影响的文件：
//! - 由参数表算出各种类应有的参数版本（`wanted`），交给 `Db::requeue_stale` 把版本不符的已完成作业放回、标为重建作业。
//!   启动流水线时与核心修改影响索引的参数、开关文件类型后各做一次（`Pipeline::requeue`）。
//!   图片的整图与切片、视频帧的档位各自写在自己的版本中：只改 `long_image_tokens` 时只放回长图，只改 `image_tokens`
//!   时放回全部图片，只改 `video_frame_tokens` 时只放回视频；后两者也决定文档内嵌媒体的标记（`docmedia::tokens`），
//!   可能内嵌媒体的文档只重算内嵌媒体。
//! - 重建与建索引同一套调度：排在新增与内容变化的文件之后（`rummage_index::jobs`），受同样的暂停条件约束。
//! - 进度：`total` 为这一批的文件数，新的修改加入的文件计入同一批，剩余为 0 时清零（`Shared::rebuild_status`）。
//! - 换了主模型文件（手机上在 NPU 模型与 LiteRT 通用版之间切换）的文件整个重建：启动时要用的模型按要选用的设备
//!   （与建引擎时的选择相同）；之后每次新建的引擎与上一个的模型不同时（换设备、检测完成、设备出错后改用别的）按新引擎的
//!   模型再检查一次，正在处理的作业全部作废（`Shared::requeue`）。
use std::collections::BTreeMap;

use rummage_embed::space;
use rummage_embed::{Backend, SpaceConfig};
use rummage_extract::ChunkOpts;
use rummage_index::{Db, DbError, Wanted};
use rummage_media::audio;
use rummage_proto::{ParamValue, RebuildStatus, params};

use super::{Pipeline, Shared, docmedia, float, int, lock, video_opts};

/// 改了参数 `key` 后，正在处理的作业中按旧参数计算、要作废的种类：参数影响的种类；文档内嵌媒体的标记
/// （`docmedia::tokens`）用整图档位与音频、视频的设置，改这些时还有文档。长图切片的设置不影响文档
pub(crate) fn void_kinds(key: &str) -> Vec<&'static str> {
    let mut kinds = params::def(key).map_or(&[][..], |d| d.index_kinds).to_vec();
    if key == "image_tokens" || kinds.iter().any(|k| ["audio", "video"].contains(k)) {
        kinds.push("document");
    }
    kinds
}

/// 按参数表各种类的向量应有的参数版本；`kinds` 为空，由调用方按开启的种类填写
/// `cfg` 为当前运行库的空间配置，决定图像预处理版本
pub(super) fn wanted(values: &BTreeMap<String, ParamValue>, cfg: &SpaceConfig) -> Wanted {
    let long_ratio = float(values, "long_image_ratio");
    let chunk = ChunkOpts::from_params(values);
    let seg_s = int(values, "audio_segment_s").max(1) as u32;
    let overlap_s = audio::effective_overlap(seg_s, int(values, "audio_overlap_s").max(0) as u32);
    let w = video_opts(values);
    let win = w.windows;
    Wanted {
        kinds: Vec::new(),
        text: space::text_version(chunk.target, chunk.overlap_pct, chunk.min),
        image: cfg.image_version(int(values, "image_tokens") as u32),
        slice: cfg.slice_version(
            int(values, "long_image_tokens") as u32,
            long_ratio,
            int(values, "long_image_max_slices").max(0) as u32,
        ),
        long_ratio,
        audio: space::audio_preprocess(seg_s, overlap_s, int(values, "audio_silence_db")),
        video: space::video_preprocess(
            win.window_s,
            win.overlap_s,
            win.frames,
            w.frame_tokens,
            false,
        ),
        video_av: w.audio_tokens.map(|m| {
            space::video_av_preprocess(win.window_s, win.overlap_s, win.frames, w.frame_tokens, m)
        }),
        // 由调用方按开启的模态填写（`docmedia::tokens`）
        doc_media: "[]".to_string(),
        model: Some(cfg.model_sha256.clone()),
    }
}

impl Shared {
    /// 后端 `backend` 用的多模态模型可用；LiteRT 时本机芯片的 NPU 模型可用也算（只下了 NPU 版的手机）
    pub(super) fn mmproj_available(&self, backend: Backend) -> bool {
        let opts = &self.opts;
        let ok = |b: Backend| (opts.available)(opts.model_paths(b).1, opts.model_files(b).1);
        ok(backend) || (opts.litert.is_some() && ok(Backend::Npu))
    }

    /// 处理哪些媒体种类（图片、音频、视频）：mmproj 可用且对应的参数开启，与 `engine` 的判定相同
    fn media_on(&self, values: &BTreeMap<String, ParamValue>) -> (bool, bool, bool) {
        let mmproj = self.mmproj_available(Backend::Cpu);
        let on = |k: &str| mmproj && params::flag(values, k);
        (on("index_images"), on("index_audio"), on("index_video"))
    }

    /// 要用的主模型所属的后端（LiteRT 的 NPU 用另一个模型文件）：按要选用的设备，与建引擎时的选择相同，但不算后台自动检测期间
    /// 暂时只用 CPU（检测完就切回）。会读自检记录，在取数据库锁之前调用，结果交给 `requeue_stale`
    pub(super) fn model_backend(&self) -> Backend {
        let values = lock(&self.params).clone();
        self.chosen_gpu(&values, false)
            .map_or(Backend::Cpu, |r| r.backend)
    }

    /// 把需要重建的已完成作业放回（只看会建内容索引的种类：关闭的类型、缺 mmproj 时的媒体放回后不会被领取），
    /// 计入这一批的总数；向量所用的模型与后端 `backend`（`model_backend`）的不同时整个文件重建。调用方持有数据库锁
    pub(super) fn requeue_stale(
        &self,
        db: &Db,
        backend: Backend,
    ) -> Result<BTreeMap<String, usize>, DbError> {
        let values = lock(&self.params).clone();
        let (images, audio, video) = self.media_on(&values);
        let cfg = self.opts.space(backend);
        let mut w = wanted(&values, &cfg);
        if params::flag(&values, "index_documents") {
            w.kinds.push("document");
        }
        w.doc_media = serde_json::to_string(&docmedia::tokens(&values, &cfg, images, audio, video))
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
    /// 提交互斥：提交在前的已是 `stored`，会按版本判断；在后的会在提交时被改为放回。返回各种类放回的已完成作业数。
    /// 改参数时 `active` 由 `void_kinds` 给出
    pub fn requeue(&self, active: &[&str]) -> Result<BTreeMap<String, usize>, DbError> {
        self.shared.requeue(active, self.shared.model_backend())
    }
}

impl Shared {
    /// `Pipeline::requeue` 的实现：要用的模型为后端 `backend` 的
    pub(super) fn requeue(
        &self,
        active: &[&str],
        backend: Backend,
    ) -> Result<BTreeMap<String, usize>, DbError> {
        let db = lock(&self.db);
        let ids = db.active_jobs(active)?;
        let n = self.requeue_stale(&db, backend)?;
        let mut stale = lock(&self.stale);
        for id in ids {
            if !stale.contains(&id) {
                stale.push(id);
            }
        }
        drop(stale);
        drop(db);
        self.wake();
        self.broadcast();
        Ok(n)
    }
}
