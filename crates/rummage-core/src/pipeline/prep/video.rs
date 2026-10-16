//! 视频的预处理：内容哈希，库中已有同空间向量的直接复制（带音频的设置先找带音频的，
//! 再找纯画面的；改为纯画面的视频向量记带音频的版本，不作为来源）；否则再做一次占位文件检查，ffprobe 读元数据，超过时长上限 `max_audio_min`（与音频共用）的只记元数据。
//! 然后先占用内存额度（按时长估算的帧像素加音频采样，`MediaMem`），整段视频只运行一次 ffmpeg 抽帧
//! （`rummage_media::video::extract`，参数 `video_decoder` 为硬件解码且没有旋转标记时先试硬件解码）；带音频的设置且文件有音轨时另运行一次 ffmpeg 解码音轨
//! （复用音频的解码，与抽帧分开：两者的输出格式与读法不同，分开最简单，多读一遍文件的代价远小于推理）。
//! 音轨读不出时整段视频按纯画面处理，向量的参数版本仍记带音频的版本（`rebuild`）。每个窗口一个片段（`rummage_media::video::assemble`），locator 记毫秒起止。
//! ffprobe、ffmpeg 的等待不计入单文件时限；ffmpeg 自己的时限按时长放宽（最慢按 2 倍实时算），即单文件时限按时长折算。
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rummage_embed::{Input, space};
use rummage_index::{Outcome, content_hash, ensure_local};
use rummage_media::audio::{self, SAMPLE_RATE};
use rummage_media::ffmpeg::MediaError;
use rummage_media::video::{self, VideoInfo};

use rummage_index::Space;

use super::{Clock, Decoded, MediaFail, Prep, failed, file_limit, time_locator};
use crate::pipeline::{Item, Prepared, Shared, Task, int, lock, media_space_row, video_opts};

pub(super) fn video_file(shared: &Shared, task: Task, qh: Vec<u8>, clock: &Clock) -> Prep {
    let job = &task.job;
    let ch = match content_hash(&job.path) {
        Ok(h) => h.to_vec(),
        Err(err) => return failed(job, Some(&qh), err),
    };
    let (av_space, v_space) = spaces(&task);
    for sp in av_space.iter().chain([&v_space]) {
        match lock(&shared.db).media_copy_source(&ch, &sp.id, job.file_id) {
            Ok(None) => {}
            Ok(Some(from)) => {
                return Prep::Done(Outcome::Copied {
                    kind: "video",
                    quick_hash: qh,
                    content_hash: ch,
                    space_id: sp.id.clone(),
                    slice_version: String::new(),
                    from,
                });
            }
            Err(err) => return failed(job, Some(&qh), err),
        }
    }
    // 交给 ffmpeg 读取之前再确认一次不是占位文件
    if let Err(err) = ensure_local(&job.path) {
        return failed(job, Some(&qh), err);
    }
    match video_items(shared, &task, &job.path, clock, true, &time_locator) {
        Ok(d) => Prep::Ready(Box::new(Prepared {
            job: task.job.clone(),
            engine: task.engine.clone(),
            generation: task.generation,
            kind: "video",
            space: d.space,
            fallback: d.fallback,
            quick_hash: qh,
            content_hash: Some(ch),
            items: d.items,
            size: d.size,
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

/// 带音频与纯画面两个空间；设置为只看画面时前者为空
fn spaces(task: &Task) -> (Option<Space>, Space) {
    let opts = video_opts(&task.params);
    let w = opts.windows;
    let cfg = &task.engine.cfg;
    let v_space = media_space_row(
        cfg,
        &space::video_preprocess(w.window_s, w.overlap_s, w.frames, false),
    );
    let av_space = opts.audio_tokens.map(|m| {
        media_space_row(
            cfg,
            &space::video_av_preprocess(w.window_s, w.overlap_s, w.frames, m),
        )
    });
    (av_space, v_space)
}

/// ffprobe 读元数据，超过时长上限的不解码；整段只运行一次 ffmpeg 抽帧，带音频的设置且有音轨时另解码音轨，
/// 每个窗口一个片段，locator 由 `loc` 按毫秒起止给出（单独的视频与文档的内嵌视频共用）。
/// `path` 可以是 `rummage_media::ffmpeg::subfile` 给出的地址；调用方已做过占位文件检查。`wait` 见 `audio::audio_items`
pub(in crate::pipeline) fn video_items(
    shared: &Shared,
    task: &Task,
    path: &Path,
    clock: &Clock,
    wait: bool,
    loc: &dyn Fn(u64, u64) -> String,
) -> Result<Decoded, MediaFail> {
    let opts = video_opts(&task.params);
    let w = opts.windows;
    let (av_space, v_space) = spaces(task);
    let ffmpeg = &shared.opts.ffmpeg;
    let limit = file_limit(shared, task);
    let mut info = clock
        .backend(|| video::probe(ffmpeg, path, limit))
        .map_err(MediaFail::Failed)?;
    let max_ms = (int(&task.params, "max_audio_min").max(1) as u64).saturating_mul(60_000);
    let too_long = |mut info: VideoInfo| {
        // 抽出的帧超过上限时，元数据给的时长不可信，按超过上限记，扫描才不会再排队（rummage_index::scan）
        info.duration_ms = Some(info.duration_ms.unwrap_or(0).max(max_ms + 1));
        MediaFail::TooLong(info.meta_json())
    };
    if info.duration_ms.is_some_and(|d| d > max_ms) {
        return Err(too_long(info));
    }
    let size = video::frame_size(info.width, info.height).map_err(MediaFail::Size)?;
    let grid = w.grid();
    // 时长未知时按上限估算额度与时限
    let dur = info.duration_ms.unwrap_or(max_ms);
    let per_ms = u64::from(SAMPLE_RATE) / 1000;
    let with_audio = av_space.is_some() && info.has_audio;
    let frame_bytes = u64::from(size.0) * u64::from(size.1) * 3;
    // 整段采样与各窗口的音频段副本各一份
    let audio_bytes = if with_audio { dur * per_ms * 4 * 2 } else { 0 };
    let bytes = grid.frames_in(dur) as u64 * frame_bytes + audio_bytes;
    let lease = clock.backend(|| shared.media_mem.lease(bytes, wait, &shared.stop))?;
    let timeout = limit + Duration::from_millis(dur / 2);
    let extracted = clock.backend(|| {
        video::extract(
            ffmpeg,
            path,
            size,
            grid,
            grid.frames_in(max_ms),
            int(&task.params, "video_decoder") == 1 && !info.rotated,
            None,
            timeout,
        )
    });
    let frames: Vec<Arc<rummage_media::Rgb>> = match extracted {
        Ok(f) => f.into_iter().map(Arc::new).collect(),
        Err(MediaError::TooLong) => return Err(too_long(info)),
        Err(err) => return Err(MediaFail::Failed(err)),
    };
    if info.duration_ms.is_none() {
        info.duration_ms = Some(grid.ms(frames.len()));
    }
    let duration_ms = info.duration_ms.unwrap_or(0);
    let samples = if with_audio {
        let decoded = clock.backend(|| {
            audio::decode(
                ffmpeg,
                path,
                (duration_ms * per_ms) as usize + SAMPLE_RATE as usize,
                max_ms.saturating_mul(per_ms) as usize,
                timeout,
            )
        });
        match decoded {
            Ok(s) => Some(s),
            Err(MediaError::TooLong) => return Err(too_long(info)),
            Err(err) => {
                log::warn!(
                    "{} 的音轨无法读取，只按画面建索引：{err}",
                    task.job.path.display()
                );
                None
            }
        }
    } else {
        None
    };
    let audio = samples.is_some();
    let preprocess = match &av_space {
        Some(av) if audio => &av.preprocess,
        _ => &v_space.preprocess,
    };
    // 音轨读不出时向量在纯画面空间，参数版本记带音频的版本：按当前设置已处理过，不必重建（`rebuild`）
    let own = av_space
        .as_ref()
        .filter(|_| with_audio && !audio)
        .map(|av| Box::new((v_space.clone(), av.preprocess.clone())));
    let items = video::windows(duration_ms, w)
        .iter()
        .map(|win| Item {
            locator: loc(win.start_ms, win.end_ms),
            text: None,
            input: Input::Video {
                parts: video::assemble(win, &frames, samples.as_deref()),
                preprocess: preprocess.clone(),
                max_tokens: opts.audio_tokens.filter(|_| audio).unwrap_or(usize::MAX),
            },
            space: own.clone(),
        })
        .collect();
    let (space, fallback) = match av_space.filter(|_| audio) {
        Some(av) => (av, Some(v_space)),
        None => (v_space, None),
    };
    Ok(Decoded {
        items,
        space,
        fallback,
        meta_json: info.meta_json(),
        size: Some((info.width, info.height)),
        lease,
    })
}
