//! 预处理：读取文件、算哈希、解码缩放或切片，产出待推理的片段。
//! - 每个预处理线程一次处理一个文件，文件本身放在单独的任务线程中执行，`catch_unwind` 捕获 panic；
//!   超过时限 `file_timeout_s` 就放弃等待该线程（它结束后结果被丢弃），记一次失败。
//!   放弃等待、还在运行的任务线程数（`Stuck`）达到 `index_threads` 时，预处理线程暂停领取新作业，直到有线程退出，
//!   避免卡死的解析器不断累积线程。
//!   等待推理进程分词（`count_tokens`）的时间不计入时限：分词请求与其他文件的推理排在同一个队列里，
//!   CPU 上长文档要等很久，那不是文件本身的问题。
//! - 顺序：不做语义索引的种类直接删除作业，不读文件 → `quick_hash`（内含 `ensure_local`，同一个句柄顺便取文件 ID，
//!   随结果写回，用于识别改名与移动）→ 黑名单 →
//!   图片：内容哈希，库中已有同空间向量（长图还要有当前参数的切片）的直接复制（宽高随之复制），否则解码原图、
//!   记下宽高、缩放得到整图片段（像素数超过 `max_image_mp` 的只记下所用的上限，`Outcome::TooLong`）；
//!   长宽比超过 `long_image_ratio` 的长图另按 `rummage_media::slices` 切段，每段按切片档位（`long_image_tokens`）
//!   缩放成一个片段，locator 记区域，向量在该档位的图像空间；整图按 `image_tokens`；
//!   `.ts` / `.mts`（种类为 `other`）：读文件头判断是否为视频，是视频就按视频处理，否则只记下已判定（`Outcome::NotVideo`）。
//!   文档：读取、切片，每片一个片段，向量的参数版本带分段参数（`space::text_version`）。
//!   PDF、Office 等经 Xberg 提取为 Markdown 后按 Markdown 切片，片段记录标题路径与字符范围，PDF 与演示文稿
//!   另记页码（`page_start` / `page_end` 或 `slide_start` / `slide_end`）；Markdown 的行号对用户没有意义，不记。
//!   加密的文档只保留元数据（`Outcome::Encrypted`）。
//!   音频：内容哈希，库中已有同空间向量的直接复制（片段、元数据随之复制）；否则再做一次占位文件检查，ffprobe 读元数据，
//!   超过时长上限 `max_audio_min` 的只记元数据（`Outcome::TooLong`）；然后 ffmpeg 解码为 16 kHz 单声道，按
//!   `audio_segment_s` / `audio_overlap_s` 切段、跳过低于 `audio_silence_db` 的静音段，每段一个片段，locator 记毫秒起止。
//!   ffprobe、ffmpeg 各有自己的时限（解码按时长放宽，见 `audio`），等它们的时间与等推理进程一样不计入单文件时限。
//!   解码前先占用内存额度（`MediaMem`），额度不够时等前面的音频、视频推理完。
//!   视频（含判定为视频的 `.ts` / `.mts`）见 `video`。
//! - 只读数据库（黑名单、复制来源），写入都交给写库线程。
//! - 失败时作业表中记给用户看的原因（`reason`），原始错误写日志。
use std::any::Any;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use rummage_embed::{Input, Priority, space};
use rummage_extract::chunk::ChunkError;
use rummage_extract::{
    ChunkOpts, Doc, DocError, TextKind, chunk, extract_doc, is_mpeg_ts, is_xberg, read_text,
    title_for,
};
use rummage_index::{
    Claimed, Outcome, PART_MEDIA, PART_TEXT, content_hash, kind_of, quick_hash_with_id, wants_job,
};
use rummage_proto::params;

use super::{Item, Msg, Prepared, Shared, Start, Task, docmedia, float, int, lock, space_row};

mod audio;
pub(super) mod reason;
mod video;

pub(super) use audio::{Decoded, MediaFail, audio_items};
pub(super) use video::video_items;

use reason::Reason;

/// 已解码、等待推理的音频采样与视频帧最多占用的内存（字节）。1 小时的音频解码后约 230 MB，1 小时的视频按“找物体”抽帧约
/// 0.8 GB，预处理线程与等推理的队列里同时压着很多长音频、长视频时会耗尽内存，所以超过时新的文件等前面的推理完再解码；
/// 单个文件超过它（4 小时的音频约 0.9 GB）时，没有别的占用就照常处理。音频按解码出的采样计，切段的副本约多 1/4，
/// 切完后整段采样即释放
const MEDIA_MEM: u64 = 1 << 30;

/// 任务线程的栈：Xberg 解析文档时栈用量大，debug 构建下 1 MB 的栈解析一个两页的 PDF 就会溢出，默认的 2 MB 余量不足
const FILE_STACK: usize = 16 * 1024 * 1024;

/// 预处理的结果：待推理的文件，或不需要推理的最终结果（含失败）
enum Prep {
    Ready(Box<Prepared>),
    Done(Outcome),
}

/// 记录等待推理进程的时间，这段时间不计入单文件时限
#[derive(Default)]
pub(super) struct Clock {
    /// (已累计, 当前这次等待的开始时间)
    inner: Mutex<(Duration, Option<Instant>)>,
}

impl Clock {
    pub(super) fn backend<T>(&self, f: impl FnOnce() -> T) -> T {
        lock(&self.inner).1 = Some(Instant::now());
        let out = f();
        let mut g = lock(&self.inner);
        if let Some(t) = g.1.take() {
            g.0 += t.elapsed();
        }
        out
    }

    fn waited(&self) -> Duration {
        let g = lock(&self.inner);
        g.0 + g.1.map_or(Duration::ZERO, |t| t.elapsed())
    }
}

/// 已放弃等待、还在运行的任务线程
#[derive(Default)]
pub(super) struct Stuck {
    count: Mutex<usize>,
    cv: Condvar,
}

impl Stuck {
    /// 已放弃等待、还在运行的任务线程数
    pub(super) fn count(&self) -> usize {
        *lock(&self.count)
    }

    /// 阻塞到少于 `limit` 个，或 `stop` 为真
    fn wait_below(&self, limit: usize, stop: &AtomicBool) {
        let mut n = lock(&self.count);
        if *n < limit {
            return;
        }
        log::warn!(
            "有 {} 个文件处理超时后仍未结束，暂停领取新的文件，等它们结束。",
            *n
        );
        while *n >= limit && !stop.load(Ordering::Relaxed) {
            n = self
                .cv
                .wait_timeout(n, Duration::from_secs(1))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// 停止时唤醒等待中的预处理线程
    pub(super) fn wake_all(&self) {
        self.cv.notify_all();
    }
}

/// 已解码、等待推理的音频采样、视频帧与文档内嵌图片占用的内存（见 `MEDIA_MEM`）
pub(super) struct MediaMem {
    used: Mutex<u64>,
    cv: Condvar,
    /// 额度，即 `MEDIA_MEM`；测试中调小
    pub(super) limit: AtomicU64,
    /// 正在等额度的申请数
    pub(super) waiting: AtomicUsize,
}

impl Default for MediaMem {
    fn default() -> Self {
        MediaMem {
            used: Mutex::new(0),
            cv: Condvar::new(),
            limit: AtomicU64::new(MEDIA_MEM),
            waiting: AtomicUsize::new(0),
        }
    }
}

/// 占用的额度，释放时归还
pub(super) struct MediaLease {
    mem: Arc<MediaMem>,
    bytes: u64,
}

impl MediaLease {
    /// 解码出的大图缩放后占用变少，只留缩放后的大小
    pub(super) fn shrink_to(&mut self, bytes: u64) {
        if bytes < self.bytes {
            *lock(&self.mem.used) -= self.bytes - bytes;
            self.bytes = bytes;
            self.mem.cv.notify_all();
        }
    }
}

impl Drop for MediaLease {
    fn drop(&mut self) {
        *lock(&self.mem.used) -= self.bytes;
        self.mem.cv.notify_all();
    }
}

impl MediaMem {
    /// 占用额度。`wait` 为否时不等待：已有占用、加上这次又超过额度，或有别的申请正在等额度（先让给它）时返回 `NoMemory`。
    /// 等待的一方（每批的第一个媒体、单独的文件）从不让出，不会两边互相让而谁都拿不到。文档的内嵌媒体在已持有额度
    /// （前面的媒体还没推理完，额度要等这份文档整体推理完才归还）时用它，否则同一份文档里的长音视频会互相等待而卡住
    pub(super) fn lease(
        self: &Arc<Self>,
        bytes: u64,
        wait: bool,
        stop: &AtomicBool,
    ) -> Result<MediaLease, MediaFail> {
        if wait {
            return self.acquire(bytes, stop).ok_or(MediaFail::Released);
        }
        let mut used = lock(&self.used);
        if *used > 0
            && (*used + bytes > self.limit.load(Ordering::Relaxed)
                || self.waiting.load(Ordering::Relaxed) > 0)
        {
            return Err(MediaFail::NoMemory);
        }
        *used += bytes;
        Ok(MediaLease {
            mem: self.clone(),
            bytes,
        })
    }

    /// 阻塞到额度够用（没有别的占用时总是够用），或 `stop` 为真时返回空
    fn acquire(self: &Arc<Self>, bytes: u64, stop: &AtomicBool) -> Option<MediaLease> {
        let mut used = lock(&self.used);
        // 在锁内增减，不等待的申请在同一把锁内看到它
        self.waiting.fetch_add(1, Ordering::Relaxed);
        while *used > 0 && *used + bytes > self.limit.load(Ordering::Relaxed) {
            if stop.load(Ordering::Relaxed) {
                self.waiting.fetch_sub(1, Ordering::Relaxed);
                return None;
            }
            used = self
                .cv
                .wait_timeout(used, Duration::from_secs(1))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        self.waiting.fetch_sub(1, Ordering::Relaxed);
        *used += bytes;
        Some(MediaLease {
            mem: self.clone(),
            bytes,
        })
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum Limit {
    Timeout,
    Panic(String),
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "未知错误".to_string())
}

/// 在单独的线程中执行 `f`，最多等 `limit` 加上 `clock` 记下的等待时间；超时就不再等它，计入 `stuck`，
/// 它结束时再减去（两边在同一把锁内交换 `finished`，后到的一方负责计数）
pub(super) fn run_limited<T: Send + 'static>(
    limit: Duration,
    clock: &Clock,
    stuck: &Arc<Stuck>,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Limit> {
    let (tx, rx) = mpsc::channel();
    let finished = Arc::new(AtomicBool::new(false));
    let (fin, st) = (finished.clone(), stuck.clone());
    let spawned = thread::Builder::new()
        .name("index-file".into())
        .stack_size(FILE_STACK)
        .spawn(move || {
            let res = panic::catch_unwind(AssertUnwindSafe(f));
            {
                let mut n = lock(&st.count);
                if fin.swap(true, Ordering::Relaxed) {
                    *n -= 1;
                    st.cv.notify_all();
                }
            }
            let _ = tx.send(res);
        });
    if let Err(err) = spawned {
        return Err(Limit::Panic(format!("无法启动线程：{err}")));
    }
    let start = Instant::now();
    loop {
        let deadline = start + limit + clock.waited();
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            let mut n = lock(&stuck.count);
            if !finished.swap(true, Ordering::Relaxed) {
                *n += 1;
            }
            return Err(Limit::Timeout);
        };
        match rx.recv_timeout(left) {
            Ok(Ok(v)) => return Ok(v),
            Ok(Err(payload)) => return Err(Limit::Panic(panic_message(&*payload))),
            // 期间可能又等了推理进程，重新计算期限
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(Limit::Panic("处理线程意外退出".to_string()));
            }
        }
    }
}

/// 预处理线程：逐个取作业，限时处理，结果交给推理线程或直接交给写库线程
pub(super) fn supervise(
    shared: &Arc<Shared>,
    tasks: &Mutex<Receiver<Task>>,
    ready: &SyncSender<Prepared>,
    out: &Sender<Msg>,
) {
    loop {
        let threads = int(&lock(&shared.params), "index_threads").max(1) as usize;
        shared.stuck.wait_below(threads, &shared.stop);
        let Ok(task) = lock(tasks).recv() else {
            return;
        };
        // 正在停止：作业由 `Pipeline::stop` 放回
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }
        let mut job = task.job.clone();
        let limit = file_limit(shared, &task);
        let clock = Arc::new(Clock::default());
        let hash = Arc::new(Mutex::new((None, None)));
        let (s, c, h) = (shared.clone(), clock.clone(), hash.clone());
        let res = run_limited(limit, &clock, &shared.stuck, move || {
            preprocess(&s, task, &c, &h)
        });
        let (quick_hash, file_id) = lock(&hash).clone();
        job.volume_file_id = file_id;
        let error = match res {
            Ok(Prep::Ready(p)) => {
                if ready.send(*p).is_err() {
                    return;
                }
                continue;
            }
            Ok(Prep::Done(outcome)) => {
                if out.send(Msg::Outcome(job, outcome)).is_err() {
                    return;
                }
                continue;
            }
            Err(Limit::Timeout) => {
                log::warn!("处理 {} 超时，已放弃。", job.path.display());
                format!("处理此文件超过 {} 秒，已放弃。", limit.as_secs_f64())
            }
            Err(Limit::Panic(msg)) => {
                log::error!("处理 {} 时出错：{msg}", job.path.display());
                "处理此文件时程序出错，已跳过。".to_string()
            }
        };
        if out
            .send(Msg::Outcome(job, Outcome::Failed { quick_hash, error }))
            .is_err()
        {
            return;
        }
    }
}

/// 记一次失败：作业表中记给用户看的原因，原始错误（带路径与底层细节）写日志
fn failed(job: &Claimed, quick_hash: Option<&[u8]>, err: impl Reason) -> Prep {
    log::warn!("处理 {} 失败：{err}", job.path.display());
    Prep::Done(Outcome::Failed {
        quick_hash: quick_hash.map(<[u8]>::to_vec),
        error: err.reason(),
    })
}

/// 在任务线程中执行；`hash` 一算出 `quick_hash` 就记下它和文件 ID：超时或 panic 时 `quick_hash` 用于黑名单，
/// 文件 ID 随任何结果写回
fn preprocess(
    shared: &Arc<Shared>,
    mut task: Task,
    clock: &Clock,
    hash: &Mutex<(Option<Vec<u8>>, Option<String>)>,
) -> Prep {
    let kind = kind_of(&task.job.ext);
    if !wants_job(kind, &task.job.ext) {
        return Prep::Done(Outcome::Skipped);
    }
    let (qh, file_id) = match quick_hash_with_id(&task.job.path) {
        Ok((h, id)) => (h.to_vec(), id),
        Err(err) => return failed(&task.job, None, err),
    };
    *lock(hash) = (Some(qh.clone()), file_id.clone());
    task.job.volume_file_id = file_id;
    #[cfg(test)]
    super::tests::hook(&task.job.name);
    match lock(&shared.db).is_blacklisted(&qh) {
        Ok(false) => {}
        Ok(true) => return Prep::Done(Outcome::Blacklisted { quick_hash: qh }),
        Err(err) => return failed(&task.job, Some(&qh), err),
    }
    match kind {
        "image" => image(shared, task, qh),
        "audio" => audio::audio_file(shared, task, qh, clock),
        "video" => video::video_file(shared, task, qh, clock),
        // `wants_job` 中 `other` 只有待判定的 `.ts` / `.mts`
        "other" => match is_mpeg_ts(&task.job.path) {
            // 处理视频时就地按视频建索引，否则只改种类（`Outcome::Video`）
            Ok(true) if task.engine.video => video::video_file(shared, task, qh, clock),
            Ok(true) => Prep::Done(Outcome::Video),
            Ok(false) => Prep::Done(Outcome::NotVideo { quick_hash: qh }),
            Err(err) => failed(&task.job, Some(&qh), err),
        },
        _ => text(shared, task, qh, clock),
    }
}

fn image(shared: &Shared, task: Task, qh: Vec<u8>) -> Prep {
    let (job, engine) = (&task.job, &task.engine);
    let ch = match content_hash(&job.path) {
        Ok(h) => h.to_vec(),
        Err(err) => return failed(job, Some(&qh), err),
    };
    let max_ratio = float(&task.params, "long_image_ratio");
    let max_slices = int(&task.params, "long_image_max_slices").max(0) as u32;
    let tier = int(&task.params, "image_tokens") as u32;
    let slice_tier = int(&task.params, "long_image_tokens") as u32;
    let slice_version = engine.cfg.slice_version(slice_tier, max_ratio, max_slices);
    let image_space = space_row(&engine.cfg, Some(tier));
    let copy =
        lock(&shared.db).copy_source(&ch, &image_space, &slice_version, max_ratio, job.file_id);
    match copy {
        Ok(None) => {}
        Ok(Some(from)) => {
            return Prep::Done(Outcome::Copied {
                quick_hash: qh,
                kind: "image",
                content_hash: ch,
                space_id: image_space.id.clone(),
                slice_version,
                from,
            });
        }
        Err(err) => return failed(job, Some(&qh), err),
    }
    // 长图读原图，不用缩略图
    let max_pixels = params::max_image_pixels(&task.params);
    let img = match rummage_media::decode(&job.path, max_pixels) {
        Ok(img) => img,
        // 同超过时长上限的音频：只按文件名搜索，记下所用的上限，扫描据此按当前上限重新判定
        Err(rummage_media::Error::TooLarge { .. }) => {
            return Prep::Done(Outcome::TooLong {
                quick_hash: qh,
                meta_json: serde_json::json!({ "pixel_limit": max_pixels }).to_string(),
            });
        }
        Err(err) => return failed(job, Some(&qh), err),
    };
    let size = (img.width, img.height);
    let slice_space = space_row(&engine.cfg, Some(slice_tier));
    let mut slices = Vec::new();
    for r in rummage_media::slices(img.width, img.height, max_ratio, max_slices) {
        let rgb = match rummage_media::fit(rummage_media::crop(&img, r), slice_tier) {
            Ok(rgb) => rgb,
            Err(err) => return failed(job, Some(&qh), err),
        };
        slices.push(Item {
            locator: region_locator(r),
            text: None,
            input: Input::Image {
                rgb,
                tokens: slice_tier,
            },
            space: Some(Box::new((slice_space.clone(), slice_version.clone()))),
        });
    }
    let rgb = match rummage_media::fit(img, tier) {
        Ok(rgb) => rgb,
        Err(err) => return failed(job, Some(&qh), err),
    };
    // 整图片段在前，不带区域
    let mut items = vec![Item {
        locator: "{}".to_string(),
        text: None,
        input: Input::Image { rgb, tokens: tier },
        space: None,
    }];
    items.extend(slices);
    Prep::Ready(Box::new(Prepared {
        job: task.job.clone(),
        engine: task.engine.clone(),
        generation: task.generation,
        kind: "image",
        space: image_space,
        fallback: None,
        quick_hash: qh,
        content_hash: Some(ch),
        items,
        size: Some(size),
        meta_json: None,
        _mem: None,
        media: Vec::new(),
        replace: None,
        more: false,
        media_failed: None,
    }))
}

/// 音频段、视频窗口的定位：毫秒起止，对应前端 `SegmentHit.locatorJson` 的说明
fn time_locator(start_ms: u64, end_ms: u64) -> String {
    serde_json::json!({ "start_ms": start_ms, "end_ms": end_ms }).to_string()
}

/// 单文件时限；ffprobe 也用它
pub(super) fn file_limit(shared: &Shared, task: &Task) -> Duration {
    shared
        .opts
        .file_timeout
        .unwrap_or_else(|| Duration::from_secs(int(&task.params, "file_timeout_s").max(1) as u64))
}

/// 长图切片的定位：在原图（已按 EXIF 方向转正）中的像素区域，对应前端 `SegmentHit.locatorJson` 的说明
fn region_locator(r: rummage_media::Region) -> String {
    serde_json::json!({
        "region": { "x": r.x, "y": r.y, "width": r.width, "height": r.height }
    })
    .to_string()
}

/// 文档：按作业要算的部分（`Claimed::parts`）提取正文并切片、取出内嵌媒体（`docmedia`）
fn text(shared: &Arc<Shared>, task: Task, qh: Vec<u8>, clock: &Clock) -> Prep {
    let (job, engine) = (&task.job, &task.engine);
    let (with_text, with_media) = (job.parts & PART_TEXT != 0, job.parts & PART_MEDIA != 0);
    let items = if with_text {
        match text_items(shared, &task, &qh, clock) {
            Ok(items) => items,
            Err(prep) => return *prep,
        }
    } else {
        Vec::new()
    };
    let mut c = with_media.then(|| docmedia::collect(shared, &task, clock));
    // 只算内嵌媒体时读不出文档：与单独的文件读不出相同（有正文时正文已先读过，读不出在前面就已返回）
    if let Some(err) = c.as_mut().and_then(|c| c.read_error.take()) {
        return failed(job, Some(&qh), err);
    }
    let keep = c.as_ref().is_some_and(|c| c.keep);
    // 接着上一批算内嵌媒体、或列出失败时已有的内嵌片段保留
    let replace = match (with_text, with_media) {
        (true, true) if !keep => None,
        (true, _) => Some(vec!["text"]),
        _ if keep => Some(Vec::new()),
        _ => Some(docmedia::modalities(engine)),
    };
    let media_failed = c.as_ref().map(|c| (c.failed, c.resume));
    let more = c.as_ref().is_some_and(|c| c.more);
    let (media, meta_json) = c.map_or((Vec::new(), None), |c| (c.media, c.meta_json));
    Prep::Ready(Box::new(Prepared {
        job: task.job.clone(),
        engine: task.engine.clone(),
        generation: task.generation,
        kind: "document",
        space: engine.text_space.clone(),
        fallback: None,
        quick_hash: qh,
        content_hash: None,
        items,
        size: None,
        meta_json,
        _mem: None,
        media,
        replace,
        more,
        media_failed,
    }))
}

/// 提取正文并切片，每片一个片段；失败或加密时给出最终结果
fn text_items(
    shared: &Shared,
    task: &Task,
    qh: &[u8],
    clock: &Clock,
) -> Result<Vec<Item>, Box<Prep>> {
    let (job, engine) = (&task.job, &task.engine);
    let ext = job.ext.as_str();
    let max_doc = params::max_doc_bytes(&task.params);
    let xberg = is_xberg(ext);
    let doc = if xberg {
        extract_doc(&job.path, max_doc)
    } else {
        read_text(&job.path, max_doc)
            .map(|f| Doc {
                text: f.text,
                pages: Vec::new(),
                page_unit: "page",
            })
            .map_err(DocError::from)
    };
    let doc = match doc {
        Ok(d) => d,
        Err(DocError::Encrypted(_)) => {
            return Err(Box::new(Prep::Done(Outcome::Encrypted {
                quick_hash: qh.to_vec(),
            })));
        }
        Err(err) => return Err(Box::new(failed(job, Some(qh), err))),
    };
    let mode = if xberg || ext == "md" {
        TextKind::Markdown
    } else {
        TextKind::Text
    };
    let opts = ChunkOpts::from_params(&task.params);
    // 向量的参数版本带分段参数，设置改了据此判断要不要重建（`rebuild`）
    let own = Box::new((
        engine.text_space.clone(),
        space::text_version(opts.target, opts.overlap_pct, opts.min),
    ));
    // 标题为文件名去掉扩展名
    let title = title_for(&job.path);
    let input = |text: &str| Input::Document {
        title: Some(title.clone()),
        text: text.to_string(),
    };
    let mut count =
        |s: &str| clock.backend(|| engine.backend.count_tokens(&input(s), Priority::Index));
    let start = Start {
        at: Instant::now(),
        generation: task.generation,
    };
    let chunks = match chunk(&doc.text, mode, &opts, &mut count) {
        Ok(c) => c,
        Err(err) => {
            if let ChunkError::Count(e) = &err {
                shared.engine_failed(engine, e, start);
            }
            return Err(Box::new(failed(job, Some(qh), err)));
        }
    };
    Ok(chunks
        .into_iter()
        .map(|c| {
            let mut loc = serde_json::json!({
                "char_start": c.char_start,
                "char_end": c.char_end,
                "heading": c.heading,
            });
            if !xberg {
                loc["line_start"] = c.line_start.into();
                loc["line_end"] = c.line_end.into();
            }
            if let Some((a, b)) = doc.page_range(c.char_start, c.char_end) {
                loc[format!("{}_start", doc.page_unit)] = a.into();
                loc[format!("{}_end", doc.page_unit)] = b.into();
            }
            Item {
                locator: loc.to_string(),
                input: input(&c.text),
                text: Some(c.text),
                space: Some(own.clone()),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use rummage_media::audio;

    use super::*;

    #[test]
    fn region_locator_json() {
        let r = rummage_media::Region {
            x: 0,
            y: 2160,
            width: 1080,
            height: 1080,
        };
        let loc = region_locator(r);
        assert_eq!(
            loc,
            r#"{"region":{"x":0,"y":2160,"width":1080,"height":1080}}"#
        );
        let v: serde_json::Value = serde_json::from_str(&loc).unwrap();
        assert_eq!(v["region"]["y"], 2160);
    }

    #[test]
    fn time_locator_json() {
        let s = audio::Span {
            start: 16_000 * 192,
            end: 16_000 * 202 + 8,
        };
        let v: serde_json::Value =
            serde_json::from_str(&time_locator(s.start_ms(), s.end_ms())).unwrap();
        assert_eq!(
            v,
            serde_json::json!({ "start_ms": 192000, "end_ms": 202000 })
        );
    }

    #[test]
    fn media_mem_waits_for_release() {
        let mem = Arc::new(MediaMem::default());
        let stop = AtomicBool::new(false);
        // 单个超过额度的文件在没有别的占用时照常处理
        let big = mem.acquire(MEDIA_MEM + 1, &stop).unwrap();
        let (m, t) = (mem.clone(), Instant::now());
        let waiter = thread::spawn(move || {
            let lease = m.acquire(10, &AtomicBool::new(false)).unwrap();
            (t.elapsed(), lease.bytes)
        });
        thread::sleep(Duration::from_millis(200));
        drop(big);
        let (waited, bytes) = waiter.join().unwrap();
        assert!(waited >= Duration::from_millis(150), "{waited:?}");
        assert_eq!(bytes, 10);
        assert_eq!(*lock(&mem.used), 0);
        // 停止时不再等
        let _held = mem.acquire(MEDIA_MEM, &stop).unwrap();
        assert!(mem.acquire(10, &AtomicBool::new(true)).is_none());
    }

    #[test]
    fn limited_runs() {
        let clock = Clock::default();
        let stuck = Arc::new(Stuck::default());
        assert_eq!(
            run_limited(Duration::from_secs(5), &clock, &stuck, || 7),
            Ok(7)
        );
        let slow = run_limited(Duration::from_millis(50), &clock, &stuck, || {
            thread::sleep(Duration::from_millis(500))
        });
        assert_eq!(slow, Err(Limit::Timeout));
        // 超时的线程还在运行，结束后计数归零
        assert_eq!(stuck.count(), 1);
        let deadline = Instant::now() + Duration::from_secs(5);
        while stuck.count() > 0 {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        let res: Result<(), _> =
            run_limited(Duration::from_secs(5), &clock, &stuck, || panic!("坏了"));
        assert_eq!(res, Err(Limit::Panic("坏了".into())));
        let res: Result<(), _> = run_limited(Duration::from_secs(5), &clock, &stuck, || {
            panic!("{}", String::from("格式化的"))
        });
        assert_eq!(res, Err(Limit::Panic("格式化的".into())));
        assert_eq!(stuck.count(), 0);
    }

    #[test]
    fn wait_below_blocks_until_threads_exit() {
        let clock = Clock::default();
        let stuck = Arc::new(Stuck::default());
        let res = run_limited(Duration::from_millis(20), &clock, &stuck, || {
            thread::sleep(Duration::from_millis(400))
        });
        assert_eq!(res, Err(Limit::Timeout));
        let t = Instant::now();
        stuck.wait_below(1, &AtomicBool::new(false));
        assert!(
            t.elapsed() >= Duration::from_millis(250),
            "{:?}",
            t.elapsed()
        );
        assert_eq!(stuck.count(), 0);
        // 停止时不再等待
        let _ = run_limited(Duration::from_millis(20), &clock, &stuck, || {
            thread::sleep(Duration::from_millis(400))
        });
        let t = Instant::now();
        stuck.wait_below(1, &AtomicBool::new(true));
        assert!(t.elapsed() < Duration::from_millis(200));
    }

    #[test]
    fn backend_wait_not_counted() {
        let clock = Arc::new(Clock::default());
        let c = clock.clone();
        // 等推理进程 300 毫秒，自身只用很少时间：时限 100 毫秒也不超时
        let stuck = Arc::new(Stuck::default());
        let res = run_limited(Duration::from_millis(100), &clock, &stuck, move || {
            c.backend(|| thread::sleep(Duration::from_millis(300)));
            1
        });
        assert_eq!(res, Ok(1));
        assert!(clock.waited() >= Duration::from_millis(300));
    }
}
