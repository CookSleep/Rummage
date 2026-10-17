//! 图像的解码与缩放，产出交给 mtmd 的 RGB 像素。
//!
//! 目标是与生成黄金样本的 PIL 流程（`scripts/golden/llama_embed.py` 的 `to_rgb`、`target_size` 与 `dump_golden_inputs.py`）
//! 逐像素一致，解码器本身的差异（主要是 JPEG 的 IDCT 与色度上采样）除外：
//! - 解码：Windows 先用 WIC（`windows.rs`），Android 先用 `AImageDecoder`（`android.rs`），失败再用 `image` crate；
//!   GIF 和其他平台只用 `image`。只取第一帧，
//!   带透明通道的图按 `Image.alpha_composite(白底, 图).convert("RGB")` 的整数算法合成到白底，不做色彩管理。
//! - 像素数超过调用方给的上限（参数表 `max_image_mp`）时，在分配像素内存之前报错。
//! - 文档的内嵌图片在内存中，用 `decode_bytes` 解码（只走 `image`），近乎纯色的由 `is_flat` 判断后跳过。
//! - 每通道 16 位的图两条路径都取高字节（`>> 8`）转 8 位，与 PIL 处理 16 位 RGB PNG 相同；
//!   PIL 对 16 位灰度图是把大于 255 的值截成 255，这里不照搬。
//! - EXIF 方向：transformers 的 `load_image` 会做 `exif_transpose`，这里同样转正（上述脚本没有做，黄金样本没有 EXIF，不受影响）。
//!   Android 上 API 31 起 AImageDecoder 按 EXIF 方向转正，API 30 上 `image` 能解的格式交给它。
//! - 尺寸：`target_size` 移植 transformers Gemma 4 的 `get_aspect_ratio_preserving_size`，边长为 48 的倍数。
//! - 缩放：`resize_bicubic` 移植 Pillow 的双三次缩放（`resample.rs`），两端共用。
//! - 缩略图见 `thumb.rs`；长图切片的几何见 `slice.rs`；音频的解码与切段见 `audio`；
//!   视频的窗口、抽帧与组装见 `video`；两者调用 ffmpeg 的方式见 `ffmpeg`（Android 的音视频改用系统解码器，见 `android.rs`）。
//!   结果预览中视频命中时刻的画面见 `frame`。

#[cfg(target_os = "android")]
mod android;
pub mod audio;
pub mod ffmpeg;
mod frame;
mod resample;
mod slice;
mod thumb;
pub mod video;
#[cfg(windows)]
mod windows;

use std::path::{Path, PathBuf};

use image::ImageDecoder;

pub use frame::frame_at;
use resample::resize_bicubic;
pub use slice::{Region, crop, slices};
use thumb::THUMB_WIDTH;
pub use thumb::{shrink_webp, thumbnail};

/// 8 位 RGB 图像，按行存放，每像素 3 字节，`data.len() == width * height * 3`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 系统错误只写日志，下同
    #[error("无法读取图片 \"{}\"，请确认它存在且可以打开。", .path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("无法解码图片 \"{}\"，文件可能已损坏或格式不受支持。", .path.display())]
    Decode { path: PathBuf },
    /// 设置项名称与参数表 `max_image_mp` 一致
    #[error("图片 \"{}\" 的像素数超过设置中的“图片像素上限（百万像素）”，不识别内容。", .path.display())]
    TooLarge { path: PathBuf },
    #[error("图片尺寸为 {width}×{height}，过于细长或为空，无法识别内容。")]
    UnsupportedSize { width: u32, height: u32 },
    #[error("图像数据长度与尺寸 {width}×{height} 不符，无法缩放。")]
    InvalidPixels { width: u32, height: u32 },
}

/// 解码第一帧并转成 8 位 RGB，已按 EXIF 方向转正、透明部分合成到白底。
/// 像素数（宽 × 高）超过 `max_pixels` 时返回 `Error::TooLarge`，不分配像素内存。
pub fn decode(path: &Path, max_pixels: u64) -> Result<Rgb, Error> {
    #[cfg(windows)]
    match windows::decode(path, max_pixels) {
        Ok(Some(f)) => return Ok(from_frame(f)),
        Ok(None) => {}
        Err(err @ Error::TooLarge { .. }) => return Err(err),
        // WIC 的报错已在 windows.rs 写调试日志，改用 image
        Err(_) => {}
    }
    #[cfg(target_os = "android")]
    match android::decode(path, max_pixels) {
        Ok(Some(img)) => return Ok(img),
        Ok(None) => {}
        Err(err @ Error::TooLarge { .. }) => return Err(err),
        // 报错已在 android.rs 写调试日志，改用 image
        Err(_) => {}
    }
    decode_image(path, max_pixels)
}

#[cfg(windows)]
fn from_frame(f: windows::Frame) -> Rgb {
    orient(over_white(f.width, f.height, &f.rgba), f.orientation)
}

fn decode_image(path: &Path, max_pixels: u64) -> Result<Rgb, Error> {
    let io_err = |source: std::io::Error| {
        log::warn!("无法读取图片“{}”：{source}", path.display());
        Error::Io {
            path: path.to_path_buf(),
            source,
        }
    };
    let reader = image::ImageReader::open(path)
        .map_err(io_err)?
        .with_guessed_format()
        .map_err(io_err)?;
    decode_reader(reader, path, max_pixels)
}

/// 解码内存中的图片文件（文档的内嵌图片），处理与 `decode` 的 `image` 路径相同；
/// `name` 只用于错误信息
pub fn decode_bytes(bytes: &[u8], name: &Path, max_pixels: u64) -> Result<Rgb, Error> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|source| {
            log::warn!("无法读取图片“{}”：{source}", name.display());
            Error::Io {
                path: name.to_path_buf(),
                source,
            }
        })?;
    decode_reader(reader, name, max_pixels)
}

fn decode_reader<R: std::io::BufRead + std::io::Seek>(
    mut reader: image::ImageReader<R>,
    path: &Path,
    max_pixels: u64,
) -> Result<Rgb, Error> {
    let decode_err = |err: image::ImageError| {
        log::warn!("无法解码图片“{}”：{err}", path.display());
        Error::Decode {
            path: path.to_path_buf(),
        }
    };
    // 单边不超过像素上限；单次分配按最宽的 16 位 RGBA（每像素 8 字节）放宽，像素数由下面按头部尺寸检查。
    let side = u32::try_from(max_pixels).unwrap_or(u32::MAX);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(side);
    limits.max_image_height = Some(side);
    limits.max_alloc = Some(max_pixels.saturating_mul(8));
    reader.limits(limits);
    let mut dec = reader.into_decoder().map_err(|err| match err {
        image::ImageError::Limits(_) => Error::TooLarge {
            path: path.to_path_buf(),
        },
        err => decode_err(err),
    })?;
    let (width, height) = dec.dimensions();
    if u64::from(width) * u64::from(height) > max_pixels {
        return Err(Error::TooLarge {
            path: path.to_path_buf(),
        });
    }
    // 方向读不到时按 1 处理，与 PIL 没有 EXIF 时相同。
    let orientation = dec.orientation().map_or(1, |o| u16::from(o.to_exif()));
    let img = image::DynamicImage::from_decoder(dec).map_err(decode_err)?;
    let wide = matches!(
        img.color(),
        image::ColorType::L16
            | image::ColorType::La16
            | image::ColorType::Rgb16
            | image::ColorType::Rgba16
    );
    let rgb = if wide {
        // image 自带的 16 → 8 位转换是四舍五入（÷257），这里取高字节，与 WIC 路径相同。
        let rgba: Vec<u8> = img.into_rgba16().iter().map(|v| (v >> 8) as u8).collect();
        over_white(width, height, &rgba)
    } else if img.color().has_alpha() {
        over_white(width, height, img.into_rgba8().as_raw())
    } else {
        Rgb {
            width,
            height,
            data: img.into_rgb8().into_raw(),
        }
    };
    Ok(orient(rgb, orientation))
}

/// 近乎纯色：缩到不超过 64×64 后亮度的标准差小于 3（满分 255）。文档中的这类图多是色块、底纹，跳过
pub fn is_flat(img: &Rgb) -> bool {
    let step = (img.width.max(img.height) / 64).max(1) as usize;
    let (w, h) = (img.width as usize, img.height as usize);
    let luma: Vec<f64> = (0..h)
        .step_by(step)
        .flat_map(|y| (0..w).step_by(step).map(move |x| (y * w + x) * 3))
        .filter_map(|i| img.data.get(i..i + 3))
        .map(|p| 0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2]))
        .collect();
    if luma.is_empty() {
        return true;
    }
    let mean = luma.iter().sum::<f64>() / luma.len() as f64;
    let var = luma.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / luma.len() as f64;
    var.sqrt() < 3.0
}

/// 非预乘 RGBA 合成到不透明白底后取 RGB，整数算法与 Pillow 12.3.0 `AlphaComposite.c` 相同
/// （目标为白底时 dst 恒为 255）。
fn over_white(width: u32, height: u32, rgba: &[u8]) -> Rgb {
    const PRECISION_BITS: u32 = 7;
    let shift_div255 = |a: u32| ((a >> 8) + a) >> 8;
    let mut data = Vec::with_capacity(rgba.len() / 4 * 3);
    for px in rgba.as_chunks::<4>().0 {
        let a = u32::from(px[3]);
        if a == 0 {
            data.extend_from_slice(&[255; 3]);
            continue;
        }
        let blend = 255 * (255 - a);
        let outa255 = a * 255 + blend;
        let coef1 = a * 255 * 255 * (1 << PRECISION_BITS) / outa255;
        let coef2 = 255 * (1 << PRECISION_BITS) - coef1;
        for &c in &px[..3] {
            let tmp = u32::from(c) * coef1 + 255 * coef2;
            data.push((shift_div255(tmp + (0x80 << PRECISION_BITS)) >> PRECISION_BITS) as u8);
        }
    }
    Rgb {
        width,
        height,
        data,
    }
}

/// 按 EXIF 方向值转正，变换与 PIL `ImageOps.exif_transpose` 相同；1 和无效值不变。
fn orient(img: Rgb, orientation: u16) -> Rgb {
    if !(2..=8).contains(&orientation) {
        return img;
    }
    let (w, h) = (img.width as usize, img.height as usize);
    // 5–8 宽高互换。
    let (ow, oh) = if orientation >= 5 { (h, w) } else { (w, h) };
    let mut data = vec![0u8; img.data.len()];
    for oy in 0..oh {
        for ox in 0..ow {
            let (sx, sy) = match orientation {
                2 => (w - 1 - ox, oy),
                3 => (w - 1 - ox, h - 1 - oy),
                4 => (ox, h - 1 - oy),
                5 => (oy, ox),
                6 => (oy, h - 1 - ox),
                7 => (w - 1 - oy, h - 1 - ox),
                _ => (w - 1 - oy, ox),
            };
            let (s, d) = ((sy * w + sx) * 3, (oy * ow + ox) * 3);
            data[d..d + 3].copy_from_slice(&img.data[s..s + 3]);
        }
    }
    Rgb {
        width: ow as u32,
        height: oh as u32,
        data,
    }
}

/// Python 正浮点数的 `a // b`：CPython `float_floor_div` 先 fmod 再相减相除，最后取整并修正，
/// 与直接 `(a / b).floor()` 在边界上可能差 1。
fn py_floordiv(a: f64, b: f64) -> f64 {
    let m = a % b;
    let div = (a - m) / b;
    let floor = div.floor();
    if div - floor > 0.5 {
        floor + 1.0
    } else {
        floor
    }
}

/// 按视觉 token 档位计算缩放后的 `(宽, 高)`，逐行移植 `scripts/golden/llama_embed.py` 的 `target_size`
/// （transformers Gemma 4 `get_aspect_ratio_preserving_size`，patch 16、pool 3）。浮点运算顺序与 Python 相同。
pub(crate) fn target_size(
    width: u32,
    height: u32,
    max_soft_tokens: u32,
) -> Result<(u32, u32), Error> {
    const PATCH: u64 = 16;
    const POOL: u64 = 3;
    let bad = Error::UnsupportedSize { width, height };
    if width == 0 || height == 0 {
        return Err(bad);
    }
    let (w, h) = (u64::from(width), u64::from(height));
    let max_patches = u64::from(max_soft_tokens) * POOL * POOL;
    // Python 的 int / int 对任意大整数都是正确舍入；这里分子很小，分母宽 × 高在 2^53 以内（像素上限远小于此）时
    // 转 f64 无误差，相除结果与 Python 相同。`** 0.5` 即 C 的 pow。
    let factor = ((max_patches * PATCH * PATCH) as f64 / (h * w) as f64).powf(0.5);
    let side = POOL * PATCH;
    let th = py_floordiv(factor * h as f64, side as f64) as u64 * side;
    let tw = py_floordiv(factor * w as f64, side as f64) as u64 * side;
    if th == 0 && tw == 0 {
        return Err(bad);
    }
    let max_side = (max_patches / (POOL * POOL)) * side;
    let (tw, th) = match (tw, th) {
        (_, 0) => ((w / h * side).min(max_side), side),
        (0, _) => (side, (h / w * side).min(max_side)),
        _ => (tw, th),
    };
    match (u32::try_from(tw), u32::try_from(th)) {
        (Ok(tw), Ok(th)) => Ok((tw, th)),
        _ => Err(bad),
    }
}

/// 解码并缩放到 `max_soft_tokens` 档的尺寸，尺寸已符合时不缩放（与 `dump_golden_inputs.py` 相同）。
pub fn prepare(path: &Path, max_soft_tokens: u32, max_pixels: u64) -> Result<Rgb, Error> {
    fit(decode(path, max_pixels)?, max_soft_tokens)
}

/// 把已解码的图缩放到 `max_soft_tokens` 档的尺寸，尺寸已符合时不缩放。需要原图宽高的调用方先 `decode` 再调用它
pub fn fit(img: Rgb, max_soft_tokens: u32) -> Result<Rgb, Error> {
    let (w, h) = target_size(img.width, img.height, max_soft_tokens)?;
    if (w, h) == (img.width, img.height) {
        return Ok(img);
    }
    resize_bicubic(&img, w, h)
}

/// 缩放到给定尺寸（与 `fit` 相同的双三次缩放），尺寸相同时原样返回
pub fn resize(img: Rgb, width: u32, height: u32) -> Result<Rgb, Error> {
    if (width, height) == (img.width, img.height) {
        return Ok(img);
    }
    resize_bicubic(&img, width, height)
}

/// 无损编码为 PNG（交给自带解码器的推理引擎）
pub fn png(img: &Rgb) -> Vec<u8> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut out,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Sub,
    )
    .write_image(
        &img.data,
        img.width,
        img.height,
        image::ExtendedColorType::Rgb8,
    )
    .expect("像素数据的长度与宽高一致，写入内存不会失败");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 参数表 `max_image_mp` 的默认值 100（百万像素）。
    const MAX_PIXELS: u64 = 100_000_000;

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
    }

    fn fnv1a64(data: &[u8]) -> u64 {
        data.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }

    /// 夹具由 tests/fixtures/gen.py 用 Pillow 生成，摘要是 Pillow 缩放结果的 FNV-1a-64。
    /// 档位为“-”的两行只缩放一个方向。
    #[test]
    fn resize_matches_pillow() {
        let dir = fixtures();
        let expected = std::fs::read_to_string(dir.join("expected.txt")).unwrap();
        let mut n = 0;
        for line in expected.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            let num = |i: usize| f[i].parse::<u32>().unwrap();
            let (w, h, tw, th) = (num(1), num(2), num(4), num(5));
            let src = Rgb {
                width: w,
                height: h,
                data: std::fs::read(dir.join(format!("{}.rgb", f[0]))).unwrap(),
            };
            if f[3] != "-" {
                assert_eq!(target_size(w, h, num(3)).unwrap(), (tw, th), "{line}");
            }
            let out = resize_bicubic(&src, tw, th).unwrap();
            assert_eq!(out.data.len(), (tw * th * 3) as usize);
            assert_eq!(format!("{:016x}", fnv1a64(&out.data)), f[6], "{line}");
            n += 1;
        }
        assert_eq!(n, 8);
    }

    #[test]
    fn resize_rejects_bad_length() {
        let img = Rgb {
            width: 2,
            height: 2,
            data: vec![0; 11],
        };
        assert!(matches!(
            resize_bicubic(&img, 1, 1),
            Err(Error::InvalidPixels { .. })
        ));
    }

    /// 两条解码路径的结果；非 Windows 只有 image 一条。
    fn both_paths(name: &str) -> Vec<(&'static str, Rgb)> {
        let path = fixtures().join(name);
        let image = ("image", decode_image(&path, MAX_PIXELS).unwrap());
        #[cfg(windows)]
        return vec![
            image,
            (
                "wic",
                from_frame(windows::decode(&path, MAX_PIXELS).unwrap().unwrap()),
            ),
        ];
        #[cfg(not(windows))]
        vec![image]
    }

    /// 16 位图按高字节转 8 位，两条路径都与 gen.py 写出的期望值逐字节相同。
    #[test]
    fn decode_16bit_takes_high_byte() {
        for name in ["rgb16", "gray16"] {
            let want = std::fs::read(fixtures().join(format!("{name}.expected.rgb"))).unwrap();
            for (path, got) in both_paths(&format!("{name}.png")) {
                assert_eq!((got.width, got.height), (31, 17), "{name} {path}");
                assert_eq!(got.data, want, "{name} {path}");
            }
        }
    }

    /// 带透明的调色板图和 LA 图：两条路径合成到白底后逐字节相同。
    #[test]
    fn decode_paths_agree_on_alpha() {
        for name in ["palette_trns.png", "la.png"] {
            let got = both_paths(name);
            for (path, img) in &got[1..] {
                assert_eq!(img, &got[0].1, "{name}：{path} 与 image 不同");
            }
        }
    }

    /// 头部声称 100000×100000 的 PNG：两条路径都在分配像素内存之前报错。
    #[test]
    fn decode_rejects_huge_header() {
        let path = fixtures().join("huge.png");
        assert!(matches!(
            decode_image(&path, MAX_PIXELS),
            Err(Error::TooLarge { .. })
        ));
        #[cfg(windows)]
        assert!(matches!(
            windows::decode(&path, MAX_PIXELS),
            Err(Error::TooLarge { .. })
        ));
        assert!(matches!(
            decode(&path, MAX_PIXELS),
            Err(Error::TooLarge { .. })
        ));
    }

    #[test]
    fn resize_same_size_is_copy() {
        let img = Rgb {
            width: 2,
            height: 1,
            data: vec![1, 2, 3, 4, 5, 6],
        };
        assert_eq!(resize_bicubic(&img, 2, 1).unwrap(), img);
    }

    /// 期望值由 `llama_embed.py` 的 `target_size` 算出（Python 版参数和返回值都是先高后宽）。
    #[test]
    fn target_size_matches_python() {
        let cases = [
            ((517, 311, 140), (720, 432)),
            ((64, 1500, 140), (96, 2736)),
            ((1536, 1152, 140), (624, 480)),
            ((1152, 1536, 140), (480, 624)),
            ((1920, 1200, 140), (672, 432)),
            ((1, 1000, 140), (48, 6720)),
            ((1000, 1, 140), (6720, 48)),
            ((3000, 20, 70), (3360, 48)),
            ((48, 48, 280), (768, 768)),
            ((100000, 1, 1120), (53760, 48)),
            ((4000, 3000, 560), (1296, 960)),
            ((333, 777, 1120), (1008, 2448)),
            ((1080, 2400, 280), (528, 1152)),
        ];
        for ((w, h, t), want) in cases {
            assert_eq!(target_size(w, h, t).unwrap(), want, "{w}×{h} @ {t}");
        }
        // 两边都算成 0 时报错（Python 抛 ValueError），以及空图。
        assert!(target_size(100, 1, 0).is_err());
        assert!(target_size(0, 10, 140).is_err());
        assert!(target_size(10, 0, 140).is_err());
    }

    /// 3×2 的图，红色通道为像素序号；期望值由 PIL `exif_transpose` 用的各 `Transpose` 算出。
    #[test]
    fn orientation_matches_pil() {
        let img = Rgb {
            width: 3,
            height: 2,
            data: (0..6u8).flat_map(|i| [i, 0, 0]).collect(),
        };
        let cases: [(u16, (u32, u32), [u8; 6]); 9] = [
            (1, (3, 2), [0, 1, 2, 3, 4, 5]),
            (2, (3, 2), [2, 1, 0, 5, 4, 3]),
            (3, (3, 2), [5, 4, 3, 2, 1, 0]),
            (4, (3, 2), [3, 4, 5, 0, 1, 2]),
            (5, (2, 3), [0, 3, 1, 4, 2, 5]),
            (6, (2, 3), [3, 0, 4, 1, 5, 2]),
            (7, (2, 3), [5, 2, 4, 1, 3, 0]),
            (8, (2, 3), [2, 5, 1, 4, 0, 3]),
            (9, (3, 2), [0, 1, 2, 3, 4, 5]),
        ];
        for (o, size, want) in cases {
            let out = orient(img.clone(), o);
            assert_eq!((out.width, out.height), size, "方向 {o}");
            let red: Vec<u8> = out.data.chunks(3).map(|p| p[0]).collect();
            assert_eq!(red, want, "方向 {o}");
        }
    }

    /// 像素为 (c, c / 2, 255 - c, a)；期望值由 PIL `alpha_composite(白底, 图).convert("RGB")` 算出。
    #[test]
    fn alpha_over_white_matches_pil() {
        let cases: [(u8, u8, [u8; 3]); 24] = [
            (0, 0, [255, 255, 255]),
            (0, 77, [255, 255, 255]),
            (0, 255, [255, 255, 255]),
            (1, 0, [254, 254, 255]),
            (1, 77, [254, 254, 255]),
            (1, 255, [255, 254, 254]),
            (64, 0, [191, 191, 255]),
            (64, 77, [210, 201, 236]),
            (64, 255, [255, 223, 191]),
            (127, 0, [128, 128, 255]),
            (127, 77, [166, 147, 217]),
            (127, 255, [255, 191, 128]),
            (128, 0, [127, 127, 255]),
            (128, 77, [166, 146, 216]),
            (128, 255, [255, 191, 127]),
            (200, 0, [55, 55, 255]),
            (200, 77, [115, 85, 195]),
            (200, 255, [255, 155, 55]),
            (254, 0, [1, 1, 255]),
            (254, 77, [78, 39, 178]),
            (254, 255, [255, 128, 1]),
            (255, 0, [0, 0, 255]),
            (255, 77, [77, 38, 178]),
            (255, 255, [255, 127, 0]),
        ];
        for (a, c, want) in cases {
            let out = over_white(1, 1, &[c, c / 2, 255 - c, a]);
            assert_eq!(out.data, want, "a = {a}，c = {c}");
        }
        // 不透明时原样保留。
        let all: Vec<u8> = (0..=255u8).flat_map(|v| [v, v, v, 255]).collect();
        let out = over_white(256, 1, &all);
        assert!(
            out.data
                .chunks(3)
                .enumerate()
                .all(|(i, p)| p == [i as u8; 3])
        );
    }

    /// 黄金图片的 ID 与文件，来自 assets/golden/manifest.json。
    fn golden_images() -> Vec<(String, PathBuf)> {
        let root = repo_root().join("assets/golden");
        let text = std::fs::read_to_string(root.join("manifest.json")).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&text).unwrap();
        manifest["images"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                (
                    i["id"].as_str().unwrap().to_string(),
                    root.join(i["file"].as_str().unwrap()),
                )
            })
            .collect()
    }

    /// 与 `dump_golden_inputs.py` 导出的 PIL 像素（`.cache/golden/pil-inputs`）比较：PNG 必须逐字节相同，JPEG 只报告解码差异。
    #[test]
    fn golden_pixels_match_pil() {
        let inputs = repo_root().join(".cache/golden/pil-inputs");
        for (id, file) in golden_images() {
            let want_path = inputs.join(&id).join("0.rgb");
            if !want_path.exists() {
                println!("跳过 {id}：找不到 {}", want_path.display());
                continue;
            }
            let want = std::fs::read(&want_path).unwrap();
            // 与导出的档位相同
            let got = prepare(&file, 280, MAX_PIXELS).unwrap();
            assert_eq!(got.data.len(), want.len(), "{id} 尺寸不同");
            let diff: Vec<u8> = got
                .data
                .iter()
                .zip(&want)
                .map(|(a, b)| a.abs_diff(*b))
                .collect();
            let max = diff.iter().copied().max().unwrap_or(0);
            let px_diff = diff.chunks(3).filter(|d| d.iter().any(|&v| v != 0)).count();
            let total = diff.len() / 3;
            println!(
                "{id}：{}×{}，最大差值 {max}，有差异的像素 {px_diff} / {total}（{:.3}%）",
                got.width,
                got.height,
                px_diff as f64 * 100.0 / total as f64
            );
            if file.extension().is_some_and(|e| e == "png") {
                assert_eq!(max, 0, "{id} 是 PNG，必须与 PIL 逐字节相同");
            }
        }
    }

    #[test]
    fn decode_bytes_and_flat() {
        let mut png = Vec::new();
        let img = image::RgbaImage::from_fn(80, 60, |x, _| {
            image::Rgba([(x * 3) as u8, 100, 200, if x < 10 { 0 } else { 255 }])
        });
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let rgb = decode_bytes(&png, Path::new("文档.pptx"), MAX_PIXELS).unwrap();
        assert_eq!((rgb.width, rgb.height), (80, 60));
        // 透明处合成到白底
        assert_eq!(rgb.data[..3], [255, 255, 255]);
        assert!(!is_flat(&rgb));
        assert!(matches!(
            decode_bytes(&png, Path::new("文档.pptx"), 100),
            Err(Error::TooLarge { .. })
        ));
        let err = decode_bytes(b"not an image", Path::new("文档.pptx"), MAX_PIXELS).unwrap_err();
        assert!(err.to_string().contains("文档.pptx"), "{err}");
        let flat = Rgb {
            width: 300,
            height: 200,
            data: [250u8, 251, 249].repeat(300 * 200),
        };
        assert!(is_flat(&flat));
    }
}
