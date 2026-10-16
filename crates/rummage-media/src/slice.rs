//! 长图切片的几何：长边超过短边的阈值倍时，沿长边切成接近正方形的若干段，每段单独算向量。
//! - 段长取短边；段数取让相邻段至少重叠 `OVERLAP` 所需的最少段数，各段起点在 `[0, 长边 - 段长]` 上均匀分布，
//!   最后一段与末端对齐，不会留下很窄的尾段（实际重叠因此可能略大于 `OVERLAP`）。
//! - 所需段数超过上限时只切上限段，段长按比例增大，使这些段在同样的重叠下正好覆盖长边。
//! - 区域为原图（已按 EXIF 方向转正）的像素坐标。修改这里的规则时提升 `rummage_embed::space::IMAGE_SLICE`。
use crate::Rgb;

/// 相邻段的最小重叠（占段长的比例）：10% 让切在边界上的一行文字完整落在其中一段
const OVERLAP: f64 = 0.1;

/// 原图中的像素区域
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// 长边超过短边的 `max_ratio` 倍时返回各段区域（从上到下或从左到右），否则为空。`max_slices` 至少按 2 计
pub fn slices(width: u32, height: u32, max_ratio: f64, max_slices: u32) -> Vec<Region> {
    let (long, short) = (width.max(height), width.min(height));
    if short == 0 || f64::from(long) <= max_ratio * f64::from(short) {
        return Vec::new();
    }
    let step = short - (f64::from(short) * OVERLAP).round() as u32;
    let need = (long - short).div_ceil(step) + 1;
    let max = max_slices.max(2);
    let (n, len) = if need <= max {
        (need, short)
    } else {
        // n 段、相邻重叠 OVERLAP 时覆盖的长度为 len × (1 + (n - 1)(1 - OVERLAP))
        let len = f64::from(long) / (1.0 + f64::from(max - 1) * (1.0 - OVERLAP));
        (max, (len.ceil() as u32).min(long))
    };
    (0..n)
        .map(|i| {
            let start = (u64::from(long - len) * u64::from(i) / u64::from(n - 1)) as u32;
            if width >= height {
                Region {
                    x: start,
                    y: 0,
                    width: len,
                    height,
                }
            } else {
                Region {
                    x: 0,
                    y: start,
                    width,
                    height: len,
                }
            }
        })
        .collect()
}

/// 取出区域 `r` 的像素；`r` 须在图内（`slices` 给出的区域总是如此）
pub fn crop(img: &Rgb, r: Region) -> Rgb {
    let row = r.width as usize * 3;
    let mut data = Vec::with_capacity(row * r.height as usize);
    for y in r.y..r.y + r.height {
        let start = (y as usize * img.width as usize + r.x as usize) * 3;
        data.extend_from_slice(&img.data[start..start + row]);
    }
    Rgb {
        width: r.width,
        height: r.height,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 沿长边的 (起点, 段长)
    fn spans(width: u32, height: u32, max_ratio: f64, max_slices: u32) -> Vec<(u32, u32)> {
        slices(width, height, max_ratio, max_slices)
            .iter()
            .map(|r| {
                if width >= height {
                    assert_eq!((r.y, r.height), (0, height));
                    (r.x, r.width)
                } else {
                    assert_eq!((r.x, r.width), (0, width));
                    (r.y, r.height)
                }
            })
            .collect()
    }

    #[test]
    fn threshold_boundary() {
        // 正好等于阈值不切，超过才切
        assert!(slices(1000, 2500, 2.5, 12).is_empty());
        assert_eq!(slices(1000, 2501, 2.5, 12).len(), 3);
        assert!(slices(1980, 852, 2.5, 12).is_empty());
        assert!(slices(1000, 1000, 2.5, 12).is_empty());
        assert!(slices(0, 5000, 2.5, 12).is_empty());
    }

    #[test]
    fn vertical_and_horizontal_agree() {
        // 1080×3000：步长 1080 - 108 = 972，需要 ceil(1920 / 972) + 1 = 3 段，起点均匀分布、末段对齐
        let tall = spans(1080, 3000, 2.5, 12);
        assert_eq!(tall, [(0, 1080), (960, 1080), (1920, 1080)]);
        assert_eq!(spans(3000, 1080, 2.5, 12), tall);
        let wide = slices(3000, 1080, 2.5, 12);
        assert_eq!(
            wide[1],
            Region {
                x: 960,
                y: 0,
                width: 1080,
                height: 1080
            }
        );
    }

    #[test]
    fn overlap_and_tail() {
        for (w, h) in [
            (1080, 2701),
            (1080, 5000),
            (720, 9999),
            (1170, 25000),
            (3, 10),
        ] {
            let s = spans(w, h, 2.5, 12);
            assert!(s.len() >= 2, "{w}×{h}");
            assert_eq!(s[0].0, 0, "{w}×{h}");
            // 末段与末端对齐，没有窄尾段：所有段等长
            let last = s.last().unwrap();
            assert_eq!(last.0 + last.1, h, "{w}×{h}");
            assert!(s.iter().all(|p| p.1 == s[0].1), "{w}×{h}");
            for pair in s.windows(2) {
                let overlap = pair[0].0 + pair[0].1 - pair[1].0;
                assert!(
                    f64::from(overlap) >= f64::from(pair[0].1) * OVERLAP - 1.0,
                    "{w}×{h}：{pair:?}"
                );
                assert!(pair[1].0 > pair[0].0, "{w}×{h}");
            }
        }
    }

    #[test]
    fn slice_count_capped() {
        // 1080×30000 需要 30 段，上限 12：段长增大到 30000 / (1 + 11 × 0.9) 向上取整
        let s = spans(1080, 30000, 2.5, 12);
        assert_eq!(s.len(), 12);
        assert_eq!(s[0], (0, 2753));
        assert_eq!(s[11], (30000 - 2753, 2753));
        let overlap = s[0].1 - s[1].0;
        assert!((270..=280).contains(&overlap), "{s:?}");
        // 上限不足 2 时按 2 计
        assert_eq!(spans(100, 1000, 2.5, 1).len(), 2);
        // 极细的图
        let s = spans(1, 100_000, 2.5, 12);
        assert_eq!(s.len(), 12);
        assert_eq!(s[11].0 + s[11].1, 100_000);
    }

    #[test]
    fn crop_copies_rows() {
        // 4×3，红色通道为像素序号
        let img = Rgb {
            width: 4,
            height: 3,
            data: (0..12u8).flat_map(|i| [i, 0, 0]).collect(),
        };
        let out = crop(
            &img,
            Region {
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            },
        );
        assert_eq!((out.width, out.height), (2, 2));
        let red: Vec<u8> = out.data.chunks(3).map(|p| p[0]).collect();
        assert_eq!(red, [5, 6, 9, 10]);
    }
}
