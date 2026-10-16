//! 文档的内嵌媒体：在文档的预处理中列出并逐个取出内嵌的图片、音频、视频，
//! 解码后交给推理线程；每个媒体是一个 `Media`，推理失败只跳过它自己（`embed`）。
//! - 是否抽取跟随图片、音频、视频的开关与图片与音视频模型（`Engine::images` / `audio` / `video`）。所用的设置记成标记
//!   （`tokens`，写在文档 `meta_json` 的 `media` 中），与当前设置不符时由 `rebuild` 只放回内嵌媒体（`jobs.parts`）。
//!   只算内嵌媒体时只替换开启的模态的片段（`modalities`），关闭的模态已有的片段保留，与单独的图片、音视频相同。
//! - 时限：文档整体不设时限，处理内嵌媒体的时间不计入正文的时限（`Clock::backend`）。列出媒体、每个媒体各用一次单文件时限，
//!   在单独的任务线程中执行（`prep::run_limited`）：图片按图片文件，音视频与单独的文件一样按时长放宽 ffmpeg 的时限。
//!   某个媒体损坏、解码出错或超时时跳过它并写日志，不影响正文与其他媒体；它计入作业的 `media_failed`，文档因此出现在
//!   “失败文件”列表中，正文照常可搜。超时后立即结束这一批，下一批从它后面一个接着算。超过时长或大小上限的
//!   与单独的文件一样只写日志、不算失败。列出媒体时超时、出错或无法解析，记 1 个失败，已有的内嵌片段保留，重试时从头算。
//! - 读取：文档的字节由 `read_bytes` 读出（含占位文件检查）；音视频交给 ffmpeg 之前再检查一次。不压缩的 zip 条目
//!   用 `subfile` 直接读文档，压缩的写入数据目录的 `tmp\embed\`（`Options::embed_tmp`），解码完即删除，后台服务启动时清空该目录。
//! - 图片只算整图、不切长图，档位与单独的图片相同（`image_tokens`）；近乎纯色的跳过（`rummage_media::is_flat`）。
//! - 分批：图片解码后的像素与音视频一样计入内存额度（`MediaMem`）。额度要等整份文档推理完才归还，所以这份文档已持有额度时
//!   不再等待：下一个媒体会超额，或超时后仍在运行的任务线程达到 `index_threads` 时，停止收集，只把已解码的这一批交去识别，
//!   在 `meta_json` 中记下进度（`media_partial` 为所用设置的各项，`media_next` 为下一个序号），作业放回、只算内嵌媒体，
//!   从断点继续（`Outcome::Stored::more`），全部完成后才写入标记 `media`。每一批的第一个媒体照常等待额度（没有别的占用时
//!   总能拿到），也不看线程数，所以每批至少处理一个，不会空转。有别的文件在等额度时，已持有额度的文档不再申请，
//!   先把这一批交出去（`MediaMem::lease`）。设置在中途改了时（`media_partial` 与当前不同）从头重算。
//! - 只算内嵌媒体时文档读不出（`read_error`），与单独的文件读不出相同处理（`prep::text`），已有的内嵌片段与失败个数不动。
//! - 媒体本身损坏、解码出错、单个超时或识别出错的跳过并写日志，算作已处理；个数记在作业的 `media_failed` 中，
//!   这份文档因此出现在“失败文件”列表中（正文与其他媒体照常可搜），重试时只重算内嵌媒体。
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rummage_embed::Input;
use rummage_extract::media::{Embedded, Loaded, MEDIA_EXTS, MediaKind, MediaOpts, Source};
use rummage_extract::read_bytes;
use rummage_index::{Space, ensure_local};
use rummage_proto::{ParamValue, params};

use super::prep::{
    Clock, Decoded, Limit, MediaFail, MediaLease, audio_items, file_limit, run_limited, video_items,
};
use super::{Engine, Item, Shared, Task, int, lock, rebuild};

/// 一个内嵌媒体的片段，推理时作为一组（`embed`）
pub(super) struct Media {
    /// `image`、`audio`、`video_av` 或 `video_v`
    pub modality: &'static str,
    pub space: Space,
    /// 带音频的视频：失败时这一个视频改用的纯画面空间
    pub fallback: Option<Space>,
    /// 都带自己的空间（`Item::space`）
    pub items: Vec<Item>,
    /// 音频采样、视频帧占用的内存额度
    pub _mem: Option<MediaLease>,
}

/// 内嵌媒体标记应含的各项：过滤参数的版本与开启的各模态的参数版本（`rummage_index::Wanted::doc_media`）
pub(super) fn tokens(
    values: &BTreeMap<String, ParamValue>,
    images: bool,
    audio: bool,
    video: bool,
) -> Vec<String> {
    let w = rebuild::wanted(values);
    let mut out = vec![format!(
        "docmedia-v1-p{}-n{}-r{}-mp{}-mb{}",
        int(values, "embed_min_px"),
        int(values, "embed_max_per_doc"),
        int(values, "embed_repeat_pages"),
        int(values, "max_image_mp"),
        int(values, "max_doc_mb")
    )];
    if images {
        out.push(w.image);
    }
    if audio {
        out.push(w.audio);
    }
    if video {
        out.push(w.video_av.unwrap_or(w.video));
    }
    out
}

/// 只算内嵌媒体时要替换的模态：开启的模态
pub(super) fn modalities(engine: &Engine) -> Vec<&'static str> {
    let mut out = Vec::new();
    if engine.images {
        out.push("image");
    }
    if engine.audio {
        out.push("audio");
    }
    if engine.video {
        out.extend(["video_av", "video_v"]);
    }
    out
}

/// 这一批的结果
pub(super) struct Collected {
    pub media: Vec<Media>,
    /// 要写入 `meta_json` 的标记或进度；文档读不出时为空，之后按标记不符再算
    pub meta_json: Option<String>,
    /// 接着上一批继续：失败个数累加到前几批上
    pub resume: bool,
    /// 已有的内嵌片段保留、只追加（接着上一批，或列出媒体失败时）
    pub keep: bool,
    /// 文档读不出（只算内嵌媒体时由 `prep::text` 按单独文件读不出的规则处理）
    pub read_error: Option<rummage_extract::ReadError>,
    /// 还没处理完，写入后放回再算下一批
    pub more: bool,
    /// 这一批中无法识别（损坏、解码出错、超时）而跳过的个数
    pub failed: u32,
}

/// 列出并解码文档的内嵌媒体（从上一批的断点继续），见模块说明
pub(super) fn collect(shared: &Arc<Shared>, task: &Task, clock: &Clock) -> Collected {
    let (job, engine) = (&task.job, &task.engine);
    let tokens = tokens(&task.params, engine.images, engine.audio, engine.video);
    let done = |media: Vec<Media>, resume: bool| Collected {
        media,
        meta_json: Some(serde_json::json!({ "media": tokens }).to_string()),
        resume,
        keep: resume,
        more: false,
        failed: 0,
        read_error: None,
    };
    if !(engine.images || engine.audio || engine.video) || !MEDIA_EXTS.contains(&job.ext.as_str()) {
        return done(Vec::new(), false);
    }
    // 上一批记下的断点；设置改了就从头算
    let progress = match lock(&shared.db).media_progress(job.file_id) {
        Ok(p) => p.filter(|(t, _)| *t == tokens).map(|(_, next)| next),
        Err(err) => {
            log::warn!("无法读取 {} 的内嵌媒体进度：{err}", job.path.display());
            None
        }
    };
    let resume = progress.is_some();
    let start = progress.unwrap_or(1);
    let max_doc = params::max_doc_bytes(&task.params);
    let opts = MediaOpts {
        images: engine.images,
        audio: engine.audio,
        video: engine.video,
        min_px: int(&task.params, "embed_min_px").max(1) as u32,
        max_count: int(&task.params, "embed_max_per_doc").max(0) as usize,
        repeat_pages: int(&task.params, "embed_repeat_pages").max(1) as usize,
        max_pixels: params::max_image_pixels(&task.params),
        max_entry_bytes: max_doc,
    };
    let limit = file_limit(shared, task);
    clock.backend(|| {
        // 列出也限时：解析损坏的文档可能很慢
        let (path, ext, o) = (job.path.clone(), job.ext.clone(), opts.clone());
        let c = Clock::default();
        let listed = run_limited(limit, &c, &shared.stuck, move || {
            let bytes = read_bytes(&path, max_doc).map_err(ListErr::Read)?;
            let src = Source::open(&ext, bytes).map_err(|err| ListErr::Parse(err.to_string()))?;
            let Some(src) = src else {
                return Ok(None);
            };
            let list = src
                .list(&o)
                .map_err(|err| ListErr::Parse(err.to_string()))?;
            Ok(Some((Arc::new(src), list)))
        });
        // 列出失败：记 1 个失败、已有的内嵌片段保留，文档进入失败列表，重试时从头算
        let unlisted = || Collected {
            keep: true,
            failed: 1,
            ..done(Vec::new(), resume)
        };
        let (src, list) = match listed {
            Ok(Ok(Some(v))) => v,
            Ok(Ok(None)) => return done(Vec::new(), resume),
            Ok(Err(ListErr::Read(err))) => {
                return Collected {
                    meta_json: None,
                    keep: true,
                    read_error: Some(err),
                    ..done(Vec::new(), resume)
                };
            }
            Ok(Err(ListErr::Parse(msg))) => {
                log::warn!("无法列出 {} 中的内嵌媒体：{msg}", job.path.display());
                return unlisted();
            }
            Err(err) => {
                log::warn!(
                    "列出 {} 中的内嵌媒体时出错或超时：{err:?}",
                    job.path.display()
                );
                return unlisted();
            }
        };
        let threads = int(&task.params, "index_threads").max(1) as usize;
        let mut out: Vec<Media> = Vec::new();
        // 这一批已处理（含跳过的）个数与其中无法识别的个数
        let (mut handled, mut failed) = (0, 0);
        for e in list.into_iter().filter(|e| e.n >= start) {
            let n = e.n;
            // 这份文档已持有内存额度时不再等待额度（`MediaMem::lease`）
            let held = out.iter().any(|m| m._mem.is_some());
            let step = if handled > 0 && shared.stuck.count() >= threads {
                Step::Stop
            } else {
                one(shared, task, &src, e, &opts, limit, !held)
            };
            if matches!(step, Step::Failed) {
                failed += 1;
            }
            // 资源不够时从这一个接着算；超时的已计入失败，从下一个接着算
            let next = match step {
                Step::Stop => Some(n),
                Step::Timeout => {
                    failed += 1;
                    Some(n + 1)
                }
                _ => None,
            };
            if let Some(next) = next {
                log::info!(
                    "{} 的内嵌媒体分批处理，下一批从第 {next} 个开始。",
                    job.path.display()
                );
                return Collected {
                    media: out,
                    meta_json: Some(
                        serde_json::json!({ "media_partial": tokens, "media_next": next })
                            .to_string(),
                    ),
                    more: true,
                    failed,
                    ..done(Vec::new(), resume)
                };
            }
            if let Step::Done(m) = step {
                out.extend(m.map(|m| *m));
            }
            handled += 1;
        }
        Collected {
            failed,
            ..done(out, resume)
        }
    })
}

/// 列出媒体失败
enum ListErr {
    /// 读不出文档
    Read(rummage_extract::ReadError),
    /// 文档无法解析
    Parse(String),
}

/// 处理一个媒体的结果
enum Step {
    /// 处理完（近乎纯色、全是静音、超过时长或大小上限的为空）
    Done(Option<Box<Media>>),
    /// 无法识别（损坏、解码出错），跳过并计入失败
    Failed,
    /// 超时：计入失败，这一批到此为止
    Timeout,
    /// 资源不够，这一批到此为止
    Stop,
}

/// 在单独的任务线程中限时处理一个媒体；超时或出错时写日志并跳过
fn one(
    shared: &Arc<Shared>,
    task: &Task,
    src: &Arc<Source>,
    e: Embedded,
    opts: &MediaOpts,
    limit: std::time::Duration,
    wait: bool,
) -> Step {
    let clock = Arc::new(Clock::default());
    let n = e.n;
    let (s, t, sr, o, c) = (
        shared.clone(),
        task.clone(),
        src.clone(),
        opts.clone(),
        clock.clone(),
    );
    let res = run_limited(limit, &clock, &shared.stuck, move || {
        load(&s, &t, &sr, &e, &o, &c, wait)
    });
    let path = task.job.path.display();
    match res {
        Ok(Ok(m)) => Step::Done(m.map(Box::new)),
        Ok(Err(LoadErr::Stop)) => Step::Stop,
        Ok(Err(LoadErr::Ignore(msg))) => {
            log::info!("{path} 中的第 {n} 个内嵌媒体{msg}，已跳过。");
            Step::Done(None)
        }
        Ok(Err(LoadErr::Skip(msg))) => {
            log::warn!("{path} 中的第 {n} 个内嵌媒体无法识别，已跳过：{msg}");
            Step::Failed
        }
        Err(Limit::Timeout) => {
            log::warn!(
                "处理 {path} 中的第 {n} 个内嵌媒体超过 {} 秒，已跳过。",
                limit.as_secs()
            );
            Step::Timeout
        }
        Err(Limit::Panic(msg)) => {
            log::error!("处理 {path} 中的第 {n} 个内嵌媒体时出错，已跳过：{msg}");
            Step::Failed
        }
    }
}

/// 解压到 `tmp\embed\` 的音视频，释放时删除
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.0)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("无法删除临时文件 {}：{err}", self.0.display());
        }
    }
}

/// 取出、解码一个媒体失败
enum LoadErr {
    /// 内存额度不够或正在停止：这一批到此为止
    Stop,
    /// 跳过这个媒体并计入失败，附原因（写日志）
    Skip(String),
    /// 超过时长或大小上限：与单独的文件一样只写日志、跳过，不算失败
    Ignore(&'static str),
}

impl From<String> for LoadErr {
    fn from(msg: String) -> Self {
        LoadErr::Skip(msg)
    }
}

/// 取出、解码一个媒体；近乎纯色的图片、全是静音的音频为空
fn load(
    shared: &Arc<Shared>,
    task: &Task,
    src: &Source,
    e: &Embedded,
    opts: &MediaOpts,
    clock: &Clock,
    wait: bool,
) -> Result<Option<Media>, LoadErr> {
    let (job, engine) = (&task.job, &task.engine);
    let fail = |fail: MediaFail| match fail {
        MediaFail::TooLong(_) => LoadErr::Ignore("超过时长上限"),
        MediaFail::Failed(err) => LoadErr::Skip(err.to_string()),
        MediaFail::Size(err) => LoadErr::Skip(err.to_string()),
        MediaFail::NoMemory | MediaFail::Released => LoadErr::Stop,
    };
    // 图片解码后的像素也计入内存额度，缩放后只留缩放后的大小
    let mut lease = match (e.kind, e.size) {
        (MediaKind::Image, Some((w, h))) => Some(
            clock
                .backend(|| {
                    shared
                        .media_mem
                        .lease(u64::from(w) * u64::from(h) * 3, wait, &shared.stop)
                })
                .map_err(fail)?,
        ),
        _ => None,
    };
    let loaded = src.load(&e.at, opts).map_err(|err| match err {
        rummage_extract::media::MediaError::TooLarge(_) => LoadErr::Ignore("超过大小上限"),
        err => LoadErr::Skip(err.to_string()),
    })?;
    let base = serde_json::json!({ "embed": e.locator() });
    if e.kind == MediaKind::Image {
        let rgb = match loaded {
            Loaded::Rgb {
                width,
                height,
                data,
            } => rummage_media::Rgb {
                width,
                height,
                data,
            },
            Loaded::File(bytes) => rummage_media::decode_bytes(&bytes, &job.path, opts.max_pixels)
                .map_err(|err| match err {
                    rummage_media::Error::TooLarge { .. } => LoadErr::Ignore("超过像素上限"),
                    err => LoadErr::Skip(err.to_string()),
                })?,
            Loaded::Range(..) => return Err("图片不应按字节范围读取".to_string().into()),
        };
        if rummage_media::is_flat(&rgb) {
            return Ok(None);
        }
        let rgb = rummage_media::fit(rgb, engine.tier).map_err(|err| err.to_string())?;
        if let Some(l) = &mut lease {
            l.shrink_to(rgb.data.len() as u64);
        }
        let sp = engine.image_space.clone();
        return Ok(Some(Media {
            modality: "image",
            items: vec![Item {
                locator: base.to_string(),
                text: None,
                input: Input::Image {
                    rgb,
                    tokens: engine.tier,
                },
                space: Some(Box::new((sp.clone(), sp.preprocess.clone()))),
            }],
            space: sp,
            fallback: None,
            _mem: lease,
        }));
    }
    // 交给 ffmpeg 读取之前再确认一次不是占位文件
    ensure_local(&job.path).map_err(|err| err.to_string())?;
    let (path, _tmp) = match loaded {
        Loaded::Range(start, end) => (rummage_media::ffmpeg::subfile(&job.path, start, end), None),
        Loaded::File(bytes) => {
            let tmp = temp_path(&shared.opts.embed_tmp, job.file_id, e);
            // 先建守卫：写到一半失败时也删掉
            let guard = TempFile(tmp.clone());
            fs::create_dir_all(&shared.opts.embed_tmp)
                .and_then(|()| fs::write(&tmp, bytes))
                .map_err(|err| format!("无法写入临时文件 \"{}\"：{err}", tmp.display()))?;
            (tmp, Some(guard))
        }
        Loaded::Rgb { .. } => return Err("音视频不应解码为图像".to_string().into()),
    };
    let loc = |start: u64, end: u64| {
        let mut v = base.clone();
        v["start_ms"] = start.into();
        v["end_ms"] = end.into();
        v.to_string()
    };
    let decoded = match e.kind {
        MediaKind::Audio => audio_items(shared, task, &path, clock, wait, &loc),
        _ => video_items(shared, task, &path, clock, wait, &loc),
    };
    let d: Decoded = decoded.map_err(fail)?;
    if d.items.is_empty() {
        return Ok(None);
    }
    let modality = match (e.kind, d.fallback.is_some()) {
        (MediaKind::Audio, _) => "audio",
        (_, true) => "video_av",
        _ => "video_v",
    };
    let own = Box::new((d.space.clone(), d.space.preprocess.clone()));
    let items = d
        .items
        .into_iter()
        .map(|it| Item {
            space: it.space.or_else(|| Some(own.clone())),
            ..it
        })
        .collect();
    Ok(Some(Media {
        modality,
        space: d.space,
        fallback: d.fallback,
        items,
        _mem: Some(d.lease),
    }))
}

/// 内嵌媒体所在的压缩包条目（`At::Part`）的扩展名，没有时为 `bin`；解压到临时文件时用，ffmpeg 据此识别格式
pub(crate) fn part_ext(part: Option<&str>) -> &str {
    part.and_then(|p| p.rsplit_once('.')).map_or("bin", |x| x.1)
}

/// 压缩存放的音视频解压后的临时文件：`<文件 ID>-<序号>.<扩展名>`
fn temp_path(dir: &Path, file_id: i64, e: &Embedded) -> PathBuf {
    let ext = part_ext(match &e.at {
        rummage_extract::media::At::Part(p) => Some(p),
        _ => None,
    });
    dir.join(format!("{file_id}-{}.{ext}", e.n))
}
