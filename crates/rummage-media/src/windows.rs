//! Windows 的 WIC 解码：系统装了扩展时还能解 HEIC、AVIF、RAW 等。
//!
//! - 只读第一帧，转成非预乘 RGBA；不做色彩管理（与 PIL 相同）。每通道 16 位的来源先转成 64 位 RGBA
//!   （16 位灰度原样读出），再自己取高字节，不依赖 WIC 的取整规则（与 `image` 回退路径和 PIL 处理 16 位 RGB PNG 一致）。
//! - GIF 不走 WIC：WIC 第 0 帧的尺寸是帧本身而不是整张画布，返回 `None` 交给 `image` crate。
//! - 在 `GetSize` 之后、分配像素内存之前检查像素上限。
//! - EXIF 方向依次查 JPEG、TIFF 的元数据路径和照片元数据策略名，读不到当作 1；方向的变换与白底合成由调用方完成。
//!   HEIC 等格式的系统解码器可能已按 irot/imir 转正，与 EXIF 方向叠加的情况待真机验证。
//! - 每次调用在当前线程初始化 COM（MTA）；线程已是 STA 时 COM 照样可用，只是不由这里反初始化。
//! - 系统缩略图：`shell_thumbnail` 用 `IShellItemImageFactory::GetImage`，先带 `SIIGBF_INCACHEONLY` 只读系统缓存，
//!   未命中再正常生成；只要缩略图（`SIIGBF_THUMBNAILONLY`），不要文件图标。位图按 32 位自上而下读出：
//!   alpha 全为 0 时视为不透明（不带透明通道的图常见），否则按预乘 alpha 合成到白底（实测 GetImage 返回的是
//!   预乘 alpha：(100, 100, 100, 128) 的 PNG 读出 (50, 50, 50, 128)，见测试 `shell_alpha_is_premultiplied`）。按 STA 初始化 COM，
//!   部分缩略图处理程序要求单线程套间。调用方须先做占位文件检查（`thumb.rs`）。

use std::path::Path;
use std::ptr;

use ::windows::Win32::Foundation::{GENERIC_READ, SIZE};
use ::windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GetDIBits, GetObjectW, HBITMAP, HGDIOBJ,
};
use ::windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatGif, GUID_WICPixelFormat16bppGray,
    GUID_WICPixelFormat32bppRGBA, GUID_WICPixelFormat64bppRGBA, IWICBitmapFrameDecode,
    IWICImagingFactory, IWICPixelFormatInfo, WICConvertBitmapSource,
    WICDecodeMetadataCacheOnDemand,
};
use ::windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use ::windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, COINIT_MULTITHREADED,
    CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use ::windows::Win32::System::Variant::VT_UI2;
use ::windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_INCACHEONLY, SIIGBF_THUMBNAILONLY,
};
use ::windows::core::{GUID, HSTRING, Interface, PCWSTR, w};

use crate::{Error, Rgb};

/// 解码出的第一帧：8 位非预乘 RGBA，按行存放；`orientation` 为 EXIF 方向值（1–8）。
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub orientation: u16,
}

/// EXIF 方向标签（274）的查询路径：JPEG 的 APP1、TIFF 的 IFD0，其余格式（HEIC 等）用照片元数据策略名。
const ORIENTATION_QUERIES: [PCWSTR; 3] = [
    w!("/app1/ifd/{ushort=274}"),
    w!("/ifd/{ushort=274}"),
    w!("System.Photo.Orientation"),
];

/// `Ok(None)` 表示应改用 `image` crate（GIF）；`Error::TooLarge` 之外的错误由调用方回退到 `image` crate。
pub fn decode(path: &Path, max_pixels: u64) -> Result<Option<Frame>, Error> {
    // SAFETY: 保留参数为空、标志为合法常量；只在初始化成功（含 S_FALSE）时配对调用 CoUninitialize，
    // 且此前 read 中创建的 COM 对象都已在 read 返回时释放。
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let res = read(path, max_pixels);
    if hr.is_ok() {
        // SAFETY: 与上面成功的 CoInitializeEx 配对。
        unsafe { CoUninitialize() };
    }
    res
}

fn read(path: &Path, max_pixels: u64) -> Result<Option<Frame>, Error> {
    // 失败后由调用方改用 image crate，细节只记调试日志
    let wic = |err: ::windows::core::Error| {
        log::debug!("WIC 无法解码“{}”：{}", path.display(), err.message());
        Error::Decode {
            path: path.to_path_buf(),
        }
    };
    // SAFETY: CLSID 与接口类型匹配，没有聚合对象。
    let factory: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
            .map_err(wic)?;
    // SAFETY: 文件名是以空字符结尾的 HSTRING，调用期间有效；厂商 GUID 为空表示不限定解码器。
    let decoder = unsafe {
        factory.CreateDecoderFromFilename(
            &HSTRING::from(path),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnDemand,
        )
    }
    .map_err(wic)?;
    // SAFETY: decoder 是有效的 COM 对象。
    if unsafe { decoder.GetContainerFormat() }.map_err(wic)? == GUID_ContainerFormatGif {
        return Ok(None);
    }
    // SAFETY: decoder 有效，0 号帧不存在时返回错误。
    let frame = unsafe { decoder.GetFrame(0) }.map_err(wic)?;
    let (mut width, mut height) = (0u32, 0u32);
    // SAFETY: 两个输出指针指向本地变量。
    unsafe { frame.GetSize(&mut width, &mut height) }.map_err(wic)?;
    let too_large = || Error::TooLarge {
        path: path.to_path_buf(),
    };
    if u64::from(width) * u64::from(height) > max_pixels {
        return Err(too_large());
    }

    // SAFETY: frame 有效。
    let format = unsafe { frame.GetPixelFormat() }.map_err(wic)?;
    // 16 位灰度原样读出：实测 WIC 把它转成 64 位 RGBA 时，低字节为 0 的值会变成高字节减 1。
    let gray16 = format == GUID_WICPixelFormat16bppGray;
    let wide = !gray16 && bits_per_channel(&factory, &format) == Some(16);
    let (target, bytes) = match (gray16, wide) {
        (true, _) => (GUID_WICPixelFormat16bppGray, 2u64),
        (_, true) => (GUID_WICPixelFormat64bppRGBA, 8),
        _ => (GUID_WICPixelFormat32bppRGBA, 4),
    };
    // CopyPixels 的步长和缓冲区大小都是 u32。
    let len = u64::from(width) * u64::from(height) * bytes;
    if len > u64::from(u32::MAX) {
        return Err(too_large());
    }
    let orientation = orientation(&frame);
    // SAFETY: 目标格式 GUID 是静态常量，frame 有效。
    let src = unsafe { WICConvertBitmapSource(&target, &frame) }.map_err(wic)?;
    let mut buf = Vec::new();
    buf.try_reserve_exact(len as usize)
        .map_err(|_| too_large())?;
    buf.resize(len as usize, 0);
    // SAFETY: 矩形为空表示整幅图；缓冲区长度正好是步长 × 高度（已确认不超过 u32），绑定层按切片长度传入缓冲区大小。
    unsafe { src.CopyPixels(ptr::null(), width * bytes as u32, &mut buf) }.map_err(wic)?;
    // 16 位的每个通道是本机字节序（小端）u16，高字节在后。
    let rgba = match (gray16, wide) {
        (true, _) => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|c| [c[1], c[1], c[1], 255])
            .collect(),
        (_, true) => buf.as_chunks::<2>().0.iter().map(|c| c[1]).collect(),
        _ => buf,
    };
    Ok(Some(Frame {
        width,
        height,
        rgba,
        orientation,
    }))
}

/// 像素格式每通道的位数；查不到时返回 `None`，按 8 位处理。
fn bits_per_channel(factory: &IWICImagingFactory, format: &GUID) -> Option<u32> {
    // SAFETY: format 指向有效的 GUID；返回的组件信息若是像素格式，cast 成功。
    let info: IWICPixelFormatInfo = unsafe { factory.CreateComponentInfo(format) }
        .ok()?
        .cast()
        .ok()?;
    // SAFETY: info 是有效的 COM 对象。
    let (bits, channels) = unsafe { (info.GetBitsPerPixel().ok()?, info.GetChannelCount().ok()?) };
    (channels > 0).then(|| bits / channels)
}

fn orientation(frame: &IWICBitmapFrameDecode) -> u16 {
    // SAFETY: frame 有效；不支持元数据的格式返回错误。
    let Ok(reader) = (unsafe { frame.GetMetadataQueryReader() }) else {
        return 1;
    };
    for query in ORIENTATION_QUERIES {
        let mut value = PROPVARIANT::default();
        // SAFETY: 查询名是静态的宽字符串，value 是已清零的 PROPVARIANT，成功时由下面的 PropVariantClear 释放。
        if unsafe { reader.GetMetadataByName(query, &mut value) }.is_err() {
            continue;
        }
        // SAFETY: 读联合体前先检查类型标记，VT_UI2 时 uiVal 是有效成员。
        let found = unsafe {
            let v = &value.Anonymous.Anonymous;
            (v.vt == VT_UI2).then_some(v.Anonymous.uiVal)
        };
        // SAFETY: value 由 GetMetadataByName 填写，只释放一次。
        let _ = unsafe { PropVariantClear(&mut value) };
        if let Some(o) = found.filter(|o| (1..=8).contains(o)) {
            return o;
        }
    }
    1
}

/// 向系统要一张不超过 `cx` × `cy` 的缩略图（保持宽高比），转成白底 RGB。错误为给日志看的说明
pub fn shell_thumbnail(path: &Path, cx: i32, cy: i32) -> Result<Rgb, String> {
    // SAFETY: 保留参数为空、标志为合法常量；只在初始化成功（含 S_FALSE）时配对调用 CoUninitialize，
    // 且 shell_image 中创建的 COM 对象都已在它返回时释放。
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    let res = shell_image(path, cx, cy);
    if hr.is_ok() {
        // SAFETY: 与上面成功的 CoInitializeEx 配对。
        unsafe { CoUninitialize() };
    }
    res
}

fn shell_image(path: &Path, cx: i32, cy: i32) -> Result<Rgb, String> {
    let msg = |err: ::windows::core::Error| err.message();
    // SAFETY: 路径是以空字符结尾的 HSTRING，调用期间有效；不传绑定上下文。
    let factory: IShellItemImageFactory =
        unsafe { SHCreateItemFromParsingName(&HSTRING::from(path), None) }.map_err(msg)?;
    let size = SIZE { cx, cy };
    // SAFETY: factory 是有效的 COM 对象，标志为合法常量；成功时返回的位图归调用方所有，由下面释放。
    let hbm = unsafe { factory.GetImage(size, SIIGBF_THUMBNAILONLY | SIIGBF_INCACHEONLY) }
        // SAFETY: 同上。
        .or_else(|_| unsafe { factory.GetImage(size, SIIGBF_THUMBNAILONLY) })
        .map_err(msg)?;
    let res = bitmap_rgb(hbm);
    // SAFETY: hbm 是 GetImage 交给调用方的位图，此后不再使用，只释放一次。
    unsafe {
        let _ = DeleteObject(HGDIOBJ(hbm.0));
    }
    res
}

/// 读出位图像素：32 位自上而下的 BGRA
fn bitmap_rgb(hbm: HBITMAP) -> Result<Rgb, String> {
    let mut bm = BITMAP::default();
    // SAFETY: hbm 是有效的位图句柄，输出缓冲区是大小正确的 BITMAP。
    let n = unsafe {
        GetObjectW(
            HGDIOBJ(hbm.0),
            size_of::<BITMAP>() as i32,
            Some((&raw mut bm).cast()),
        )
    };
    if n == 0 || bm.bmWidth <= 0 || bm.bmHeight == 0 {
        return Err("无法读取缩略图位图的尺寸".into());
    }
    let (w, h) = (bm.bmWidth as u32, bm.bmHeight.unsigned_abs());
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            // 负值表示自上而下
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut buf = vec![0u8; w as usize * h as usize * 4];
    // SAFETY: 不传 DC 时创建与屏幕兼容的内存 DC，下面配对释放。
    let dc = unsafe { CreateCompatibleDC(None) };
    // SAFETY: 缓冲区正好是 宽 × 高 × 4 字节（32 位行天然 4 字节对齐），info 描述的格式与之一致；
    // 位图此时没有被选入任何 DC。
    let lines = unsafe {
        GetDIBits(
            dc,
            hbm,
            0,
            h,
            Some(buf.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    // SAFETY: dc 由上面的 CreateCompatibleDC 创建，只释放一次。
    unsafe {
        let _ = DeleteDC(dc);
    }
    if lines != h as i32 {
        return Err("无法读取缩略图位图的像素".into());
    }
    let data = bgra_over_white(&buf);
    Ok(Rgb {
        width: w,
        height: h,
        data,
    })
}

/// 32 位 BGRA 转为白底 RGB：alpha 全为 0 时视为不透明；否则按预乘 alpha 合成，`c + (255 − a)`
fn bgra_over_white(bgra: &[u8]) -> Vec<u8> {
    let px = bgra.as_chunks::<4>().0;
    let opaque = px.iter().all(|p| p[3] == 0);
    px.iter()
        .flat_map(|p| {
            if opaque {
                return [p[2], p[1], p[0]];
            }
            let bg = 255 - p[3];
            [
                p[2].saturating_add(bg),
                p[1].saturating_add(bg),
                p[0].saturating_add(bg),
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplied_over_white() {
        // 全不透明、半透明灰（预乘后 50）、全透明、alpha 全为 0 的整幅图
        assert_eq!(
            bgra_over_white(&[10, 20, 30, 255, 50, 50, 50, 128, 0, 0, 0, 0]),
            [30, 20, 10, 177, 177, 177, 255, 255, 255]
        );
        assert_eq!(
            bgra_over_white(&[10, 20, 30, 0, 1, 2, 3, 0]),
            [30, 20, 10, 3, 2, 1]
        );
    }

    #[test]
    fn shell_alpha_is_premultiplied() {
        let dir = std::env::temp_dir().join(format!("rummage-alpha-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("半透明.png");
        // 左半边 50% 透明的灰 (100, 100, 100)，右半边不透明的蓝
        let img = image::RgbaImage::from_fn(200, 100, |x, _| {
            if x < 100 {
                image::Rgba([100, 100, 100, 128])
            } else {
                image::Rgba([0, 0, 255, 255])
            }
        });
        img.save(&path).unwrap();
        let res = shell_thumbnail(&path, 480, 480);
        let _ = std::fs::remove_dir_all(&dir);
        let out = match res {
            Ok(out) => out,
            Err(err) => {
                eprintln!("系统缩略图接口不可用（{err}），跳过");
                return;
            }
        };
        // 合成到白底应为 100 × 0.5 + 255 × 0.5 ≈ 177；若把预乘值当作直通 alpha 再合成会得到 152 左右
        let left = &out.data[..3];
        assert!(left.iter().all(|c| c.abs_diff(177) <= 2), "{left:?}");
        let right = &out.data[(out.width as usize - 1) * 3..out.width as usize * 3];
        assert!(
            right[0] <= 2 && right[1] <= 2 && right[2] >= 253,
            "{right:?}"
        );
    }
}
