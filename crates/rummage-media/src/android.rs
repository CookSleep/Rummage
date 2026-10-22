//! Android 的系统解码：图片与缩略图用 `AImageDecoder`（API 30），音频用 `AMediaExtractor` + `AMediaCodec`；
//! 视频也用后两者，见 `android/video.rs`，与音频共用提取器、格式、解码器与送数据的部分（`Source`、`Format`、`Codec`、`feed`）。
//!
//! - 图片：只解第一帧，按非预乘 RGBA_8888、原尺寸解码，再由 `over_white` 合成白底、调用方缩放，与电脑相同。
//!   不改输出的色彩空间（默认即文件自身的，相当于不做色彩管理，与 PIL 相同）。
//!   API 31 起 AImageDecoder 按 EXIF 方向转正，头部给出的宽高也是转正后的，这里不做 `orient`（头文件没有写明，依据是 AOSP 的实现：
//!   hwui 的 `ImageDecoder` 按编码中的方向变换输出并交换宽高）；API 30 不转正，`image` 能解的格式（JPEG、PNG 等）交给它，HEIF、AVIF 等仍用系统解码。
//!   GIF 返回 `None` 交给 `image`（与 Windows 相同）。像素数在读出头部后、分配像素内存前检查。
//!   每通道 16 位的 PNG 由系统转成 8 位，取整方式可能与电脑的取高字节不同。
//! - 缩略图：用 `setTargetSize` 让解码器直接按缩略图宽度缩小（JPEG 等可以在解码时缩小，比原尺寸解码再缩放快得多），
//!   `setCrop` 只解缩略图保留的顶部；像素数超过上限的不解。缩放时只能输出预乘 alpha，按预乘的公式合成白底。
//! - 音频：只取第一条音轨，请求 MediaCodec 输出 float 的交错 PCM；解码器不理会请求时也接受 8、16、24、32 位整数，
//!   都转成 f32（整数按满量程归一化，与 ffmpeg 相同）。WAV 等未压缩的音轨（`audio/raw`）不请求：系统的原样输出解码器
//!   照搬输入的整数采样，输出格式却报告为请求的 float（Android 16 实测），按它读出的全是乱码。下混与重采样见 `audio::resample`，输出采样率中途变化时换一个重采样器。
//!   元数据的 `container` 是文件的 MIME 类型（如 `audio/mpeg`），与电脑上 ffprobe 的格式名不同，不做映射。
//!   `subfile` 地址（文档中不压缩存放的音频）用 `setDataSourceFd` 的偏移与长度直接读。
//!   时限只在解码循环的间隙检查，读取本身卡住时无法中途结束（ffmpeg 子进程可以）。
//! - 调用方必须先确认不是网盘占位文件。

pub(crate) mod video;

use std::ffi::{CStr, CString, c_char};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::ptr;
use std::time::{Duration, Instant};

use ndk_sys as sys;

use crate::audio::resample::{Resampler, coefficients, downmix};
use crate::audio::{AudioInfo, SAMPLE_RATE};
use crate::ffmpeg::{self, MediaError};
use crate::{Error, Rgb, over_white};

/// 等待编解码器缓冲区的时长（微秒），也是检查时限的间隔
const DEQUEUE_US: i64 = 10_000;
/// `KEY_PCM_ENCODING` 的取值（`android.media.AudioFormat`），没有这一项时为 16 位
const PCM_16BIT: i32 = 2;
const PCM_8BIT: i32 = 3;
const PCM_FLOAT: i32 = 4;
const PCM_24BIT_PACKED: i32 = 21;
const PCM_32BIT: i32 = 22;
/// 未压缩的 PCM 音轨
const RAW_MIME: &str = "audio/raw";
/// 不接受的输出格式：声道数与采样率的上限
const MAX_CHANNELS: i32 = 8;
const MAX_RATE: i32 = 768_000;
/// AImageDecoder 从这一级 API 起按 EXIF 方向转正
const ORIENTED_API: i32 = 31;
const AUDIO: &str = "音频";

/// API 30 的 AImageDecoder 不按 EXIF 方向转正：`image` 能解的格式交给它；HEIF、AVIF 等它解不了的仍用系统解码，
/// 方向可能不正，但比解不出来好
fn leave_to_image(path: &Path) -> bool {
    // SAFETY: 无参数，只读取系统属性。
    let api = unsafe { sys::android_get_device_api_level() };
    api < ORIENTED_API
        && path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            [
                "jpg", "jpeg", "jpe", "jfif", "png", "webp", "bmp", "tif", "tiff", "gif",
            ]
            .contains(&e.to_ascii_lowercase().as_str())
        })
}

/// `drop` 中先删除解码器，之后字段中的文件才关闭
struct ImageSource {
    dec: *mut sys::AImageDecoder,
    _file: File,
}

impl Drop for ImageSource {
    fn drop(&mut self) {
        // SAFETY: `dec` 由 AImageDecoder_createFromFd 成功创建，只在这里释放一次。
        unsafe { sys::AImageDecoder_delete(self.dec) }
    }
}

/// `AImageDecoder_resultToString` 要 API 31，最低支持的 API 30 上没有，这里自己给出名称
fn image_result(code: i32) -> String {
    let name = match code {
        sys::ANDROID_IMAGE_DECODER_INCOMPLETE => "INCOMPLETE",
        sys::ANDROID_IMAGE_DECODER_ERROR => "ERROR",
        sys::ANDROID_IMAGE_DECODER_INVALID_CONVERSION => "INVALID_CONVERSION",
        sys::ANDROID_IMAGE_DECODER_INVALID_SCALE => "INVALID_SCALE",
        sys::ANDROID_IMAGE_DECODER_BAD_PARAMETER => "BAD_PARAMETER",
        sys::ANDROID_IMAGE_DECODER_INVALID_INPUT => "INVALID_INPUT",
        sys::ANDROID_IMAGE_DECODER_SEEK_ERROR => "SEEK_ERROR",
        sys::ANDROID_IMAGE_DECODER_INTERNAL_ERROR => "INTERNAL_ERROR",
        sys::ANDROID_IMAGE_DECODER_UNSUPPORTED_FORMAT => "UNSUPPORTED_FORMAT",
        _ => return format!("错误码 {code}"),
    };
    format!("{name}（错误码 {code}）")
}

fn check_image(code: i32) -> Result<(), String> {
    if code == sys::ANDROID_IMAGE_DECODER_SUCCESS {
        return Ok(());
    }
    Err(image_result(code))
}

impl ImageSource {
    fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|err| err.to_string())?;
        let mut dec = ptr::null_mut();
        // SAFETY: fd 在 `ImageSource` 存在期间有效（文件在解码器之后关闭）；输出参数指向局部变量。
        let code = unsafe { sys::AImageDecoder_createFromFd(file.as_raw_fd(), &mut dec) };
        check_image(code)?;
        if dec.is_null() {
            return Err("没有创建解码器".into());
        }
        Ok(Self { dec, _file: file })
    }

    /// `(宽, 高, 是否 GIF)`，宽高是转正后的
    fn header(&self) -> Result<(u32, u32, bool), String> {
        // SAFETY: 解码器有效；返回的头部信息归解码器所有，只在本函数内使用。
        let info = unsafe { sys::AImageDecoder_getHeaderInfo(self.dec) };
        // SAFETY: 同上，`info` 由有效的解码器给出，非空（头文件标注为 _Nonnull）。
        let (w, h, mime) = unsafe {
            (
                sys::AImageDecoderHeaderInfo_getWidth(info),
                sys::AImageDecoderHeaderInfo_getHeight(info),
                sys::AImageDecoderHeaderInfo_getMimeType(info),
            )
        };
        let gif = !mime.is_null()
            // SAFETY: 非空时是头部信息中以 NUL 结尾的字符串，解码器存在期间有效。
            && unsafe { CStr::from_ptr(mime) }.to_bytes() == b"image/gif";
        match (u32::try_from(w), u32::try_from(h)) {
            (Ok(w), Ok(h)) if w > 0 && h > 0 => Ok((w, h, gif)),
            _ => Err(format!("尺寸无效：{w}×{h}")),
        }
    }

    /// 按已设置的格式与尺寸解出 RGBA，去掉行尾的对齐
    fn pixels(&self, width: u32, height: u32, unpremultiplied: bool) -> Result<Vec<u8>, String> {
        let fmt = sys::AndroidBitmapFormat::ANDROID_BITMAP_FORMAT_RGBA_8888.0 as i32;
        // SAFETY: 解码器有效，参数是 API 定义的取值。
        check_image(unsafe { sys::AImageDecoder_setAndroidBitmapFormat(self.dec, fmt) })?;
        if unpremultiplied {
            // SAFETY: 同上。
            check_image(unsafe { sys::AImageDecoder_setUnpremultipliedRequired(self.dec, true) })?;
        }
        // SAFETY: 解码器有效。
        let stride = unsafe { sys::AImageDecoder_getMinimumStride(self.dec) };
        let row = width as usize * 4;
        if stride < row {
            return Err(format!("行宽 {stride} 小于 {row}"));
        }
        let size = stride * height as usize;
        let mut buf = vec![0u8; size];
        // SAFETY: 缓冲区可写 `size` 字节，不小于 stride × 高；stride 不小于最小行宽且是像素大小的倍数（取自解码器）。
        let code = unsafe {
            sys::AImageDecoder_decodeImage(self.dec, buf.as_mut_ptr().cast(), stride, size)
        };
        check_image(code)?;
        if stride == row {
            return Ok(buf);
        }
        Ok(buf
            .chunks_exact(stride)
            .flat_map(|r| &r[..row])
            .copied()
            .collect())
    }
}

/// 解码第一帧并合成白底；GIF 返回 `None`。除 `TooLarge` 外的错误只写调试日志，调用方改用 `image`
pub(crate) fn decode(path: &Path, max_pixels: u64) -> Result<Option<Rgb>, Error> {
    if leave_to_image(path) {
        return Ok(None);
    }
    let fail = |err: String| {
        log::debug!("AImageDecoder 无法解码“{}”：{err}", path.display());
        Error::Decode {
            path: path.to_path_buf(),
        }
    };
    let src = ImageSource::open(path).map_err(fail)?;
    let (w, h, gif) = src.header().map_err(fail)?;
    if gif {
        return Ok(None);
    }
    if u64::from(w) * u64::from(h) > max_pixels {
        return Err(Error::TooLarge {
            path: path.to_path_buf(),
        });
    }
    let rgba = src.pixels(w, h, true).map_err(fail)?;
    Ok(Some(over_white(w, h, &rgba)))
}

/// 解码时直接缩到不超过 `max_width` 宽（不放大），只解高不超过宽 × `max_ratio` 的顶部，预乘 alpha 合成白底。
/// 原图像素数超过 `max_pixels` 时不解
pub(crate) fn thumbnail(
    path: &Path,
    max_width: u32,
    max_ratio: u32,
    max_pixels: u64,
) -> Result<Rgb, String> {
    if leave_to_image(path) {
        return Err("API 30 的 AImageDecoder 不按 EXIF 方向转正".into());
    }
    let src = ImageSource::open(path)?;
    let (w, h, _) = src.header()?;
    if u64::from(w) * u64::from(h) > max_pixels {
        return Err(format!("像素数超过上限：{w}×{h}"));
    }
    let (tw, th) = if w > max_width {
        let th = (u64::from(h) * u64::from(max_width) / u64::from(w)).max(1) as u32;
        (max_width, th)
    } else {
        (w, h)
    };
    if (tw, th) != (w, h) {
        // SAFETY: 解码器有效；宽高为正且在 i32 范围内（不大于原图）。
        check_image(unsafe { sys::AImageDecoder_setTargetSize(src.dec, tw as i32, th as i32) })?;
    }
    let ch = th.min(tw.saturating_mul(max_ratio));
    if ch < th {
        let crop = sys::ARect {
            left: 0,
            top: 0,
            right: tw as i32,
            bottom: ch as i32,
        };
        // SAFETY: 解码器有效；裁剪框在缩放后的尺寸以内。
        check_image(unsafe { sys::AImageDecoder_setCrop(src.dec, crop) })?;
    }
    let rgba = src.pixels(tw, ch, false)?;
    let data = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| {
            let bg = 255 - p[3];
            [
                p[0].saturating_add(bg),
                p[1].saturating_add(bg),
                p[2].saturating_add(bg),
            ]
        })
        .collect();
    Ok(Rgb {
        width: tw,
        height: ch,
        data,
    })
}

#[derive(Clone, Copy)]
enum Key {
    Mime,
    Duration,
    SampleRate,
    Channels,
    BitRate,
    PcmEncoding,
    ChannelMask,
    Width,
    Height,
    Rotation,
    FrameRate,
    ColorStandard,
    ColorRange,
    ColorTransfer,
    ColorFormat,
    Crop,
}

fn key(k: Key) -> *const c_char {
    // SAFETY: libmediandk 导出的以 NUL 结尾的常量字符串指针，加载后不再改变，这里只读取指针的值。
    unsafe {
        match k {
            Key::Mime => sys::AMEDIAFORMAT_KEY_MIME,
            Key::Duration => sys::AMEDIAFORMAT_KEY_DURATION,
            Key::SampleRate => sys::AMEDIAFORMAT_KEY_SAMPLE_RATE,
            Key::Channels => sys::AMEDIAFORMAT_KEY_CHANNEL_COUNT,
            Key::BitRate => sys::AMEDIAFORMAT_KEY_BIT_RATE,
            Key::PcmEncoding => sys::AMEDIAFORMAT_KEY_PCM_ENCODING,
            Key::ChannelMask => sys::AMEDIAFORMAT_KEY_CHANNEL_MASK,
            Key::Width => sys::AMEDIAFORMAT_KEY_WIDTH,
            Key::Height => sys::AMEDIAFORMAT_KEY_HEIGHT,
            Key::Rotation => sys::AMEDIAFORMAT_KEY_ROTATION,
            Key::FrameRate => sys::AMEDIAFORMAT_KEY_FRAME_RATE,
            Key::ColorStandard => sys::AMEDIAFORMAT_KEY_COLOR_STANDARD,
            Key::ColorRange => sys::AMEDIAFORMAT_KEY_COLOR_RANGE,
            Key::ColorTransfer => sys::AMEDIAFORMAT_KEY_COLOR_TRANSFER,
            Key::ColorFormat => sys::AMEDIAFORMAT_KEY_COLOR_FORMAT,
            Key::Crop => sys::AMEDIAFORMAT_KEY_DISPLAY_CROP,
        }
    }
}

/// 空指针表示没有格式，此时不调用任何方法、也不释放
struct Format(*mut sys::AMediaFormat);

impl Drop for Format {
    fn drop(&mut self) {
        if self.0.is_null() {
            return;
        }
        // SAFETY: 由 getTrackFormat / getOutputFormat / getFileFormat 返回的格式，调用方负责释放，只释放一次。
        unsafe { sys::AMediaFormat_delete(self.0) };
    }
}

impl Format {
    fn int(&self, k: Key) -> Option<i32> {
        let mut v = 0;
        // SAFETY: 只对非空的格式调用；键见 `key`；输出参数指向局部变量。
        unsafe { sys::AMediaFormat_getInt32(self.0, key(k), &mut v) }.then_some(v)
    }

    /// 有的提取器把帧率写成浮点数
    fn float(&self, k: Key) -> Option<f32> {
        let mut v = 0.0;
        // SAFETY: 同 `int`。
        unsafe { sys::AMediaFormat_getFloat(self.0, key(k), &mut v) }.then_some(v)
    }

    fn long(&self, k: Key) -> Option<i64> {
        let mut v = 0;
        // SAFETY: 同 `int`。
        unsafe { sys::AMediaFormat_getInt64(self.0, key(k), &mut v) }.then_some(v)
    }

    fn string(&self, k: Key) -> Option<String> {
        let mut p = ptr::null();
        // SAFETY: 同 `int`；返回的字符串归格式所有，在下面立即复制。
        if !unsafe { sys::AMediaFormat_getString(self.0, key(k), &mut p) } || p.is_null() {
            return None;
        }
        // SAFETY: 非空，以 NUL 结尾，格式未被修改或释放。
        Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }
}

/// `drop` 中先删除提取器，之后字段中的文件才关闭
struct Source {
    ex: *mut sys::AMediaExtractor,
    file: File,
    track: usize,
    format: Format,
    mime: String,
}

impl Drop for Source {
    fn drop(&mut self) {
        // SAFETY: 由 AMediaExtractor_new 创建的非空提取器，只在这里释放一次。
        unsafe { sys::AMediaExtractor_delete(self.ex) };
    }
}

/// `what` 为“音频”或“视频”
fn decode_err(path: &Path, what: &'static str, err: impl std::fmt::Display) -> MediaError {
    log::warn!("无法读取“{}”的{what}：{err}", path.display());
    MediaError::Decode { what }
}

impl Source {
    /// 打开文件并找到第一条 MIME 类型满足 `want` 的轨道，没有时返回 `none`；`subfile` 地址只读其中的字节范围
    fn open(
        path: &Path,
        what: &'static str,
        want: &dyn Fn(&str) -> bool,
        none: MediaError,
    ) -> Result<Self, MediaError> {
        let (file, offset, len) = match ffmpeg::parse_subfile(path) {
            Some((p, start, end)) => (File::open(p), start, Some(end.saturating_sub(start))),
            None => (File::open(path), 0, None),
        };
        let file = file.map_err(|err| decode_err(path, what, err))?;
        let len = match len {
            Some(n) => n,
            None => file
                .metadata()
                .map_err(|err| decode_err(path, what, err))?
                .len(),
        };
        // SAFETY: 无参数，失败时返回空指针。
        let ex = unsafe { sys::AMediaExtractor_new() };
        if ex.is_null() {
            return Err(decode_err(path, what, "无法创建 AMediaExtractor"));
        }
        let mut src = Self {
            ex,
            file,
            track: 0,
            format: Format(ptr::null_mut()),
            mime: String::new(),
        };
        let fd = src.file.as_raw_fd();
        let offset = i64::try_from(offset).unwrap_or(i64::MAX);
        let len = i64::try_from(len).unwrap_or(i64::MAX);
        // SAFETY: 提取器有效；fd 在 `Source` 存在期间有效（文件在提取器之后关闭）。
        let status = unsafe { sys::AMediaExtractor_setDataSourceFd(ex, fd, offset, len) };
        if status != sys::media_status_t::AMEDIA_OK {
            return Err(decode_err(
                path,
                what,
                format!("setDataSourceFd 返回 {}", status.0),
            ));
        }
        for (i, f, mime) in src.tracks() {
            if want(&mime) {
                src.track = i;
                src.format = f;
                src.mime = mime;
                return Ok(src);
            }
        }
        Err(none)
    }

    /// 各轨道的下标、格式与 MIME 类型；读不出格式或 MIME 的轨道跳过
    fn tracks(&self) -> Vec<(usize, Format, String)> {
        // SAFETY: 提取器有效且已设置数据源。
        let count = unsafe { sys::AMediaExtractor_getTrackCount(self.ex) };
        (0..count)
            .filter_map(|i| {
                // SAFETY: 下标在轨道数以内；返回的新格式由 `Format` 负责释放。
                let f = Format(unsafe { sys::AMediaExtractor_getTrackFormat(self.ex, i) });
                if f.0.is_null() {
                    return None;
                }
                let mime = f.string(Key::Mime)?;
                Some((i, f, mime))
            })
            .collect()
    }

    /// 选中 `open` 找到的轨道，按它的 MIME 类型创建解码器
    fn decoder(&self, path: &Path, what: &'static str) -> Result<Codec, MediaError> {
        // SAFETY: 提取器有效，轨道下标来自 `open`。
        if unsafe { sys::AMediaExtractor_selectTrack(self.ex, self.track) }
            != sys::media_status_t::AMEDIA_OK
        {
            return Err(decode_err(path, what, "无法选中轨道"));
        }
        Codec::new(&self.mime)
            .ok_or_else(|| decode_err(path, what, format!("系统没有 {} 的解码器", self.mime)))
    }
}

fn audio_source(path: &Path) -> Result<Source, MediaError> {
    Source::open(
        path,
        AUDIO,
        &|m| m.starts_with("audio/"),
        MediaError::NoAudio,
    )
}

/// 编码的 MIME 类型换成 ffprobe 的编码名，其余去掉 `audio/` 前缀
fn codec_name(mime: &str) -> String {
    match mime {
        "audio/mp4a-latm" => "aac",
        "audio/mpeg" => "mp3",
        "audio/raw" => "pcm",
        "audio/3gpp" => "amr_nb",
        "audio/amr-wb" => "amr_wb",
        m => m.strip_prefix("audio/").unwrap_or(m),
    }
    .to_string()
}

/// 读取第一条音轨的元数据；录制时间与位置读不到，为空
pub(crate) fn audio_probe(path: &Path) -> Result<AudioInfo, MediaError> {
    let src = audio_source(path)?;
    // SAFETY: 提取器有效；返回的新格式（可能为空）由 `Format` 负责释放。
    let file = Format(unsafe { sys::AMediaExtractor_getFileFormat(src.ex) });
    let has_file = !file.0.is_null();
    let f = &src.format;
    Ok(AudioInfo {
        duration_ms: f
            .long(Key::Duration)
            .or_else(|| has_file.then(|| file.long(Key::Duration)).flatten())
            .and_then(|us| u64::try_from(us).ok())
            .map(|us| (us + 500) / 1000),
        codec: Some(codec_name(&src.mime)),
        sample_rate: f.int(Key::SampleRate).and_then(|x| u32::try_from(x).ok()),
        channels: f.int(Key::Channels).and_then(|x| u32::try_from(x).ok()),
        bit_rate: f.int(Key::BitRate).and_then(|x| u64::try_from(x).ok()),
        container: has_file.then(|| file.string(Key::Mime)).flatten(),
        created: None,
        has_gps: false,
    })
}

struct Codec(*mut sys::AMediaCodec);

impl Codec {
    /// 系统没有该 MIME 类型的解码器时为空
    fn new(mime: &str) -> Option<Self> {
        let mime = CString::new(mime).ok()?;
        // SAFETY: 以 NUL 结尾的字符串；系统没有该编码的解码器时返回空指针。
        let codec = unsafe { sys::AMediaCodec_createDecoderByType(mime.as_ptr()) };
        (!codec.is_null()).then_some(Self(codec))
    }

    /// 按 `format` 配置并启动；`surface` 为空时输出到缓冲区
    fn start(&self, format: &Format, surface: *mut sys::ANativeWindow) -> Result<(), String> {
        // SAFETY: 编解码器与格式有效；surface 为空或在编解码器删除前一直有效（调用方保证）；不加密。
        let status =
            unsafe { sys::AMediaCodec_configure(self.0, format.0, surface, ptr::null_mut(), 0) };
        if status != sys::media_status_t::AMEDIA_OK {
            return Err(format!("configure 返回 {}", status.0));
        }
        // SAFETY: 编解码器已配置。
        if unsafe { sys::AMediaCodec_start(self.0) } != sys::media_status_t::AMEDIA_OK {
            return Err("无法启动解码器".into());
        }
        Ok(())
    }
}

impl Drop for Codec {
    fn drop(&mut self) {
        // SAFETY: 由 createDecoderByType 创建的非空编解码器，只在这里释放一次；正在运行时 delete 会先停止它。
        unsafe { sys::AMediaCodec_delete(self.0) };
    }
}

/// 解码器输出的 PCM 格式
struct Pcm {
    rate: u32,
    /// `KEY_PCM_ENCODING` 的取值
    encoding: i32,
    /// 每声道的下混系数，个数即声道数
    coef: Vec<f32>,
}

impl Pcm {
    /// 读取并检查输出格式；不写日志，调用方决定是否算错误
    fn read(f: &Format) -> Result<Self, String> {
        let encoding = f.int(Key::PcmEncoding).unwrap_or(PCM_16BIT);
        if ![PCM_8BIT, PCM_16BIT, PCM_FLOAT, PCM_24BIT_PACKED, PCM_32BIT].contains(&encoding) {
            return Err(format!("不支持的 PCM 编码 {encoding}"));
        }
        match (f.int(Key::SampleRate), f.int(Key::Channels)) {
            (Some(rate), Some(ch))
                if (1..=MAX_RATE).contains(&rate) && (1..=MAX_CHANNELS).contains(&ch) =>
            {
                Ok(Self {
                    rate: rate as u32,
                    encoding,
                    coef: coefficients(f.int(Key::ChannelMask), ch as usize),
                })
            }
            (rate, ch) => Err(format!("输出格式无效：{rate:?} Hz，{ch:?} 声道")),
        }
    }

    /// 交错的 PCM 字节转成 f32，追加到 `out`；整数按满量程归一化（与 ffmpeg 转 f32 相同，16 位即 x / 32768）
    fn to_f32(&self, bytes: &[u8], out: &mut Vec<f32>) {
        match self.encoding {
            PCM_FLOAT => out.extend(
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| f32::from_le_bytes(*c)),
            ),
            PCM_8BIT => out.extend(bytes.iter().map(|&b| (f32::from(b) - 128.0) / 128.0)),
            PCM_24BIT_PACKED => {
                out.extend(
                    bytes.as_chunks::<3>().0.iter().map(|c| {
                        i32::from_le_bytes([0, c[0], c[1], c[2]]) as f32 / 2_147_483_648.0
                    }),
                )
            }
            PCM_32BIT => out.extend(
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| i32::from_le_bytes(*c) as f32 / 2_147_483_648.0),
            ),
            _ => out.extend(
                bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| f32::from(i16::from_le_bytes(*c)) / 32768.0),
            ),
        }
    }
}

/// 把数据送进解码器所有空闲的输入缓冲区，不等待；没有更多数据时送结束标志，返回 `true`。
/// 等待放在取输出时：解码器要等输出被取走才腾出输入缓冲区，在这里等会一直等到超时（SM8550 上
/// 一段 150 秒的视频因此多花约 18 秒）
fn feed(codec: &Codec, src: &Source, path: &Path, what: &'static str) -> Result<bool, MediaError> {
    loop {
        // SAFETY: 编解码器已启动。
        let idx = unsafe { sys::AMediaCodec_dequeueInputBuffer(codec.0, 0) };
        let Ok(idx) = usize::try_from(idx) else {
            return Ok(false);
        };
        let mut cap = 0;
        // SAFETY: 下标来自 dequeueInputBuffer；返回的缓冲区可写 `cap` 字节，归还前有效。
        let buf = unsafe { sys::AMediaCodec_getInputBuffer(codec.0, idx, &mut cap) };
        if buf.is_null() {
            return Err(decode_err(path, what, "getInputBuffer 返回空指针"));
        }
        // SAFETY: 提取器有效、已选中轨道；缓冲区可写 `cap` 字节，没有更多数据时返回负数。
        let n = unsafe { sys::AMediaExtractor_readSampleData(src.ex, buf, cap) };
        let (n, time, flags, done) = match usize::try_from(n) {
            Ok(n) => {
                // SAFETY: 提取器有效。
                let t = unsafe { sys::AMediaExtractor_getSampleTime(src.ex) };
                // SAFETY: 同上。
                unsafe { sys::AMediaExtractor_advance(src.ex) };
                (n, t.max(0) as u64, 0, false)
            }
            Err(_) => (0, 0, sys::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM, true),
        };
        // SAFETY: 归还刚取得的输入缓冲区，长度不超过读出的字节数。
        let status = unsafe { sys::AMediaCodec_queueInputBuffer(codec.0, idx, 0, n, time, flags) };
        if status != sys::media_status_t::AMEDIA_OK {
            return Err(decode_err(
                path,
                what,
                format!("queueInputBuffer 返回 {}", status.0),
            ));
        }
        if done {
            return Ok(true);
        }
    }
}

/// 解码为 16 kHz 单声道 f32 采样，语义同 `audio::decode`
pub(crate) fn audio_decode(
    path: &Path,
    expect: usize,
    max: usize,
    timeout: Duration,
) -> Result<Vec<f32>, MediaError> {
    let deadline = Instant::now() + timeout;
    let src = audio_source(path)?;
    let codec = src.decoder(path, AUDIO)?;
    if src.mime != RAW_MIME {
        // SAFETY: 格式有效，归 `src` 所有（音轨格式的副本），键见 `key`。
        unsafe { sys::AMediaFormat_setInt32(src.format.0, key(Key::PcmEncoding), PCM_FLOAT) };
    }
    // 音频不输出到 surface
    codec
        .start(&src.format, ptr::null_mut())
        .map_err(|err| decode_err(path, AUDIO, err))?;
    // 输出格式通常在第一个输出缓冲区之前由 OUTPUT_FORMAT_CHANGED 给出，这里先按音轨格式估计
    let mut pcm = Pcm::read(&src.format).ok();
    let mut out: Vec<f32> = Vec::with_capacity(expect.min(max));
    let mut rs: Option<Resampler> = None;
    let (mut frames, mut mono) = (Vec::new(), Vec::new());
    let mut input_done = false;
    loop {
        if Instant::now() > deadline {
            return Err(MediaError::Timeout {
                what: AUDIO,
                secs: timeout.as_secs(),
            });
        }
        if !input_done {
            input_done = feed(&codec, &src, path, AUDIO)?;
        }
        let mut info = sys::AMediaCodecBufferInfo {
            offset: 0,
            size: 0,
            presentationTimeUs: 0,
            flags: 0,
        };
        // SAFETY: 编解码器已启动，输出参数指向局部变量。
        let idx = unsafe { sys::AMediaCodec_dequeueOutputBuffer(codec.0, &mut info, DEQUEUE_US) };
        let Ok(idx) = usize::try_from(idx) else {
            match idx as i32 {
                sys::AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED => {
                    // SAFETY: 编解码器已启动；返回的新格式由 `Format` 负责释放。
                    let f = Format(unsafe { sys::AMediaCodec_getOutputFormat(codec.0) });
                    if !f.0.is_null() {
                        let p = Pcm::read(&f).map_err(|err| decode_err(path, AUDIO, err))?;
                        // 采样率变了：旧的重采样器收尾，之后按新采样率重建
                        if let Some(mut r) = rs.take_if(|r| r.in_rate() != p.rate) {
                            r.finish(&mut out);
                        }
                        pcm = Some(p);
                    }
                }
                sys::AMEDIACODEC_INFO_TRY_AGAIN_LATER
                | sys::AMEDIACODEC_INFO_OUTPUT_BUFFERS_CHANGED => {}
                err => {
                    return Err(decode_err(
                        path,
                        AUDIO,
                        format!("dequeueOutputBuffer 返回 {err}"),
                    ));
                }
            }
            continue;
        };
        let eos = info.flags & sys::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM != 0;
        let mut size = 0;
        // SAFETY: 下标来自 dequeueOutputBuffer；缓冲区归还前有效。
        let buf = unsafe { sys::AMediaCodec_getOutputBuffer(codec.0, idx, &mut size) };
        // NdkMediaCodec.h：API 35 及以下 `info.offset` 无效、应忽略，`getOutputBuffer` 返回的指针已含偏移，
        // 数据长度以 `info.size` 为准；`size` 是缓冲区容量
        let len = info.size.max(0) as usize;
        let res = match &pcm {
            _ if len == 0 => Ok(()),
            _ if buf.is_null() || len > size => Err(decode_err(
                path,
                AUDIO,
                format!("输出缓冲区无效：{len} 字节，容量 {size}"),
            )),
            None => Err(decode_err(path, AUDIO, "解码器没有给出输出格式")),
            Some(p) => {
                // SAFETY: `[0, len)` 在缓冲区的 `size` 字节以内，归还前不会被改写。
                let bytes = unsafe { std::slice::from_raw_parts(buf, len) };
                frames.clear();
                p.to_f32(bytes, &mut frames);
                mono.clear();
                downmix(&frames, &p.coef, &mut mono);
                rs.get_or_insert_with(|| Resampler::new(p.rate, SAMPLE_RATE))
                    .push(&mono, &mut out);
                Ok(())
            }
        };
        // SAFETY: 归还刚取得的输出缓冲区，不渲染。
        unsafe { sys::AMediaCodec_releaseOutputBuffer(codec.0, idx, false) };
        res?;
        if out.len() > max {
            return Err(MediaError::TooLong);
        }
        if eos {
            break;
        }
    }
    if let Some(r) = &mut rs {
        r.finish(&mut out);
    }
    if out.len() > max {
        return Err(MediaError::TooLong);
    }
    Ok(out)
}
