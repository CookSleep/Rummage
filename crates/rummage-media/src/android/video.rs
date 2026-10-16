//! Android 的视频：元数据、整段抽帧与命中时刻的画面，用 `AMediaExtractor` + `AMediaCodec`，解码到 `AImageReader`。
//!
//! - 轨道：MIME 为 `video/` 的轨道（`video/mjpeg` 等封面图类型除外）。元数据取第一条；解码时按顺序取第一条
//!   系统能创建解码器的，都建不了时返回 `MediaError::Decode`（日志写明各轨道的 MIME）。
//! - 解码器输出到 AImageReader（`YUV_420_888`）再读平面：直接读输出缓冲区时颜色格式因芯片而异（有厂商私有的分块格式），
//!   经 ImageReader 得到的都是能按行距、像素间距读取的 YUV 4:2:0。`KEY_ROTATION` 置 0，不让解码器处理旋转，
//!   转成 RGB 后自己转正。帧的 pts 取图像的时间戳（渲染时即缓冲区的 pts）。图像借用 ImageReader（`Image<'a>`），
//!   一定先于它删除；同时只持有待定的上一帧与刚收到的一帧。渲染是异步的：渲染一帧后先短暂轮询取到这张图像
//!   （每帧最多等 `FRAME_WAIT`，超时记调试日志后继续，图像晚到时下一轮再取），再取下一个输出缓冲区。
//! - 整段抽帧：从头顺序解码，不逐帧定位；按网格取帧、YUV 转 RGB、转正的规则见 `video::raw`，再用 `resize_bicubic` 缩放。
//!   只转换要输出的帧，内存中只留缩放后的帧。`start_s`（只供测试）同 ffmpeg 的 `-ss`：定位到之前的关键帧，
//!   早于它的帧不渲染，pts 从它算起。
//! - 色彩：矩阵与范围先取解码器输出格式的 `color-standard` / `color-range`（描述实际拿到的像素，转成 SDR 后也是），
//!   再取轨道格式，都没有时同 ffmpeg 的默认（BT.601、有限范围，见 `video::raw`）。输出到 surface 时，Codec2 的
//!   CCodecConfig 会对没有指定的色彩按尺寸填默认值（`ColorUtils::setDefaultCodecColorAspectsIfNeeded`：高清 BT.709、
//!   4K BT.2020），与 ffmpeg 不同；所以轨道格式没有这两项时，配置用的格式中设为 BT.601（`color-standard` = 4）、
//!   有限范围（`color-range` = 2）。码流 VUI 有标注时解码器仍以 VUI 为准，与 ffmpeg 按每帧的标注转换一致。
//! - 10 位 HDR：API 31 起配置时请求转为 SDR（`color-transfer-request` 设为 `COLOR_TRANSFER_SDR_VIDEO`，即 3；
//!   键名与取值见 AOSP `MediaFormat.KEY_COLOR_TRANSFER_REQUEST`，NDK 头文件没有这个键，未知的键解码器会忽略）。
//!   启动后从解码器的输入格式读回这一项：不是 3 而轨道的 `color-transfer` 为 ST2084（6）或 HLG（7）时，解码器不会转换，
//!   返回“格式不受支持”（`MediaError::Decode`）。图像不是 `YUV_420_888`、或色度平面的像素间距不是 1 或 2
//!   （10 位的 P010 为 4）时同样返回这个错误。
//! - 命中画面：`seekTo(PREVIOUS_SYNC)` 后解到第一个 pts 不早于目标时刻的帧，与 ffmpeg 把 `-ss` 放在 `-i` 前相同；
//!   转正后缩到宽不超过 `THUMB_WIDTH`，高按比例取偶数（同 ffmpeg 的 `scale=…:-2`）。
//! - 元数据：`codec` 与 `container` 是 MIME 类型，与电脑上 ffprobe 的编码名、格式名不同；录制时间、机型、位置读不到。
//! - 时限只在解码循环的间隙检查。调用方必须先确认不是网盘占位文件。

use std::ffi::CStr;
use std::marker::PhantomData;
use std::path::Path;
use std::ptr;
use std::time::{Duration, Instant};

use ndk_sys as sys;

use super::{Codec, DEQUEUE_US, Format, Key, Source, decode_err, feed, key};
use crate::ffmpeg::MediaError;
use crate::video::raw::{Matrix, Picker, Plane, rotate, yuv_to_rgb};
use crate::video::{Grid, VideoInfo};
use crate::{Rgb, THUMB_WIDTH, resize_bicubic};

const VIDEO: &str = "视频";
/// ImageReader 最多同时取出的图像数：待定的上一帧与刚收到的一帧，再留出余量，解码器不必等
const MAX_IMAGES: i32 = 4;
/// 输入结束后等最后几帧到达 ImageReader 的上限：渲染是异步的，个别帧被丢掉时不能一直等
const TAIL_WAIT: Duration = Duration::from_secs(1);
/// 渲染一帧后等它到达 ImageReader 的上限
const FRAME_WAIT: Duration = Duration::from_millis(200);
/// 等待渲染的帧到达时每次休眠的时长
const POLL: Duration = Duration::from_micros(500);
/// MediaFormat 的 `color-standard` = BT601_NTSC、`color-range` = LIMITED，见模块说明
const STANDARD_BT601: i32 = 4;
const RANGE_LIMITED: i32 = 2;
/// `color-transfer` 的 ST2084（PQ）与 HLG
const TRANSFER_HDR: [i32; 2] = [6, 7];
/// 封面图之类的“视频”轨
const COVER_MIMES: [&str; 1] = ["video/mjpeg"];
/// AOSP `MediaFormat.KEY_COLOR_TRANSFER_REQUEST` 与 `COLOR_TRANSFER_SDR_VIDEO`，API 31 起
const TRANSFER_REQUEST: &CStr = c"color-transfer-request";
const TRANSFER_SDR: i32 = 3;
const TRANSFER_REQUEST_API: i32 = 31;

fn is_video(mime: &str) -> bool {
    mime.starts_with("video/") && !COVER_MIMES.contains(&mime)
}

/// 第一条视频轨
fn source(path: &Path) -> Result<Source, MediaError> {
    Source::open(path, VIDEO, &is_video, MediaError::NoVideo)
}

/// 按顺序找第一条系统能解码的视频轨，选中它并创建解码器
fn choose_decoder(src: &mut Source, path: &Path) -> Result<Codec, MediaError> {
    let tracks: Vec<_> = src
        .tracks()
        .into_iter()
        .filter(|(_, _, m)| is_video(m))
        .collect();
    let mimes: Vec<String> = tracks.iter().map(|(_, _, m)| m.clone()).collect();
    for (i, f, mime) in tracks {
        let Some(codec) = Codec::new(&mime) else {
            continue;
        };
        src.track = i;
        src.format = f;
        src.mime = mime;
        // SAFETY: 提取器有效，下标来自 `tracks`。
        if unsafe { sys::AMediaExtractor_selectTrack(src.ex, i) } != sys::media_status_t::AMEDIA_OK
        {
            return Err(decode_err(path, VIDEO, "无法选中视频轨"));
        }
        return Ok(codec);
    }
    Err(decode_err(
        path,
        VIDEO,
        format!("系统没有这些视频轨的解码器：{}", mimes.join("、")),
    ))
}

/// 用键名读整数（没有对应 `Key` 的键）
fn int_by_name(f: &Format, name: &CStr) -> Option<i32> {
    let mut v = 0;
    // SAFETY: 只对非空的格式调用；键是以 NUL 结尾的字符串；输出参数指向局部变量。
    unsafe { sys::AMediaFormat_getInt32(f.0, name.as_ptr(), &mut v) }.then_some(v)
}

/// 编码的宽高（未转正）
fn dims(f: &Format) -> Option<(u32, u32)> {
    match (f.int(Key::Width), f.int(Key::Height)) {
        (Some(w), Some(h)) if w > 0 && h > 0 => Some((w as u32, h as u32)),
        _ => None,
    }
}

/// 顺时针转正的角度，只认 90 的倍数
fn rotation(f: &Format) -> i32 {
    let r = f.int(Key::Rotation).unwrap_or(0).rem_euclid(360);
    if r % 90 == 0 { r } else { 0 }
}

fn fps(f: &Format) -> Option<f64> {
    f.int(Key::FrameRate)
        .map(f64::from)
        .or_else(|| f.float(Key::FrameRate).map(f64::from))
        .filter(|x| x.is_finite() && *x > 0.0)
}

/// 读取第一条能解码的视频轨的元数据
pub(crate) fn probe(path: &Path) -> Result<VideoInfo, MediaError> {
    let src = source(path)?;
    let f = &src.format;
    let (w, h) = dims(f).ok_or(MediaError::NoVideo)?;
    let rot = rotation(f);
    let turned = rot % 180 == 90;
    // SAFETY: 提取器有效；返回的新格式（可能为空）由 `Format` 负责释放。
    let file = Format(unsafe { sys::AMediaExtractor_getFileFormat(src.ex) });
    let has_file = !file.0.is_null();
    Ok(VideoInfo {
        duration_ms: f
            .long(Key::Duration)
            .or_else(|| has_file.then(|| file.long(Key::Duration)).flatten())
            .and_then(|us| u64::try_from(us).ok())
            .map(|us| (us + 500) / 1000),
        width: if turned { h } else { w },
        height: if turned { w } else { h },
        rotated: rot != 0,
        codec: Some(src.mime.clone()),
        fps: fps(f),
        bit_rate: f.int(Key::BitRate).and_then(|x| u64::try_from(x).ok()),
        container: has_file.then(|| file.string(Key::Mime)).flatten(),
        created: None,
        camera: None,
        has_gps: false,
        has_audio: src.tracks().iter().any(|(_, _, m)| m.starts_with("audio/")),
    })
}

struct Reader(*mut sys::AImageReader);

impl Reader {
    fn new(width: u32, height: u32) -> Result<Self, String> {
        let (w, h) = match (i32::try_from(width), i32::try_from(height)) {
            (Ok(w), Ok(h)) => (w, h),
            _ => return Err(format!("尺寸过大：{width}×{height}")),
        };
        let fmt = sys::AIMAGE_FORMATS::AIMAGE_FORMAT_YUV_420_888.0 as i32;
        let mut reader = ptr::null_mut();
        // SAFETY: 参数是 API 定义的取值，输出参数指向局部变量。
        let status = unsafe { sys::AImageReader_new(w, h, fmt, MAX_IMAGES, &mut reader) };
        if status != sys::media_status_t::AMEDIA_OK || reader.is_null() {
            return Err(format!("AImageReader_new 返回 {}", status.0));
        }
        Ok(Self(reader))
    }

    /// ImageReader 的 window，归它所有，删除 ImageReader 前有效
    fn window(&self) -> Result<*mut sys::ANativeWindow, String> {
        let mut window = ptr::null_mut();
        // SAFETY: ImageReader 有效，输出参数指向局部变量。
        let status = unsafe { sys::AImageReader_getWindow(self.0, &mut window) };
        if status != sys::media_status_t::AMEDIA_OK || window.is_null() {
            return Err(format!("AImageReader_getWindow 返回 {}", status.0));
        }
        Ok(window)
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        // SAFETY: 由 AImageReader_new 创建的非空 ImageReader，只在这里释放一次；取出的图像借用它，已先删除。
        unsafe { sys::AImageReader_delete(self.0) };
    }
}

/// 从 ImageReader 取出的一张图像，借用它，先于它删除
struct Image<'a>(*mut sys::AImage, PhantomData<&'a Reader>);

impl Drop for Image<'_> {
    fn drop(&mut self) {
        // SAFETY: 由 acquireNextImage 取得的非空图像，只在这里释放一次；ImageReader 还在（借用）。
        unsafe { sys::AImage_delete(self.0) };
    }
}

type ImageGetter = unsafe extern "C" fn(*const sys::AImage, *mut i32) -> sys::media_status_t;

impl Image<'_> {
    fn int(&self, get: ImageGetter) -> Option<i32> {
        let mut v = 0;
        // SAFETY: 图像有效；`get` 是 AImage 的读取函数，输出参数指向局部变量。
        (unsafe { get(self.0, &mut v) } == sys::media_status_t::AMEDIA_OK).then_some(v)
    }

    fn pts_us(&self) -> Option<i64> {
        let mut ns = 0;
        // SAFETY: 图像有效，输出参数指向局部变量。
        let status = unsafe { sys::AImage_getTimestamp(self.0, &mut ns) };
        (status == sys::media_status_t::AMEDIA_OK).then_some(ns / 1000)
    }

    fn plane(&self, i: i32) -> Result<Plane<'_>, String> {
        let (mut data, mut len, mut row, mut pixel) = (ptr::null_mut(), 0, 0, 0);
        // SAFETY: 图像有效，平面下标小于 3（YUV_420_888 有 3 个平面），输出参数指向局部变量。
        let ok = unsafe {
            sys::AImage_getPlaneData(self.0, i, &mut data, &mut len)
                == sys::media_status_t::AMEDIA_OK
                && sys::AImage_getPlaneRowStride(self.0, i, &mut row)
                    == sys::media_status_t::AMEDIA_OK
                && sys::AImage_getPlanePixelStride(self.0, i, &mut pixel)
                    == sys::media_status_t::AMEDIA_OK
        };
        if !ok || data.is_null() || len <= 0 || row <= 0 || pixel <= 0 {
            return Err(format!("读不出第 {i} 个平面"));
        }
        Ok(Plane {
            // SAFETY: 平面数据在图像删除前有效、可读 `len` 字节；切片借用图像，不会活得比它久。
            data: unsafe { std::slice::from_raw_parts(data, len as usize) },
            row: row as usize,
            pixel: pixel as usize,
        })
    }

    /// 按 `m` 转成 RGB，只取裁剪框内的部分（解码器的缓冲区可能按对齐放大）
    fn to_rgb(&self, m: Matrix) -> Result<Rgb, String> {
        let fmt = self.int(sys::AImage_getFormat).ok_or("读不出图像格式")?;
        if fmt != sys::AIMAGE_FORMATS::AIMAGE_FORMAT_YUV_420_888.0 as i32 {
            return Err(format!("图像格式 {fmt:#x} 不是 8 位 YUV 4:2:0"));
        }
        let (w, h) = match (
            self.int(sys::AImage_getWidth),
            self.int(sys::AImage_getHeight),
        ) {
            (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
            (w, h) => return Err(format!("图像尺寸无效：{w:?}×{h:?}")),
        };
        let mut c = sys::AImageCropRect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: 图像有效，输出参数指向局部变量。
        let ok =
            unsafe { sys::AImage_getCropRect(self.0, &mut c) } == sys::media_status_t::AMEDIA_OK;
        let rect = if ok
            && 0 <= c.left
            && c.left < c.right
            && c.right <= w
            && 0 <= c.top
            && c.top < c.bottom
            && c.bottom <= h
        {
            (c.left, c.top, c.right - c.left, c.bottom - c.top)
        } else {
            (0, 0, w, h)
        };
        let rect = (rect.0 as u32, rect.1 as u32, rect.2 as u32, rect.3 as u32);
        let (y, u, v) = (self.plane(0)?, self.plane(1)?, self.plane(2)?);
        if ![u.pixel, v.pixel].iter().all(|p| [1, 2].contains(p)) {
            return Err(format!(
                "色度平面的像素间距为 {}、{}，不是 8 位 YUV 4:2:0",
                u.pixel, v.pixel
            ));
        }
        yuv_to_rgb(y, u, v, rect, m).ok_or_else(|| "平面数据不完整".to_string())
    }
}

/// 打开第一条能解码的视频轨、创建解码器，按编码的宽高建 ImageReader
fn open(path: &Path) -> Result<(Source, Reader, Codec), MediaError> {
    let mut src = source(path)?;
    let codec = choose_decoder(&mut src, path)?;
    let (w, h) = dims(&src.format).ok_or(MediaError::NoVideo)?;
    let reader = Reader::new(w, h).map_err(|err| decode_err(path, VIDEO, err))?;
    Ok((src, reader, codec))
}

/// 运行中的解码器，解出的帧逐个经 ImageReader 取出（`next`）
struct Decoder<'a> {
    /// 向 `reader` 的 window 输出；`reader` 是调用方更早声明的局部变量，在解码器之后删除
    codec: Codec,
    reader: &'a Reader,
    src: Source,
    rotation: i32,
    /// 轨道格式与解码器输出格式中的 `color-standard`、`color-range`
    track_color: (Option<i32>, Option<i32>),
    out_color: (Option<i32>, Option<i32>),
    /// 一帧的时长（微秒），由容器给的帧率算出，读不到时为 0
    frame_us: i64,
    /// pts 早于它的帧不渲染
    skip_before: i64,
    input_done: bool,
    output_done: bool,
    rendered: u64,
    received: u64,
    /// 输出结束或最近一次收到图像的时刻
    last_seen: Instant,
    deadline: Instant,
    timeout: Duration,
}

impl<'a> Decoder<'a> {
    /// 配置并启动解码器；`seek_us` 不为空时先定位到它之前的关键帧，早于它的帧不渲染
    fn start(
        src: Source,
        codec: Codec,
        reader: &'a Reader,
        path: &Path,
        seek_us: Option<i64>,
        timeout: Duration,
    ) -> Result<Self, MediaError> {
        let err = |e: String| decode_err(path, VIDEO, e);
        let f = &src.format;
        let rotation = rotation(f);
        let track_color = (f.int(Key::ColorStandard), f.int(Key::ColorRange));
        let hdr = f
            .int(Key::ColorTransfer)
            .is_some_and(|t| TRANSFER_HDR.contains(&t));
        let frame_us = fps(f).map_or(0, |x| (1e6 / x).round() as i64);
        // SAFETY: 格式有效，归 `src` 所有（轨道格式的副本），键见 `key`。
        unsafe { sys::AMediaFormat_setInt32(f.0, key(Key::Rotation), 0) };
        if track_color.0.is_none() {
            // SAFETY: 同上。
            unsafe { sys::AMediaFormat_setInt32(f.0, key(Key::ColorStandard), STANDARD_BT601) };
        }
        if track_color.1.is_none() {
            // SAFETY: 同上。
            unsafe { sys::AMediaFormat_setInt32(f.0, key(Key::ColorRange), RANGE_LIMITED) };
        }
        // SAFETY: 无参数，只读取系统属性。
        if unsafe { sys::android_get_device_api_level() } >= TRANSFER_REQUEST_API {
            // SAFETY: 格式有效；键是以 NUL 结尾的静态字符串。
            unsafe { sys::AMediaFormat_setInt32(f.0, TRANSFER_REQUEST.as_ptr(), TRANSFER_SDR) };
        }
        let window = reader.window().map_err(err)?;
        if let Some(us) = seek_us {
            let mode = sys::SeekMode::AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC;
            // SAFETY: 提取器有效，已选中轨道。
            let status = unsafe { sys::AMediaExtractor_seekTo(src.ex, us, mode) };
            if status != sys::media_status_t::AMEDIA_OK {
                return Err(err(format!("seekTo 返回 {}", status.0)));
            }
        }
        codec.start(&src.format, window).map_err(err)?;
        // SAFETY: 编解码器已启动；返回的新格式（可能为空）由 `Format` 负责释放。
        let input = Format(unsafe { sys::AMediaCodec_getInputFormat(codec.0) });
        let requested = (!input.0.is_null())
            .then(|| int_by_name(&input, TRANSFER_REQUEST))
            .flatten();
        if hdr && requested != Some(TRANSFER_SDR) {
            return Err(err(format!(
                "解码器不能把 HDR 转为 SDR（color-transfer-request 为 {requested:?}）"
            )));
        }
        let now = Instant::now();
        Ok(Self {
            codec,
            reader,
            src,
            rotation,
            track_color,
            out_color: (None, None),
            frame_us,
            skip_before: seek_us.unwrap_or(i64::MIN),
            input_done: false,
            output_done: false,
            rendered: 0,
            received: 0,
            last_seen: now,
            deadline: now + timeout,
            timeout,
        })
    }

    /// 取出已到达 ImageReader 的下一帧
    fn acquire(&self, path: &Path) -> Result<Option<Image<'a>>, MediaError> {
        let mut img = ptr::null_mut();
        // SAFETY: ImageReader 有效，输出参数指向局部变量。
        let status = unsafe { sys::AImageReader_acquireNextImage(self.reader.0, &mut img) };
        if status == sys::media_status_t::AMEDIA_IMGREADER_NO_BUFFER_AVAILABLE {
            return Ok(None);
        }
        if status != sys::media_status_t::AMEDIA_OK || img.is_null() {
            return Err(decode_err(
                path,
                VIDEO,
                format!("acquireNextImage 返回 {}", status.0),
            ));
        }
        Ok(Some(Image(img, PhantomData)))
    }

    /// 按顺序解出下一帧；视频结束时为空
    fn next(&mut self, path: &Path) -> Result<Option<Image<'a>>, MediaError> {
        loop {
            if let Some(img) = self.acquire(path)? {
                self.received += 1;
                self.last_seen = Instant::now();
                return Ok(Some(img));
            }
            if Instant::now() > self.deadline {
                return Err(MediaError::Timeout {
                    what: VIDEO,
                    secs: self.timeout.as_secs(),
                });
            }
            if self.output_done {
                if self.received >= self.rendered {
                    return Ok(None);
                }
                if self.last_seen.elapsed() > TAIL_WAIT {
                    log::debug!(
                        "“{}”渲染了 {} 帧，只收到 {} 帧",
                        path.display(),
                        self.rendered,
                        self.received
                    );
                    return Ok(None);
                }
                std::thread::sleep(POLL);
                continue;
            }
            if !self.input_done {
                self.input_done = feed(&self.codec, &self.src, path, VIDEO)?;
            }
            let mut info = sys::AMediaCodecBufferInfo {
                offset: 0,
                size: 0,
                presentationTimeUs: 0,
                flags: 0,
            };
            // 还在送入数据时不等输出：输入缓冲区都被占用时 `feed` 已经等过
            let wait = if self.input_done { DEQUEUE_US } else { 0 };
            // SAFETY: 编解码器已启动，输出参数指向局部变量。
            let idx =
                unsafe { sys::AMediaCodec_dequeueOutputBuffer(self.codec.0, &mut info, wait) };
            let Ok(idx) = usize::try_from(idx) else {
                match idx as i32 {
                    sys::AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED => {
                        // SAFETY: 编解码器已启动；返回的新格式由 `Format` 负责释放。
                        let f = Format(unsafe { sys::AMediaCodec_getOutputFormat(self.codec.0) });
                        if !f.0.is_null() {
                            self.out_color = (f.int(Key::ColorStandard), f.int(Key::ColorRange));
                            log::debug!(
                                "“{}”的解码器输出色彩 {:?}，轨道 {:?}",
                                path.display(),
                                self.out_color,
                                self.track_color
                            );
                        }
                    }
                    sys::AMEDIACODEC_INFO_TRY_AGAIN_LATER
                    | sys::AMEDIACODEC_INFO_OUTPUT_BUFFERS_CHANGED => {}
                    err => {
                        return Err(decode_err(
                            path,
                            VIDEO,
                            format!("dequeueOutputBuffer 返回 {err}"),
                        ));
                    }
                }
                continue;
            };
            let render = info.size > 0 && info.presentationTimeUs >= self.skip_before;
            // SAFETY: 归还刚取得的输出缓冲区；`render` 为是时渲染到 ImageReader 的 window。
            unsafe { sys::AMediaCodec_releaseOutputBuffer(self.codec.0, idx, render) };
            if info.flags & sys::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM != 0 {
                self.output_done = true;
                self.last_seen = Instant::now();
            }
            if !render {
                continue;
            }
            self.rendered += 1;
            let since = Instant::now();
            loop {
                if let Some(img) = self.acquire(path)? {
                    self.received += 1;
                    self.last_seen = Instant::now();
                    return Ok(Some(img));
                }
                if since.elapsed() > FRAME_WAIT {
                    log::debug!(
                        "“{}”pts {} 微秒的帧渲染后 {} 毫秒仍未到达",
                        path.display(),
                        info.presentationTimeUs,
                        FRAME_WAIT.as_millis()
                    );
                    break;
                }
                std::thread::sleep(POLL);
            }
        }
    }

    /// 转成 RGB 并转正
    fn rgb(&self, img: &Image, path: &Path) -> Result<Rgb, MediaError> {
        let m = Matrix::android(
            self.out_color.0.or(self.track_color.0),
            self.out_color.1.or(self.track_color.1),
        );
        let rgb = img.to_rgb(m).map_err(|err| decode_err(path, VIDEO, err))?;
        Ok(rotate(rgb, self.rotation))
    }
}

/// 整段视频按 `grid` 抽帧并缩放到 `size`，语义同 `video::extract`
pub(crate) fn extract(
    path: &Path,
    (w, h): (u32, u32),
    grid: Grid,
    max_frames: usize,
    start_s: Option<f64>,
    timeout: Duration,
) -> Result<Vec<Rgb>, MediaError> {
    let start_us = start_s.map(|s| (s * 1e6).round() as i64);
    let (src, reader, codec) = open(path)?;
    let mut dec = Decoder::start(src, codec, &reader, path, start_us, timeout)?;
    let mut picker = Picker::new(grid, dec.frame_us);
    let mut frames: Vec<Rgb> = Vec::new();
    // 把 `img` 缩放后输出 `n` 次；超过上限时不再转换
    let mut emit = |img: &Image, n: u64, dec: &Decoder| -> Result<(), MediaError> {
        if n == 0 {
            return Ok(());
        }
        if frames.len() as u64 + n > max_frames as u64 {
            return Err(MediaError::TooLong);
        }
        let rgb = resize_bicubic(&dec.rgb(img, path)?, w, h)
            .map_err(|err| decode_err(path, VIDEO, err))?;
        frames.extend(std::iter::repeat_n(rgb, n as usize));
        Ok(())
    };
    let mut held: Option<Image> = None;
    while let Some(img) = dec.next(path)? {
        let Some(pts) = img.pts_us() else {
            return Err(decode_err(path, VIDEO, "读不出图像的时间戳"));
        };
        let n = picker.push(pts - start_us.unwrap_or(0));
        emit(held.as_ref().unwrap_or(&img), n, &dec)?;
        held = Some(img);
    }
    if let Some(img) = &held {
        emit(img, picker.finish(), &dec)?;
    }
    if frames.is_empty() {
        return Err(MediaError::NoVideo);
    }
    Ok(frames)
}

/// 取 `ms` 毫秒处的一帧，语义同 `frame_at`
pub(crate) fn frame_at(path: &Path, ms: u64, timeout: Duration) -> Result<Rgb, MediaError> {
    let target = i64::try_from(ms).unwrap_or(i64::MAX).saturating_mul(1000);
    let (src, reader, codec) = open(path)?;
    let mut dec = Decoder::start(src, codec, &reader, path, Some(target), timeout)?;
    while let Some(img) = dec.next(path)? {
        if !img.pts_us().is_some_and(|p| p >= target) {
            continue;
        }
        let rgb = dec.rgb(&img, path)?;
        let tw = rgb.width.min(THUMB_WIDTH);
        // 同 ffmpeg 的 `-2`：按比例四舍五入到偶数
        let th = ((u64::from(tw) * u64::from(rgb.height) + u64::from(rgb.width))
            / (2 * u64::from(rgb.width))
            * 2)
        .max(2) as u32;
        if (tw, th) == (rgb.width, rgb.height) {
            return Ok(rgb);
        }
        return resize_bicubic(&rgb, tw, th).map_err(|err| decode_err(path, VIDEO, err));
    }
    Err(MediaError::NoVideo)
}
