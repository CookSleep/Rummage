//! 音频的预处理：单独的音频文件（`audio_file`），以及与文档的内嵌音视频共用的解码与切段
//! （`audio_items`）和结果类型（`Decoded`、`MediaFail`）。
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use rummage_embed::{Input, space};
use rummage_index::{Claimed, Outcome, Space, content_hash, ensure_local};
use rummage_media::audio;
use rummage_media::ffmpeg::MediaError;
use rummage_proto::ParamValue;

use super::{Clock, MediaLease, Prep, failed, file_limit, time_locator};
use crate::pipeline::{Item, Prepared, Shared, Task, int, lock, media_space_row};

pub(super) fn audio_file(shared: &Shared, task: Task, qh: Vec<u8>, clock: &Clock) -> Prep {
    let (job, engine) = (&task.job, &task.engine);
    let ch = match content_hash(&job.path) {
        Ok(h) => h.to_vec(),
        Err(err) => return failed(job, Some(&qh), err),
    };
    let space = media_space_row(&engine.cfg, &audio_version(&task.params));
    match lock(&shared.db).media_copy_source(&ch, &space.id, job.file_id) {
        Ok(None) => {}
        Ok(Some(from)) => {
            return Prep::Done(Outcome::Copied {
                kind: "audio",
                quick_hash: qh,
                content_hash: ch,
                space_id: space.id,
                slice_version: String::new(),
                from,
            });
        }
        Err(err) => return failed(job, Some(&qh), err),
    }
    // 交给 ffmpeg 读取之前再确认一次不是占位文件
    if let Err(err) = ensure_local(&job.path) {
        return failed(job, Some(&qh), err);
    }
    match audio_items(shared, &task, &job.path, clock, true, &time_locator) {
        Ok(d) => Prep::Ready(Box::new(Prepared {
            job: task.job.clone(),
            engine: task.engine.clone(),
            generation: task.generation,
            kind: "audio",
            space: d.space,
            fallback: None,
            quick_hash: qh,
            content_hash: Some(ch),
            items: d.items,
            size: None,
            meta_json: Some(d.meta_json),
            _mem: Some(d.lease),
            media: Vec::new(),
            replace: None,
            more: false,
            media_failed: None,
        })),
        Err(fail) => fail.into_prep(job, qh),
    }
}

/// 音频的参数版本
fn audio_version(params: &BTreeMap<String, ParamValue>) -> String {
    let seg_s = int(params, "audio_segment_s").max(1) as u32;
    let overlap_s = audio::effective_overlap(seg_s, int(params, "audio_overlap_s").max(0) as u32);
    space::audio_preprocess(seg_s, overlap_s, int(params, "audio_silence_db"))
}

/// 解码好、等待推理的音频或视频（单独的文件与文档的内嵌音视频共用，`audio_items`、`video::video_items`）
pub(in crate::pipeline) struct Decoded {
    pub items: Vec<Item>,
    pub space: Space,
    /// 带音频的视频：失败时整段改用的纯画面空间
    pub fallback: Option<Space>,
    pub meta_json: String,
    /// 视频转正后的宽高
    pub size: Option<(u32, u32)>,
    pub lease: MediaLease,
}

/// 音视频解码的失败
pub(in crate::pipeline) enum MediaFail {
    /// 超过时长上限，带上元数据（`Outcome::TooLong`）
    TooLong(String),
    Failed(MediaError),
    /// 画面尺寸无法处理
    Size(rummage_media::Error),
    /// 不等待额度时额度不够（文档的内嵌音视频，`MediaMem::lease`）
    NoMemory,
    /// 正在停止
    Released,
}

impl MediaFail {
    pub(super) fn into_prep(self, job: &Claimed, qh: Vec<u8>) -> Prep {
        match self {
            MediaFail::TooLong(meta_json) => Prep::Done(Outcome::TooLong {
                quick_hash: qh,
                meta_json,
            }),
            MediaFail::Failed(err) => failed(job, Some(&qh), err),
            MediaFail::Size(err) => failed(job, Some(&qh), err),
            // 单独的文件总是等待额度，不会出现 `NoMemory`
            MediaFail::NoMemory | MediaFail::Released => Prep::Done(Outcome::Released),
        }
    }
}

/// ffprobe 读元数据，超过时长上限 `max_audio_min` 的不解码；然后 ffmpeg 解码为 16 kHz 单声道，
/// 按 `audio_segment_s` / `audio_overlap_s` 切段、跳过低于 `audio_silence_db` 的静音段，每段一个片段，locator 由 `loc` 按毫秒起止给出。
/// `path` 可以是 `rummage_media::ffmpeg::subfile` 给出的地址；调用方已做过占位文件检查。
/// `wait` 为否时不等待内存额度，额度不够就返回 `NoMemory`（`MediaMem::lease`）
pub(in crate::pipeline) fn audio_items(
    shared: &Shared,
    task: &Task,
    path: &Path,
    clock: &Clock,
    wait: bool,
    loc: &dyn Fn(u64, u64) -> String,
) -> Result<Decoded, MediaFail> {
    let params = &task.params;
    let seg_s = int(params, "audio_segment_s").max(1) as u32;
    let overlap_s = audio::effective_overlap(seg_s, int(params, "audio_overlap_s").max(0) as u32);
    let silence_db = int(params, "audio_silence_db");
    let version = audio_version(params);
    let space = media_space_row(&task.engine.cfg, &version);
    let ffmpeg = &shared.opts.ffmpeg;
    let limit = file_limit(shared, task);
    let mut info = clock
        .backend(|| audio::probe(ffmpeg, path, limit))
        .map_err(MediaFail::Failed)?;
    let max_ms = (int(params, "max_audio_min").max(1) as u64).saturating_mul(60_000);
    let per_ms = u64::from(audio::SAMPLE_RATE) / 1000;
    let too_long = |mut info: audio::AudioInfo| {
        // 解码出的长度超过上限时，元数据给的时长不可信，按超过上限记，扫描才不会再排队（rummage_index::scan）
        info.duration_ms = Some(info.duration_ms.unwrap_or(0).max(max_ms + 1));
        MediaFail::TooLong(info.meta_json())
    };
    if info.duration_ms.is_some_and(|d| d > max_ms) {
        return Err(too_long(info));
    }
    // 时长未知时按上限占用额度
    let expect = info.duration_ms.unwrap_or(max_ms).saturating_mul(per_ms);
    let lease = clock.backend(|| shared.media_mem.lease(expect * 4, wait, &shared.stop))?;
    // ffmpeg 解码通常是实时的几百倍：在单文件时限之外，每 10 秒音频多给 1 秒
    let timeout = limit + Duration::from_millis(info.duration_ms.unwrap_or(max_ms) / 10);
    let decoded = clock.backend(|| {
        audio::decode(
            ffmpeg,
            path,
            // 元数据中的时长常有小误差，预先分配时多留 1 秒，免得最后再扩容一次
            expect as usize + audio::SAMPLE_RATE as usize,
            max_ms.saturating_mul(per_ms) as usize,
            timeout,
        )
    });
    let samples = match decoded {
        Ok(s) => s,
        Err(MediaError::TooLong) => return Err(too_long(info)),
        Err(err) => return Err(MediaFail::Failed(err)),
    };
    if info.duration_ms.is_none() {
        info.duration_ms = Some(samples.len() as u64 / per_ms);
    }
    let items = audio::segments(&samples, seg_s, overlap_s, silence_db as f64)
        .into_iter()
        .map(|s| Item {
            locator: loc(s.start_ms(), s.end_ms()),
            text: None,
            input: Input::Audio {
                samples: samples[s.start..s.end].to_vec(),
                preprocess: version.clone(),
            },
            space: None,
        })
        .collect();
    Ok(Decoded {
        items,
        space,
        fallback: None,
        meta_json: info.meta_json(),
        size: None,
        lease,
    })
}
