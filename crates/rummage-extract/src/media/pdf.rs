//! PDF 的内嵌图片：`xberg-native-pdf` 的图像句柄。
//!
//! - 列出时只遍历内容流（`page_image_handles`），读压缩流算去重的键，不解码。
//! - 1.3.9 不套用 SMask：遮罩（`/SMask`、`/Mask`）作为紧随原图之后、显示框相同的灰度图返回，按此识别并跳过；
//!   取出时把它当作透明度，把原图合成到白底（与普通图片相同）。句柄不给出对象号，只能这样判断。
//! - 带 `/Decode` 的 CMYK JPEG 解出来偏黑（1.3.9 的缺陷），不另做处理。
use xberg_native_pdf::PdfDocument;
use xberg_native_pdf::extractors::images::{ColorSpace, ImageData};
use xberg_native_pdf::{PdfFilter, PdfImageHandle};

use super::{At, Cand, Loaded, MediaError, MediaKind, SCAN_AREA};

fn pdf_err(err: impl std::fmt::Display) -> MediaError {
    MediaError::Pdf(err.to_string())
}

/// 像素宽高：DCTDecode（JPEG）以 JPEG 头部的尺寸与图像字典中的尺寸各取较大值，字典可能写小了，按它判断像素上限不可靠。
/// JPEG 前面可能还有 ASCII85 等编码，用 `decode` 取出 JPEG 数据：DCT 编码的图它只解开外层编码、不解码像素
fn pixel_size(h: &PdfImageHandle) -> (u32, u32) {
    if h.filter_chain.last() != Some(&PdfFilter::DCTDecode) {
        return (h.width, h.height);
    }
    let jpeg = h.decode().ok().and_then(|img| match img.data() {
        ImageData::Jpeg(j) => {
            image::ImageReader::with_format(std::io::Cursor::new(j), image::ImageFormat::Jpeg)
                .into_dimensions()
                .ok()
        }
        ImageData::Raw { .. } => None,
    });
    let (w, hh) = jpeg.unwrap_or((0, 0));
    (h.width.max(w), h.height.max(hh))
}

/// `next` 是 `img` 的遮罩（见模块说明）
fn is_mask(img: &PdfImageHandle, next: &PdfImageHandle) -> bool {
    next.bbox == img.bbox && matches!(next.color_space, ColorSpace::DeviceGray)
}

/// 页面上的图像句柄，去掉遮罩；每项为（绘制顺序，句柄，后面紧跟的遮罩）
#[allow(clippy::type_complexity)]
fn images<'a, 'd>(
    handles: &'a [PdfImageHandle<'d>],
) -> Vec<(
    usize,
    &'a PdfImageHandle<'d>,
    Option<&'a PdfImageHandle<'d>>,
)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < handles.len() {
        let mask = handles.get(i + 1).filter(|m| is_mask(&handles[i], m));
        out.push((i, &handles[i], mask));
        i += if mask.is_some() { 2 } else { 1 };
    }
    out
}

pub(super) fn candidates(doc: &PdfDocument) -> Result<Vec<Cand>, MediaError> {
    let pages = doc.page_count().map_err(pdf_err)?;
    let mut out = Vec::new();
    for p in 0..pages {
        let (x0, y0, x1, y1) = doc.get_page_media_box(p).map_err(pdf_err)?;
        let (pw, ph) = (f64::from(x1 - x0), f64::from(y1 - y0));
        let handles = match doc.page_image_handles(p) {
            Ok(h) => h,
            Err(err) => {
                log::debug!("无法列出 PDF 第 {} 页的图片：{err}", p + 1);
                continue;
            }
        };
        let page = p as u32 + 1;
        for (pos, h, _) in images(&handles) {
            // 压缩流读不出的图也解不出来
            let Ok(raw) = h.raw_compressed_bytes() else {
                continue;
            };
            let (w, hh) = (f64::from(h.bbox.width), f64::from(h.bbox.height));
            let display = (pw > 0.0 && ph > 0.0).then(|| (w / pw, hh / ph));
            out.push(Cand {
                kind: MediaKind::Image,
                at: At::Pdf {
                    page,
                    pos: pos as u32,
                },
                page: Some(page),
                slide: None,
                sheet: None,
                place: page,
                key: blake3::hash(&raw).as_bytes().to_vec(),
                size: Some(pixel_size(h)),
                display,
                scan: display.is_some_and(|(a, b)| a * b >= SCAN_AREA),
            });
        }
    }
    Ok(out)
}

/// 解码成 8 位 RGB，有遮罩时合成到白底
pub(super) fn load(
    doc: &PdfDocument,
    page: u32,
    pos: u32,
    max_pixels: u64,
) -> Result<Loaded, MediaError> {
    let missing = || MediaError::Missing(format!("PDF 第 {page} 页的第 {pos} 个图像"));
    let p = page.checked_sub(1).ok_or_else(missing)? as usize;
    let handles = doc.page_image_handles(p).map_err(pdf_err)?;
    let (_, h, mask) = images(&handles)
        .into_iter()
        .find(|(i, ..)| *i == pos as usize)
        .ok_or_else(missing)?;
    let (w, hh) = pixel_size(h);
    if u64::from(w) * u64::from(hh) > max_pixels {
        return Err(MediaError::TooLarge(format!("PDF 第 {page} 页的图像")));
    }
    let img = h
        .decode()
        .and_then(|i| i.to_dynamic_image())
        .map_err(pdf_err)?
        .into_rgb8();
    let (width, height) = img.dimensions();
    let mut data = img.into_raw();
    let alpha = mask.and_then(|m| {
        let a = m
            .decode()
            .and_then(|i| i.to_dynamic_image())
            .ok()?
            .into_luma8();
        Some(if a.dimensions() == (width, height) {
            a
        } else {
            image::imageops::resize(&a, width, height, image::imageops::FilterType::Triangle)
        })
    });
    if let Some(a) = alpha {
        for (px, &a) in data.as_chunks_mut::<3>().0.iter_mut().zip(a.as_raw()) {
            for c in px {
                *c =
                    ((u32::from(*c) * u32::from(a) + 255 * (255 - u32::from(a)) + 127) / 255) as u8;
            }
        }
    }
    Ok(Loaded::Rgb {
        width,
        height,
        data,
    })
}
