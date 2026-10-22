//! Android 的视频：元数据、整段抽帧与命中时刻的画面，用 `AMediaExtractor` + `AMediaCodec`，解码到缓冲区。
//!
//! - 轨道：MIME 为 `video/` 的轨道（`video/mjpeg` 等封面图类型除外）。元数据取第一条；解码时按顺序取第一条
//!   系统能创建解码器的，都建不了时返回 `MediaError::Decode`（日志写明各轨道的 MIME）。
//! - 解码器输出到缓冲区（不用 surface），请求 `COLOR_FormatYUV420Flexible`，按输出格式的 `image-data`（MediaImage2，
//!   与 Java 的 `MediaCodec.getOutputImage` 相同）读出三个平面的起点、像素间距与行距，裁剪框取 `crop`。这是 CTS 要求
//!   所有解码器都支持的输出方式；输出到 AImageReader 时，高通的解码器即使 ImageReader 只声明了 CPU 读取，仍给出 UBWC
//!   压缩格式（SM8550 实测），CPU 读不出色度平面。帧直接从输出缓冲区读取、不复制，用完才归还（`Frame`），
//!   同时只持有待定的上一帧与刚收到的一帧，不输出的帧不做任何转换。缓冲区输出不做旋转，转成 RGB 后自己转正。
//! - 时间：帧的 pts 与定位的时刻都从视频轨第一个样本的时间算起。剪辑出的片段常保留原视频的时间戳（如从 44 秒开始），
//!   ffmpeg 同样把开头归零，否则开头会补出几十个第一帧、命中画面也会定位到错误的时刻。
//! - 整段抽帧：从头顺序解码，不逐帧定位；按网格取帧、YUV 转 RGB、转正的规则见 `video::raw`，再用 `resize_bicubic` 缩放。
//!   只转换要输出的帧，内存中只留缩放后的帧。`start_s`（只供测试）同 ffmpeg 的 `-ss`：定位到之前的关键帧，
//!   早于它的帧不取出，pts 从它算起。
//! - 色彩：矩阵与范围先取解码器输出格式的 `color-standard` / `color-range`（描述实际拿到的像素，转成 SDR 后也是），
//!   再取轨道格式，都没有时用 BT.601、有限范围（见 `video::raw`）。码流没有标注时 Codec2 按尺寸填默认值
//!   （`ColorUtils::setDefaultCodecColorAspectsIfNeeded`：高清 BT.709、4K BT.2020），与系统播放器显示的颜色一致，
//!   与 ffmpeg 的默认（BT.601）不同；配置中指定的色彩在输出到缓冲区时不起作用。手机与电脑各自建立索引，不要求逐像素一致
//!   （实测 5 段视频与电脑抽出的帧逐帧对齐，像素平均相差约 3%）。
//! - 10 位 HDR：API 31 起配置时请求转为 SDR（`color-transfer-request` 设为 `COLOR_TRANSFER_SDR_VIDEO`，即 3；
//!   键名与取值见 AOSP `MediaFormat.KEY_COLOR_TRANSFER_REQUEST`，NDK 头文件没有这个键，未知的键解码器会忽略）。
//!   启动后从解码器的输入格式读回这一项：不是 3 而轨道的 `color-transfer` 为 ST2084（6）或 HLG（7）时，解码器不会转换，
//!   返回“格式不受支持”（`MediaError::Decode`）。`image-data` 不是 8 位 YUV 4:2:0、或色度平面的像素间距不是 1 或 2
//!   （如 10 位的 P010）时同样返回这个错误。
//! - 命中画面：`seekTo(PREVIOUS_SYNC)` 后解到第一个 pts 不早于目标时刻的帧，与 ffmpeg 把 `-ss` 放在 `-i` 前相同；
//!   转正后缩到宽不超过 `THUMB_WIDTH`，高按比例取偶数（同 ffmpeg 的 `scale=…:-2`）。
//! - 元数据：`codec` 与 `container` 是 MIME 类型，与电脑上 ffprobe 的编码名、格式名不同；录制时间、机型、位置读不到。
//! - 时限只在解码循环的间隙检查。调用方必须先确认不是网盘占位文件。

use std::ffi::{CStr, c_void};
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
/// `MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible`
const YUV420_FLEXIBLE: i32 = 0x7F42_0888;
/// MediaImage2 的类型 `MEDIA_IMAGE_TYPE_YUV`
const MEDIA_IMAGE_YUV: u32 = 1;
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

/// 输出缓冲区中 Y、U、V 三个平面的起点、像素间距与行距（字节），以及有效区域 `(x, y, 宽, 高)`
#[derive(Debug, Clone, Copy, PartialEq)]
struct Layout {
    planes: [(usize, usize, usize); 3],
    rect: (u32, u32, u32, u32),
}

/// 按 MediaImage2（AOSP `media/hardware/VideoAPI.h`）解析 `image-data`：先是 6 个 u32（类型、平面数、宽、高、位深、
/// 分配的位深），再是每个平面的起点、列增量、行增量、水平与垂直下采样（各 4 字节）。只接受 8 位 YUV 4:2:0
fn parse_image_data(raw: &[u8]) -> Result<Layout, String> {
    let word = |i: usize| {
        raw.get(i * 4..i * 4 + 4)
            .map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
    };
    let short = || format!("image-data 只有 {} 字节", raw.len());
    let head: Vec<u32> = (0..6).map(word).collect::<Option<_>>().ok_or_else(short)?;
    let (kind, count, w, h, depth, alloc) = (head[0], head[1], head[2], head[3], head[4], head[5]);
    if kind != MEDIA_IMAGE_YUV || count != 3 || depth != 8 || alloc != 8 || w == 0 || h == 0 {
        return Err(format!(
            "输出不是 8 位 YUV 4:2:0（类型 {kind}，{count} 个平面，位深 {depth}/{alloc}，{w}×{h}）"
        ));
    }
    let mut planes = [(0, 0, 0); 3];
    for (i, p) in planes.iter_mut().enumerate() {
        let f: Vec<u32> = (0..5)
            .map(|k| word(6 + i * 5 + k))
            .collect::<Option<_>>()
            .ok_or_else(short)?;
        let sub = if i == 0 { 1 } else { 2 };
        let (col, row) = (f[1] as i32, f[2] as i32);
        if f[3] != sub || f[4] != sub || row <= 0 || !(1..=2).contains(&col) {
            return Err(format!(
                "第 {i} 个平面不是 8 位 YUV 4:2:0：像素间距 {col}，行距 {row}，下采样 {}×{}",
                f[3], f[4]
            ));
        }
        *p = (f[0] as usize, col as usize, row as usize);
    }
    Ok(Layout {
        planes,
        rect: (0, 0, w, h),
    })
}

/// 输出格式中的平面布局（`parse_image_data`）与裁剪框（`crop` 的右、下边界包含在内，越界时用整个画面）
fn layout(f: &Format) -> Result<Layout, String> {
    let (mut data, mut size): (*mut c_void, usize) = (ptr::null_mut(), 0);
    // SAFETY: 格式有效，键是以 NUL 结尾的静态字符串，输出参数指向局部变量；数据归格式所有，在下面立即解析。
    let found =
        unsafe { sys::AMediaFormat_getBuffer(f.0, c"image-data".as_ptr(), &mut data, &mut size) };
    if !found || data.is_null() {
        return Err("解码器的输出格式中没有 image-data".into());
    }
    // SAFETY: 非空，可读 `size` 字节，格式在解析期间不被修改或释放。
    let mut l = parse_image_data(unsafe { std::slice::from_raw_parts(data as *const u8, size) })?;
    let (w, h) = (l.rect.2 as i32, l.rect.3 as i32);
    let (mut left, mut top, mut right, mut bottom) = (0, 0, 0, 0);
    // SAFETY: 格式有效，键见 `key`，输出参数指向局部变量。
    let crop = unsafe {
        sys::AMediaFormat_getRect(
            f.0,
            key(Key::Crop),
            &mut left,
            &mut top,
            &mut right,
            &mut bottom,
        )
    };
    if crop && 0 <= left && left <= right && right < w && 0 <= top && top <= bottom && bottom < h {
        l.rect = (
            left as u32,
            top as u32,
            (right - left + 1) as u32,
            (bottom - top + 1) as u32,
        );
    }
    Ok(l)
}

/// 解出的一帧：解码器的一个输出缓冲区，删除时归还；借用解码器，先于它删除
struct Frame<'a> {
    codec: &'a Codec,
    idx: usize,
    data: *const u8,
    len: usize,
    /// 从视频开头（`Decoder::origin`）算起
    pts_us: i64,
}

impl Frame<'_> {
    fn data(&self) -> &[u8] {
        // SAFETY: 非空（`Decoder::next` 已检查），可读 `len` 字节（不超过容量），缓冲区归还（`drop`）前有效。
        unsafe { std::slice::from_raw_parts(self.data, self.len) }
    }
}

impl Drop for Frame<'_> {
    fn drop(&mut self) {
        // SAFETY: 下标来自 dequeueOutputBuffer，只在这里归还一次，不渲染；解码器还在（借用）。
        unsafe { sys::AMediaCodec_releaseOutputBuffer(self.codec.0, self.idx, false) };
    }
}

/// 打开第一条能解码的视频轨并创建解码器
fn open(path: &Path) -> Result<(Source, Codec), MediaError> {
    let mut src = source(path)?;
    let codec = choose_decoder(&mut src, path)?;
    Ok((src, codec))
}

/// 运行中的解码器，解出的帧逐个取出（`next`）；`codec` 是调用方更早声明的局部变量，在解码器与帧之后删除
struct Decoder<'a> {
    codec: &'a Codec,
    src: Source,
    rotation: i32,
    /// 轨道格式与解码器输出格式中的 `color-standard`、`color-range`
    track_color: (Option<i32>, Option<i32>),
    out_color: (Option<i32>, Option<i32>),
    /// 输出缓冲区的平面布局，读到输出格式后才有
    layout: Option<Layout>,
    /// 一帧的时长（微秒），由容器给的帧率算出，读不到时为 0
    frame_us: i64,
    /// 视频轨第一个样本的时间（微秒），帧的 pts 从它算起（见模块说明）
    origin: i64,
    /// pts 早于它的帧不取出
    skip_before: i64,
    input_done: bool,
    output_done: bool,
    deadline: Instant,
    timeout: Duration,
}

impl<'a> Decoder<'a> {
    /// 配置并启动解码器；`seek_us`（从视频开头算起）不为空时先定位到它之前的关键帧，早于它的帧不取出
    fn start(
        src: Source,
        codec: &'a Codec,
        path: &Path,
        seek_us: Option<i64>,
        timeout: Duration,
    ) -> Result<Self, MediaError> {
        let err = |e: String| decode_err(path, VIDEO, e);
        // SAFETY: 提取器有效，已选中轨道，还没有读取或定位；没有样本时返回 -1。
        let origin = unsafe { sys::AMediaExtractor_getSampleTime(src.ex) }.max(0);
        let f = &src.format;
        let rotation = rotation(f);
        let track_color = (f.int(Key::ColorStandard), f.int(Key::ColorRange));
        let hdr = f
            .int(Key::ColorTransfer)
            .is_some_and(|t| TRANSFER_HDR.contains(&t));
        let frame_us = fps(f).map_or(0, |x| (1e6 / x).round() as i64);
        // SAFETY: 格式有效，归 `src` 所有（轨道格式的副本），键见 `key`。
        unsafe { sys::AMediaFormat_setInt32(f.0, key(Key::ColorFormat), YUV420_FLEXIBLE) };
        // SAFETY: 无参数，只读取系统属性。
        if unsafe { sys::android_get_device_api_level() } >= TRANSFER_REQUEST_API {
            // SAFETY: 格式有效；键是以 NUL 结尾的静态字符串。
            unsafe { sys::AMediaFormat_setInt32(f.0, TRANSFER_REQUEST.as_ptr(), TRANSFER_SDR) };
        }
        if let Some(us) = seek_us {
            let mode = sys::SeekMode::AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC;
            // SAFETY: 提取器有效，已选中轨道。
            let status = unsafe { sys::AMediaExtractor_seekTo(src.ex, origin + us, mode) };
            if status != sys::media_status_t::AMEDIA_OK {
                return Err(err(format!("seekTo 返回 {}", status.0)));
            }
        }
        codec.start(&src.format, ptr::null_mut()).map_err(err)?;
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
        Ok(Self {
            codec,
            src,
            rotation,
            track_color,
            out_color: (None, None),
            layout: None,
            frame_us,
            origin,
            skip_before: seek_us.map_or(i64::MIN, |us| origin + us),
            input_done: false,
            output_done: false,
            deadline: Instant::now() + timeout,
            timeout,
        })
    }

    /// 读取解码器当前的输出格式：色彩与平面布局
    fn read_format(&mut self, path: &Path) -> Result<(), MediaError> {
        // SAFETY: 编解码器已启动；返回的新格式（可能为空）由 `Format` 负责释放。
        let f = Format(unsafe { sys::AMediaCodec_getOutputFormat(self.codec.0) });
        if f.0.is_null() {
            return Err(decode_err(path, VIDEO, "解码器没有给出输出格式"));
        }
        self.out_color = (f.int(Key::ColorStandard), f.int(Key::ColorRange));
        let l = layout(&f).map_err(|err| decode_err(path, VIDEO, err))?;
        log::debug!(
            "“{}”的解码器输出色彩 {:?}，轨道 {:?}，平面 {l:?}",
            path.display(),
            self.out_color,
            self.track_color
        );
        self.layout = Some(l);
        Ok(())
    }

    /// 按顺序解出下一帧；视频结束时为空
    fn next(&mut self, path: &Path) -> Result<Option<Frame<'a>>, MediaError> {
        loop {
            if Instant::now() > self.deadline {
                return Err(MediaError::Timeout {
                    what: VIDEO,
                    secs: self.timeout.as_secs(),
                });
            }
            if self.output_done {
                return Ok(None);
            }
            if !self.input_done {
                self.input_done = feed(self.codec, &self.src, path, VIDEO)?;
            }
            let mut info = sys::AMediaCodecBufferInfo {
                offset: 0,
                size: 0,
                presentationTimeUs: 0,
                flags: 0,
            };
            // SAFETY: 编解码器已启动，输出参数指向局部变量。
            let idx = unsafe {
                sys::AMediaCodec_dequeueOutputBuffer(self.codec.0, &mut info, DEQUEUE_US)
            };
            let Ok(idx) = usize::try_from(idx) else {
                match idx as i32 {
                    sys::AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED => self.read_format(path)?,
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
            if info.flags & sys::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM != 0 {
                self.output_done = true;
            }
            // NdkMediaCodec.h：API 35 及以下 `info.offset` 无效、应忽略，`getOutputBuffer` 返回的指针已含偏移，
            // 数据长度以 `info.size` 为准
            let len = info.size.max(0) as usize;
            let mut cap = 0;
            // SAFETY: 下标来自 dequeueOutputBuffer；缓冲区归还前有效。
            let data = unsafe { sys::AMediaCodec_getOutputBuffer(self.codec.0, idx, &mut cap) };
            // 由它负责归还缓冲区，包括下面不要的与出错的
            let frame = Frame {
                codec: self.codec,
                idx,
                data,
                len,
                pts_us: info.presentationTimeUs - self.origin,
            };
            if len == 0 || info.presentationTimeUs < self.skip_before {
                continue;
            }
            if data.is_null() || len > cap {
                return Err(decode_err(
                    path,
                    VIDEO,
                    format!("输出缓冲区无效：{len} 字节，容量 {cap}"),
                ));
            }
            if self.layout.is_none() {
                self.read_format(path)?;
            }
            return Ok(Some(frame));
        }
    }

    /// 转成 RGB 并转正
    fn rgb(&self, frame: &Frame, path: &Path) -> Result<Rgb, MediaError> {
        let Some(l) = self.layout else {
            return Err(decode_err(path, VIDEO, "解码器没有给出输出格式"));
        };
        let m = Matrix::android(
            self.out_color.0.or(self.track_color.0),
            self.out_color.1.or(self.track_color.1),
        );
        let [y, u, v] = l.planes.map(|(start, pixel, row)| Plane {
            data: frame.data().get(start..).unwrap_or_default(),
            row,
            pixel,
        });
        let rgb = yuv_to_rgb(y, u, v, l.rect, m)
            .ok_or_else(|| decode_err(path, VIDEO, "平面数据不完整"))?;
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
    let (src, codec) = open(path)?;
    let mut dec = Decoder::start(src, &codec, path, start_us, timeout)?;
    let mut picker = Picker::new(grid, dec.frame_us);
    let mut frames: Vec<Rgb> = Vec::new();
    // 把 `frame` 缩放后输出 `n` 次；超过上限时不再转换
    let mut emit = |frame: &Frame, n: u64, dec: &Decoder| -> Result<(), MediaError> {
        if n == 0 {
            return Ok(());
        }
        if frames.len() as u64 + n > max_frames as u64 {
            return Err(MediaError::TooLong);
        }
        let rgb = resize_bicubic(&dec.rgb(frame, path)?, w, h)
            .map_err(|err| decode_err(path, VIDEO, err))?;
        frames.extend(std::iter::repeat_n(rgb, n as usize));
        Ok(())
    };
    let mut held: Option<Frame> = None;
    while let Some(frame) = dec.next(path)? {
        let n = picker.push(frame.pts_us - start_us.unwrap_or(0));
        emit(held.as_ref().unwrap_or(&frame), n, &dec)?;
        held = Some(frame);
    }
    if let Some(frame) = &held {
        emit(frame, picker.finish(), &dec)?;
    }
    if frames.is_empty() {
        return Err(MediaError::NoVideo);
    }
    Ok(frames)
}

/// 取 `ms` 毫秒处的一帧，语义同 `frame_at`
pub(crate) fn frame_at(path: &Path, ms: u64, timeout: Duration) -> Result<Rgb, MediaError> {
    let target = i64::try_from(ms).unwrap_or(i64::MAX).saturating_mul(1000);
    let (src, codec) = open(path)?;
    let mut dec = Decoder::start(src, &codec, path, Some(target), timeout)?;
    while let Some(frame) = dec.next(path)? {
        if frame.pts_us < target {
            continue;
        }
        let rgb = dec.rgb(&frame, path)?;
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
