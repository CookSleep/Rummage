//! 8 位 RGB 的双三次缩放，逐行移植自 Pillow 12.3.0 的 `src/libImaging/Resample.c`
//! （`precompute_coeffs`、`normalize_coeffs_8bpc`、`ImagingResampleHorizontal_8bpc` / `Vertical_8bpc`、
//! `ImagingResampleInner`）。Pillow 的许可证为 MIT-CMU（HPND），属于宽松协议，原文见
//! `third-party-licenses/Pillow-LICENSE.txt`。
//!
//! 目的是与生成黄金样本用的 `Image.resize((w, h), Image.BICUBIC)` 逐字节相同，两端共用这一份纯 Rust 代码：
//! - 双三次核 a = -0.5，缩小时按缩放比放大支撑范围（抗锯齿）；
//! - 系数先以 f64 计算并归一化，再按 `PRECISION_BITS` 转成定点整数，每趟结果截断到 8 位；
//! - 先水平后垂直，水平趟只处理垂直趟用得到的行；尺寸不变的方向不做。
//!
//! 浮点运算的顺序与 C 源码保持一致，不要“化简”表达式，否则系数的末位会变，结果不再逐字节相同。

use crate::{Error, Rgb};

/// 双三次核的支撑半径。
const SUPPORT: f64 = 2.0;
/// 结果 8 位，核有负值区，和可能小于 0 或大于 1，因此留 2 位余量（与 Resample.c 相同）。
const PRECISION_BITS: u32 = 32 - 8 - 2;

fn bicubic_filter(x: f64) -> f64 {
    const A: f64 = -0.5;
    let x = if x < 0.0 { -x } else { x };
    if x < 1.0 {
        return ((A + 2.0) * x - (A + 3.0)) * x * x + 1.0;
    }
    if x < 2.0 {
        return (((x - 5.0) * x + 8.0) * x - 4.0) * A;
    }
    0.0
}

/// 每个输出位置的取样起点、取样个数和定点系数（每个位置 `ksize` 个，不足的补 0）。
struct Coeffs {
    ksize: usize,
    bounds: Vec<(usize, usize)>,
    kk: Vec<i32>,
}

/// `precompute_coeffs` 与 `normalize_coeffs_8bpc`，取样框固定为整幅图 `(0, in_size)`。
fn precompute_coeffs(in_size: u32, out_size: u32) -> Coeffs {
    // C 中取样框是 float，先做 float 减法再转 double。
    let (in0, in1) = (0.0f32, in_size as f32);
    let scale = f64::from(in1 - in0) / f64::from(out_size);
    let filterscale = if scale < 1.0 { 1.0 } else { scale };
    let support = SUPPORT * filterscale;
    let ksize = support.ceil() as usize * 2 + 1;

    let mut bounds = Vec::with_capacity(out_size as usize);
    let mut kk = vec![0i32; out_size as usize * ksize];
    let mut k = vec![0f64; ksize];
    let inv_filterscale = 1.0 / filterscale;
    for xx in 0..out_size as usize {
        let center = f64::from(in0) + (xx as f64 + 0.5) * scale;
        let mut ww = 0.0;
        // C 的 (int) 向零截断，与 Rust 的 as 相同。
        let xmin = ((center - support + 0.5) as i32).max(0);
        let xmax = ((center + support + 0.5) as i32).min(in_size as i32) - xmin;
        let xmin = xmin as usize;
        let xmax = xmax.max(0) as usize;
        k.fill(0.0);
        for (x, w) in k.iter_mut().enumerate().take(xmax) {
            *w = bicubic_filter(((x + xmin) as f64 - center + 0.5) * inv_filterscale);
            ww += *w;
        }
        if ww != 0.0 {
            for w in k.iter_mut().take(xmax) {
                *w /= ww;
            }
        }
        for (dst, &w) in kk[xx * ksize..(xx + 1) * ksize].iter_mut().zip(&k) {
            *dst = if w < 0.0 {
                (-0.5 + w * f64::from(1u32 << PRECISION_BITS)) as i32
            } else {
                (0.5 + w * f64::from(1u32 << PRECISION_BITS)) as i32
            };
        }
        bounds.push((xmin, xmax));
    }
    Coeffs { ksize, bounds, kk }
}

/// Resample.c 的 `clip8`：查表范围之外的值同样钳到 0 或 255。
fn clip8(ss: i32) -> u8 {
    (ss >> PRECISION_BITS).clamp(0, 255) as u8
}

/// 水平趟：取输入的 `[row0, row0 + rows)` 行，宽度缩放到 `out_w`。
fn horizontal(src: &Rgb, row0: usize, rows: usize, out_w: u32, c: &Coeffs) -> Rgb {
    let in_stride = src.width as usize * 3;
    let mut data = vec![0u8; rows * out_w as usize * 3];
    for (yy, line_out) in data.chunks_exact_mut(out_w as usize * 3).enumerate() {
        let line_in = &src.data[(yy + row0) * in_stride..(yy + row0 + 1) * in_stride];
        for (xx, px) in line_out.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let (xmin, xmax) = c.bounds[xx];
            let k = &c.kk[xx * c.ksize..xx * c.ksize + xmax];
            let mut ss = [1i32 << (PRECISION_BITS - 1); 3];
            for (x, &kx) in k.iter().enumerate() {
                let p = (x + xmin) * 3;
                ss[0] += i32::from(line_in[p]) * kx;
                ss[1] += i32::from(line_in[p + 1]) * kx;
                ss[2] += i32::from(line_in[p + 2]) * kx;
            }
            *px = [clip8(ss[0]), clip8(ss[1]), clip8(ss[2])];
        }
    }
    Rgb {
        width: out_w,
        height: rows as u32,
        data,
    }
}

/// 垂直趟：`bounds` 中的行号已相对 `src` 的第 0 行。
fn vertical(src: &Rgb, out_h: u32, c: &Coeffs) -> Rgb {
    let stride = src.width as usize * 3;
    let mut data = vec![0u8; out_h as usize * stride];
    for (yy, line_out) in data.chunks_exact_mut(stride).enumerate() {
        let (ymin, ymax) = c.bounds[yy];
        let k = &c.kk[yy * c.ksize..yy * c.ksize + ymax];
        for (i, out) in line_out.iter_mut().enumerate() {
            let mut ss = 1i32 << (PRECISION_BITS - 1);
            for (y, &ky) in k.iter().enumerate() {
                ss += i32::from(src.data[(y + ymin) * stride + i]) * ky;
            }
            *out = clip8(ss);
        }
    }
    Rgb {
        width: src.width,
        height: out_h,
        data,
    }
}

/// 把 `img` 双三次缩放到 `width × height`，结果与 Pillow `resize((width, height), BICUBIC)` 逐字节相同。
/// `img.data` 的长度与尺寸不符时返回错误。
pub(crate) fn resize_bicubic(img: &Rgb, width: u32, height: u32) -> Result<Rgb, Error> {
    if img.data.len() as u64 != u64::from(img.width) * u64::from(img.height) * 3 {
        return Err(Error::InvalidPixels {
            width: img.width,
            height: img.height,
        });
    }
    if width == 0 || height == 0 {
        return Ok(Rgb {
            width,
            height,
            data: Vec::new(),
        });
    }
    let need_horizontal = width != img.width;
    let need_vertical = height != img.height;
    let mut vert = precompute_coeffs(img.height, height);
    // 垂直趟用到的第一行和最后一行之后的位置。
    let first = vert.bounds[0].0;
    let last = vert.bounds[height as usize - 1].0 + vert.bounds[height as usize - 1].1;

    let temp = need_horizontal.then(|| {
        for b in &mut vert.bounds {
            b.0 -= first;
        }
        horizontal(
            img,
            first,
            last - first,
            width,
            &precompute_coeffs(img.width, width),
        )
    });
    let src = temp.as_ref().unwrap_or(img);
    if need_vertical {
        return Ok(vertical(src, height, &vert));
    }
    Ok(temp.unwrap_or_else(|| img.clone()))
}
