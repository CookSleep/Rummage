//! 缩略图：宽不超过 `THUMB_WIDTH`，高不超过宽 × `THUMB_MAX_RATIO`（更长的图只保留顶部，
//! 与瀑布流卡片的最大显示高度一致），编码为有损 WebP。
//! - Windows 先用系统缩略图接口 `IShellItemImageFactory::GetImage`（`windows.rs`）：先只读系统缓存，未命中再正常生成；
//!   系统给不出缩略图时（没有对应的缩略图处理程序等）退回自己解码。
//! - Android 先用 `AImageDecoder` 在解码时直接缩到缩略图宽度（`android.rs`），失败再自己解码；其他平台只自己解码。
//! - **调用方必须先确认不是网盘占位文件**（`rummage_index::ensure_local`），这里不再检查：
//!   系统缩略图接口和解码都会读取文件内容。
use std::path::Path;

use crate::{Error, Rgb, decode, resize_bicubic};

/// 缩略图的宽度（像素），供瀑布流卡片在高分屏上清晰显示
pub(crate) const THUMB_WIDTH: u32 = 480;
/// 缩略图与瀑布流卡片图片区域的最大高宽比。对应前端 `src/lib/masonry.ts` 的 `MAX_IMAGE_RATIO`
const THUMB_MAX_RATIO: u32 = 2;
/// 向系统要缩略图时高宽比的上限，避免为极长的图要一张巨大的位图；更长的图系统会按比例缩窄
const REQUEST_MAX_RATIO: f64 = 8.0;
/// WebP 有损压缩质量（0–100）
const QUALITY: f32 = 80.0;

/// 生成 WebP 缩略图。`aspect` 为已知的高宽比（索引中记录的宽高），用于向系统要合适大小的缩略图；
/// 未知时按最大高宽比要（系统按比例缩放到框内，宽图、方图仍是 480 宽，长图不会因框太矮而变窄）。`max_pixels` 是自己解码时的像素上限（参数表 `max_image_mp`）
pub fn thumbnail(path: &Path, aspect: Option<f64>, max_pixels: u64) -> Result<Vec<u8>, Error> {
    #[cfg(windows)]
    let shell = {
        let ratio = aspect
            .filter(|a| a.is_finite() && *a > 0.0)
            .unwrap_or(REQUEST_MAX_RATIO)
            .min(REQUEST_MAX_RATIO);
        let cy = (f64::from(THUMB_WIDTH) * ratio).round().max(1.0) as i32;
        crate::windows::shell_thumbnail(path, THUMB_WIDTH as i32, cy)
    };
    #[cfg(target_os = "android")]
    let shell = {
        let _ = (aspect, REQUEST_MAX_RATIO);
        crate::android::thumbnail(path, THUMB_WIDTH, THUMB_MAX_RATIO, max_pixels)
    };
    #[cfg(not(any(windows, target_os = "android")))]
    let shell: Result<Rgb, String> = {
        let _ = (aspect, REQUEST_MAX_RATIO);
        Err("没有系统缩略图接口".into())
    };
    let img = match shell {
        Ok(img) => img,
        Err(err) => {
            log::debug!("系统缩略图不可用，改为自己解码：{err}");
            decode(path, max_pixels)?
        }
    };
    shrink_webp(img, path)
}

/// 已解码的图（长图命中区域的裁剪、视频画面帧）按缩略图的规则缩小并编码为 WebP；`path` 只用于错误提示
pub fn shrink_webp(img: Rgb, path: &Path) -> Result<Vec<u8>, Error> {
    encode(&shrink(img)?, path)
}

/// 先从底部裁到最大高宽比，再按比例缩到 `THUMB_WIDTH` 宽（不放大）
fn shrink(img: Rgb) -> Result<Rgb, Error> {
    let (w, h) = (img.width, img.height);
    if w == 0 || h == 0 {
        return Err(Error::UnsupportedSize {
            width: w,
            height: h,
        });
    }
    let max_h = w.saturating_mul(THUMB_MAX_RATIO);
    let img = if h > max_h {
        let mut data = img.data;
        data.truncate(w as usize * max_h as usize * 3);
        Rgb {
            width: w,
            height: max_h,
            data,
        }
    } else {
        img
    };
    if img.width <= THUMB_WIDTH {
        return Ok(img);
    }
    let th = (u64::from(img.height) * u64::from(THUMB_WIDTH) / u64::from(img.width)).max(1) as u32;
    resize_bicubic(&img, THUMB_WIDTH, th)
}

fn encode(img: &Rgb, path: &Path) -> Result<Vec<u8>, Error> {
    webp::Encoder::from_rgb(&img.data, img.width, img.height)
        .encode_simple(false, QUALITY)
        .map(|mem| mem.to_vec())
        .map_err(|err| {
            log::warn!("“{}”的 WebP 编码失败：{err:?}", path.display());
            Error::Decode {
                path: path.to_path_buf(),
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32) -> Rgb {
        Rgb {
            width,
            height,
            data: (0..width * height)
                .flat_map(|i| [(i % 251) as u8, 128, 200])
                .collect(),
        }
    }

    #[test]
    fn shrink_caps_width_and_height() {
        // 宽图：缩到 480 宽，按比例
        let out = shrink(solid(960, 600)).unwrap();
        assert_eq!((out.width, out.height), (480, 300));
        // 长图：先裁到 2 倍宽的高度（保留顶部），再缩
        let cut = shrink(solid(960, 5000)).unwrap();
        assert_eq!((cut.width, cut.height), (480, 960));
        assert_eq!(cut.data.len(), 480 * 960 * 3);
        // 小图不放大；只裁不缩时第一行与原图相同
        let out = shrink(solid(100, 1000)).unwrap();
        assert_eq!((out.width, out.height), (100, 200));
        let small = solid(100, 1000);
        assert_eq!(out.data[..], small.data[..100 * 200 * 3]);
        assert!(shrink(solid(0, 10)).is_err());
    }

    #[test]
    fn encodes_webp() {
        let bytes = encode(&shrink(solid(640, 480)).unwrap(), Path::new("a.png")).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WEBP");
    }

    #[test]
    fn thumbnail_of_golden_image() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/golden/images/img02_photo_corgi.jpg");
        if !path.exists() {
            eprintln!("缺少黄金图片 {}，跳过", path.display());
            return;
        }
        // 系统缩略图接口不接受带 `..` 和 `\\?\` 前缀的路径；索引中的路径都是规范化后去掉前缀的形式
        let full = path.canonicalize().unwrap().to_string_lossy().into_owned();
        let path = std::path::PathBuf::from(full.strip_prefix(r"\\?\").unwrap_or(&full));
        #[cfg(windows)]
        match crate::windows::shell_thumbnail(&path, 480, 360) {
            Ok(img) => {
                assert_eq!(img.width, 480);
                assert!(img.height.abs_diff(360) <= 1, "{}", img.height);
                assert_eq!(img.data.len(), (img.width * img.height * 3) as usize);
            }
            Err(err) => eprintln!("系统缩略图接口不可用（{err}），跳过这部分检查"),
        }
        let img = decode(&path, 100_000_000).unwrap();
        let aspect = f64::from(img.height) / f64::from(img.width);
        let bytes = thumbnail(&path, Some(aspect), 100_000_000).unwrap();
        assert_eq!(&bytes[8..12], b"WEBP");
        // 不支持的文件：系统接口拿不到缩略图，自己解码也失败
        let dir = std::env::temp_dir().join(format!("rummage-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("坏.jpg");
        std::fs::write(&bad, b"not an image").unwrap();
        assert!(thumbnail(&bad, None, 100_000_000).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
