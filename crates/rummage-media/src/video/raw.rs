//! 系统解码器（Android 的 MediaCodec）给出的原始帧：按抽帧网格取帧、YUV 转 RGB、按旋转角度转正。
//! 平台无关，Android 与测试时编译；测试中与 ffmpeg 的结果对照（`video::raw::tests::raw_matches_ffmpeg`）。
//!
//! - 取帧（`Picker`）与 ffmpeg 的 `fps=fps=<网格>:start_time=0:round=up` 相同：网格第 m 帧（时刻 T）取 pts ≤ T 的最后一帧，
//!   开头缺帧时重复第一帧。帧按解码器输出的顺序到来，pts 不必递增：与 fps 滤镜一样，新到的帧 pts 不超过下一个网格时刻时
//!   顶替保留的帧，超过时先把保留的帧输出到它之前的各网格时刻。
//!   视频末尾记为最后一帧的 pts 加一帧的时长（最后两帧的间隔，只有一帧时按容器给的帧率），末尾之前的网格时刻都输出，
//!   与 ffmpeg 在输入结束时按“最后一帧 pts 加时长”补齐的帧数相同。pts 取整到微秒，末尾正好落在网格上时
//!   （如 30 fps 的 4 秒视频，网格 2 秒）算出的末尾可能多出一两微秒，所以末尾前 `END_SLACK_US` 以内的网格时刻不算。
//!   每帧要输出的次数按 pts 直接算出（不逐格累加），pts 异常大时立即返回一个很大的次数，由调用方按帧数上限报错。
//!   pts 从视频开头算起：ffmpeg 减去容器的 start_time，Android 减去视频轨第一个样本的时间，
//!   音轨比画面先开始的文件两者网格可能差一帧。
//! - YUV 转 RGB（`yuv_to_rgb`）：三个平面各有行距与像素间距，NV12、NV21、I420 都按同一种方式读；色度取所在 2×2 块的样本。
//!   矩阵与范围读不到时用 BT.601、有限范围：ffmpeg 9.0 对没有标注的视频不论分辨率都这样转换（实测 1920×1080 与 854×480）。
//! - 旋转（`rotate`）：顺时针 90、180、270 度，其他角度不转。

use super::Grid;
use crate::Rgb;

/// 见模块说明
const END_SLACK_US: i64 = 5;

/// 按网格取帧，见模块说明
pub(crate) struct Picker {
    grid: Grid,
    /// 下一个网格帧的序号
    next: u64,
    last: Option<i64>,
    prev: Option<i64>,
    /// 一帧的时长（微秒），只有一帧时用来估计末尾
    frame_us: i64,
}

impl Picker {
    pub(crate) fn new(grid: Grid, frame_us: i64) -> Self {
        Self {
            grid,
            next: 0,
            last: None,
            prev: None,
            frame_us,
        }
    }

    /// 网格时刻早于 `us`（微秒）的网格帧数，即 ceil(us × ticks / (spacing × 10⁶))，负数为 0
    fn count_before(&self, us: i64) -> u64 {
        let num = i128::from(us) * i128::from(self.grid.ticks);
        let den = i128::from(self.grid.spacing) * 1_000_000;
        if num <= 0 || den <= 0 {
            return 0;
        }
        u64::try_from((num + den - 1) / den).unwrap_or(u64::MAX)
    }

    /// 网格推进到早于 `us` 的网格帧之后，返回推进了几格
    fn advance(&mut self, us: i64) -> u64 {
        let first = self.next;
        self.next = self.next.max(self.count_before(us));
        self.next - first
    }

    /// 收到 pts 为 `us`（微秒）的一帧，返回保留的上一帧要输出几次；还没有上一帧时，这几次输出的是这一帧（开头缺帧）。
    /// 之后这一帧成为保留的帧
    pub(crate) fn push(&mut self, us: i64) -> u64 {
        let n = self.advance(us);
        self.prev = self.last;
        self.last = Some(us);
        n
    }

    /// 输入结束，返回保留的帧还要输出几次；收到过帧时总共至少输出一帧
    pub(crate) fn finish(&mut self) -> u64 {
        let Some(last) = self.last else {
            return 0;
        };
        let dur = self
            .prev
            .map(|p| last.saturating_sub(p))
            .filter(|d| *d > 0)
            .unwrap_or(self.frame_us);
        let mut n = self.advance(last.saturating_add(dur).saturating_sub(END_SLACK_US));
        if self.next == 0 {
            self.next = 1;
            n = 1;
        }
        n
    }
}

/// YUV 转 RGB 的矩阵与范围
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Matrix {
    kr: f64,
    kb: f64,
    full: bool,
}

pub(crate) const BT601: (f64, f64) = (0.299, 0.114);
pub(crate) const BT709: (f64, f64) = (0.2126, 0.0722);
pub(crate) const BT2020: (f64, f64) = (0.2627, 0.0593);

impl Matrix {
    pub(crate) fn new((kr, kb): (f64, f64), full: bool) -> Self {
        Self { kr, kb, full }
    }

    /// MediaFormat 的 `color-standard` 与 `color-range`（取值见 `android.media.MediaFormat`）；读不到时同 ffmpeg：BT.601、有限范围
    pub(crate) fn android(standard: Option<i32>, range: Option<i32>) -> Self {
        let k = match standard {
            Some(1) => BT709,
            Some(6) => BT2020,
            _ => BT601,
        };
        Self::new(k, range == Some(1))
    }
}

/// 一个平面：数据、行距、像素间距（字节）
#[derive(Clone, Copy)]
pub(crate) struct Plane<'a> {
    pub data: &'a [u8],
    pub row: usize,
    pub pixel: usize,
}

impl Plane<'_> {
    /// 第 `y` 行第 `x` 个样本在范围内
    fn holds(&self, x: usize, y: usize) -> bool {
        self.pixel > 0 && y * self.row + x * self.pixel < self.data.len()
    }

    fn at(&self, x: usize, y: usize) -> i32 {
        i32::from(self.data[y * self.row + x * self.pixel])
    }
}

/// 从 `(x0, y0)` 起取 `w × h` 的 YUV 4:2:0 画面转成 RGB；平面数据不够长时返回 `None`
pub(crate) fn yuv_to_rgb(
    y: Plane,
    u: Plane,
    v: Plane,
    (x0, y0, w, h): (u32, u32, u32, u32),
    m: Matrix,
) -> Option<Rgb> {
    let (x0, y0, w, h) = (x0 as usize, y0 as usize, w as usize, h as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let (x1, y1) = (x0 + w - 1, y0 + h - 1);
    if !(y.holds(x1, y1) && u.holds(x1 / 2, y1 / 2) && v.holds(x1 / 2, y1 / 2)) {
        return None;
    }
    // 定点系数（× 65536）
    let (ys, cs, yoff) = if m.full {
        (1.0, 1.0, 0)
    } else {
        (255.0 / 219.0, 255.0 / 224.0, 16)
    };
    let kg = 1.0 - m.kr - m.kb;
    let fix = |x: f64| (x * 65536.0).round() as i32;
    let cy = fix(ys);
    let rv = fix(2.0 * (1.0 - m.kr) * cs);
    let gu = fix(2.0 * m.kb * (1.0 - m.kb) / kg * cs);
    let gv = fix(2.0 * m.kr * (1.0 - m.kr) / kg * cs);
    let bu = fix(2.0 * (1.0 - m.kb) * cs);
    let clip = |x: i32| ((x + 32768) >> 16).clamp(0, 255) as u8;
    let mut data = Vec::with_capacity(w * h * 3);
    for py in y0..=y1 {
        for px in x0..=x1 {
            let l = cy * (y.at(px, py) - yoff);
            let cb = u.at(px / 2, py / 2) - 128;
            let cr = v.at(px / 2, py / 2) - 128;
            data.push(clip(l + rv * cr));
            data.push(clip(l - gu * cb - gv * cr));
            data.push(clip(l + bu * cb));
        }
    }
    Some(Rgb {
        width: w as u32,
        height: h as u32,
        data,
    })
}

/// 顺时针旋转 `degrees` 度（90、180、270），其他角度原样返回
pub(crate) fn rotate(img: Rgb, degrees: i32) -> Rgb {
    let deg = degrees.rem_euclid(360);
    if ![90, 180, 270].contains(&deg) {
        return img;
    }
    let (w, h) = (img.width as usize, img.height as usize);
    let (ow, oh) = if deg == 180 { (w, h) } else { (h, w) };
    let mut data = vec![0u8; img.data.len()];
    for oy in 0..oh {
        for ox in 0..ow {
            let (sx, sy) = match deg {
                90 => (oy, h - 1 - ox),
                180 => (w - 1 - ox, h - 1 - oy),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 依次送入各帧的 pts（毫秒），网格每 `grid_ms` 毫秒一帧，返回每个网格帧取的是第几帧
    fn pick(grid_ms: u64, pts: &[i64], frame_ms: i64) -> Vec<usize> {
        let grid = Grid {
            ticks: 1000,
            spacing: grid_ms,
        };
        let mut p = Picker::new(grid, frame_ms * 1000);
        let mut out = Vec::new();
        let mut held = None;
        for (i, &t) in pts.iter().enumerate() {
            let n = p.push(t * 1000) as usize;
            out.extend(std::iter::repeat_n(held.unwrap_or(i), n));
            held = Some(i);
        }
        out.extend(std::iter::repeat_n(held.unwrap_or(0), p.finish() as usize));
        out
    }

    #[test]
    fn picks_last_frame_not_after_grid() {
        // 10 fps、1 秒，网格 0.5 秒：取 0、0.5 秒处的帧；末尾 1.0 秒之前共 2 帧
        let pts: Vec<i64> = (0..10).map(|i| i * 100).collect();
        assert_eq!(pick(500, &pts, 100), [0, 5]);
        // pts 不在网格上时取之前最后一帧
        assert_eq!(pick(250, &[0, 120, 240, 360, 480, 600], 120), [0, 2, 4]);
    }

    #[test]
    fn missing_start_repeats_first() {
        // 第一帧在 0.3 秒：0、0.1、0.2 秒都取它
        assert_eq!(pick(100, &[300, 400, 500], 100), [0, 0, 0, 0, 1, 2]);
    }

    #[test]
    fn low_frame_rate_repeats() {
        // 1 fps、网格 0.25 秒：每帧重复 4 次；3 帧的末尾在 3 秒
        let want = [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2];
        assert_eq!(pick(250, &[0, 1000, 2000], 1000), want);
    }

    #[test]
    fn irregular_pts() {
        // 间隔不均：末尾为 0.45 + 0.05 秒
        assert_eq!(pick(100, &[0, 50, 400, 450], 50), [0, 1, 1, 1, 2]);
        // 乱序：后到的 0.15 秒没有超过下一个网格时刻 0.2 秒，顶替保留的 0.2 秒；末尾按最后两帧的间隔
        assert_eq!(pick(100, &[0, 200, 150, 300], 100), [0, 0, 2, 3, 3]);
        // 只有一帧：按帧率估计末尾，至少一帧
        assert_eq!(pick(100, &[0], 250), [0, 0, 0]);
        assert_eq!(pick(1000, &[0], 0), [0]);
        assert!(pick(100, &[], 100).is_empty());
    }

    #[test]
    fn huge_pts_returns_at_once() {
        // pts 异常大：直接算出次数，不逐格累加；次数远超任何帧数上限，调用方返回 TooLong
        let grid = Grid {
            ticks: 1000,
            spacing: 100,
        };
        let mut p = Picker::new(grid, 0);
        assert_eq!(p.push(0), 0);
        let n = p.push(i64::MAX);
        assert!(n > 1_000_000_000_000, "{n}");
        assert_eq!(p.finish(), 0);
        // 负的 pts 不推进网格
        let mut p = Picker::new(grid, 0);
        assert_eq!(p.push(-5_000_000), 0);
        assert_eq!(p.push(0), 0);
    }

    fn plane(data: &[u8], row: usize, pixel: usize) -> Plane<'_> {
        Plane { data, row, pixel }
    }

    /// 4×2 的画面：亮度 0、30、…、210，左右两块色度不同
    const Y: [u8; 8] = [0, 30, 60, 90, 120, 150, 180, 210];
    const U: [u8; 2] = [90, 200];
    const V: [u8; 2] = [240, 60];

    fn convert(u: Plane, v: Plane, rect: (u32, u32, u32, u32)) -> Option<Rgb> {
        yuv_to_rgb(plane(&Y, 4, 1), u, v, rect, Matrix::android(None, None))
    }

    #[test]
    fn yuv_layouts_agree() {
        let all = (0, 0, 4, 2);
        let i420 = convert(plane(&U, 2, 1), plane(&V, 2, 1), all).unwrap();
        // NV12：UV 交错，行尾有对齐
        let nv12 = [U[0], V[0], U[1], V[1], 0, 0];
        let a = convert(plane(&nv12, 6, 2), plane(&nv12[1..], 6, 2), all).unwrap();
        // NV21：VU 交错
        let nv21 = [V[0], U[0], V[1], U[1]];
        let b = convert(plane(&nv21[1..], 4, 2), plane(&nv21, 4, 2), all).unwrap();
        assert_eq!(a, i420);
        assert_eq!(b, i420);
        // BT.601 有限范围：Y=30、U=90、V=240 → R = 1.164 × 14 + 1.596 × 112 ≈ 195，G、B 截到 0
        assert_eq!(&i420.data[3..6], &[195, 0, 0]);
        // 从 (2, 1) 起 2×1
        let part = convert(plane(&U, 2, 1), plane(&V, 2, 1), (2, 1, 2, 1)).unwrap();
        assert_eq!(part.data, i420.data[18..24]);
        // 平面不够长、尺寸为空
        assert!(convert(plane(&U[..1], 2, 1), plane(&V, 2, 1), all).is_none());
        assert!(convert(plane(&U, 2, 1), plane(&V, 2, 1), (0, 0, 0, 2)).is_none());
    }

    #[test]
    fn matrix_and_range() {
        let px = |y: u8, c: u8, m| {
            yuv_to_rgb(
                plane(&[y], 1, 1),
                plane(&[c], 1, 1),
                plane(&[c], 1, 1),
                (0, 0, 1, 1),
                m,
            )
            .unwrap()
            .data
        };
        // 有限范围的 235 是白，全范围不是
        assert_eq!(px(235, 128, Matrix::android(None, None)), [255, 255, 255]);
        assert_eq!(
            px(235, 128, Matrix::android(Some(1), Some(1))),
            [235, 235, 235]
        );
        assert_eq!(Matrix::android(Some(4), Some(2)), Matrix::new(BT601, false));
        let (a, b, c) = (
            px(128, 100, Matrix::android(None, None)),
            px(128, 100, Matrix::android(Some(1), None)),
            px(128, 100, Matrix::android(Some(6), None)),
        );
        assert!(a != b && b != c);
    }

    #[test]
    fn rotations() {
        // 3×2，像素值即序号：
        // 0 1 2   顺时针 90 度为  3 0
        // 3 4 5                   4 1
        //                         5 2
        let img = Rgb {
            width: 3,
            height: 2,
            data: (0..6).flat_map(|i| [i; 3]).collect(),
        };
        let px = |r: &Rgb| -> Vec<u8> { r.data.iter().step_by(3).copied().collect() };
        let r = rotate(img.clone(), 90);
        assert_eq!((r.width, r.height), (2, 3));
        assert_eq!(px(&r), [3, 0, 4, 1, 5, 2]);
        assert_eq!(px(&rotate(img.clone(), 180)), [5, 4, 3, 2, 1, 0]);
        let r = rotate(img.clone(), 270);
        assert_eq!((r.width, r.height), (2, 3));
        assert_eq!(px(&r), [2, 5, 1, 4, 0, 3]);
        assert_eq!(rotate(img.clone(), -90), r);
        assert_eq!(rotate(img.clone(), 0), img);
        assert_eq!(rotate(img.clone(), 45), img);
    }

    /// 对照：ffmpeg 解出的原始 YUV 帧与 pts（ffprobe）经这里的取帧、转换、旋转与 `resize_bicubic`，
    /// 与 `video::extract`（ffmpeg 的 `fps` + `scale`）比较。帧数必须相同，逐帧报告像素余弦与平均绝对误差。运行：
    /// `cargo test -p rummage-media -- --ignored --nocapture raw_matches_ffmpeg`
    #[test]
    #[ignore = "需要本机的 ffmpeg，生成约 30 MB 的临时视频"]
    fn raw_matches_ffmpeg() {
        use std::io::Read;
        use std::process::{Command, Stdio};
        use std::time::Duration;

        use crate::ffmpeg::tests::ffmpeg_dir;
        use crate::video::{WindowOpts, extract, frame_size, probe};

        let Some(dir) = ffmpeg_dir() else {
            return;
        };
        let (ffmpeg, ffprobe) = (dir.join("ffmpeg.exe"), dir.join("ffprobe.exe"));
        let tmp = std::env::temp_dir().join(format!("rummage-raw-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let run = |cmd: &mut Command| assert!(cmd.status().unwrap().success());
        // (名称, 尺寸, 帧率, 时长, 逆时针旋转角度（ffmpeg 的 display_rotation）, 色彩标注)；
        // 4 秒与 3.6 秒的末尾正好落在网格上
        let cases = [
            ("1080p", "1920x1080", 30, "4", 0, None),
            ("480p", "854x480", 25, "3.6", 0, None),
            ("1080p-709", "1920x1080", 30, "4.3", 0, Some("bt709")),
            ("竖拍", "1920x1080", 30, "4.3", -90, None),
            ("480p-旋转", "854x480", 24, "4.3", 90, None),
        ];
        let grids = [
            WindowOpts {
                window_s: 2,
                overlap_s: 0,
                frames: 2,
            },
            WindowOpts {
                window_s: 6,
                overlap_s: 0,
                frames: 6,
            },
        ];
        for (name, size, fps, secs, rot, tag) in cases {
            let plain = tmp.join(format!("{name}-src.mp4"));
            let src = tmp.join(format!("{name}.mp4"));
            let mut make = Command::new(&ffmpeg);
            make.args(["-v", "error", "-y", "-f", "lavfi", "-i"])
                .arg(format!("testsrc2=duration={secs}:size={size}:rate={fps}"))
                .args(["-pix_fmt", "yuv420p", "-c:v", "libopenh264", "-b:v", "6M"]);
            if let Some(t) = tag {
                make.args(["-colorspace", t]);
            }
            run(make.arg(&plain));
            run(Command::new(&ffmpeg)
                .args(["-v", "error", "-y", "-display_rotation:v:0"])
                .arg(rot.to_string())
                .arg("-i")
                .arg(&plain)
                .args(["-c", "copy"])
                .arg(&src));
            let info = probe(&dir, &src, Duration::from_secs(30)).unwrap();
            let out = frame_size(info.width, info.height, 140).unwrap();
            let (w, h): (u32, u32) = {
                let (a, b) = size.split_once('x').unwrap();
                (a.parse().unwrap(), b.parse().unwrap())
            };
            let matrix = Matrix::new(if tag == Some("bt709") { BT709 } else { BT601 }, false);
            // 顺时针角度；ffprobe 的 rotation 是逆时针
            let cw = i32::rem_euclid(-rot, 360);
            let pts: Vec<i64> = {
                let o = Command::new(&ffprobe)
                    .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
                    .args(["frame=best_effort_timestamp_time", "-of", "csv=p=0"])
                    .arg(&src)
                    .output()
                    .unwrap();
                String::from_utf8(o.stdout)
                    .unwrap()
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(|l| {
                        (l.trim().trim_end_matches(',').parse::<f64>().unwrap() * 1e6).round()
                            as i64
                    })
                    .collect()
            };
            for opts in grids {
                let grid = opts.grid();
                let want = extract(
                    &dir,
                    &src,
                    out,
                    grid,
                    100_000,
                    false,
                    None,
                    Duration::from_secs(120),
                )
                .unwrap();
                let mut child = Command::new(&ffmpeg)
                    .args(["-v", "error", "-noautorotate", "-i"])
                    .arg(&src)
                    .args([
                        "-map",
                        "0:V:0",
                        "-fps_mode",
                        "passthrough",
                        "-pix_fmt",
                        "yuv420p",
                    ])
                    .args(["-f", "rawvideo", "-"])
                    .stdout(Stdio::piped())
                    .spawn()
                    .unwrap();
                let mut stdout = child.stdout.take().unwrap();
                let (wu, hu) = (w as usize, h as usize);
                let mut buf = vec![0u8; wu * hu * 3 / 2];
                let mut picker = Picker::new(grid, 1_000_000 / fps);
                let mut held: Option<Vec<u8>> = None;
                let mut got: Vec<Rgb> = Vec::new();
                let emit = |yuv: &[u8], n: u64, got: &mut Vec<Rgb>| {
                    if n == 0 {
                        return;
                    }
                    let (y, c) = yuv.split_at(wu * hu);
                    let (u, v) = c.split_at(wu * hu / 4);
                    let rgb = yuv_to_rgb(
                        Plane {
                            data: y,
                            row: wu,
                            pixel: 1,
                        },
                        Plane {
                            data: u,
                            row: wu / 2,
                            pixel: 1,
                        },
                        Plane {
                            data: v,
                            row: wu / 2,
                            pixel: 1,
                        },
                        (0, 0, w, h),
                        matrix,
                    )
                    .unwrap();
                    let rgb = crate::resize_bicubic(&rotate(rgb, cw), out.0, out.1).unwrap();
                    got.extend(std::iter::repeat_n(rgb, n as usize));
                };
                let mut count = 0;
                while stdout.read_exact(&mut buf).is_ok() {
                    let n = picker.push(pts[count]);
                    emit(held.as_deref().unwrap_or(&buf), n, &mut got);
                    held = Some(buf.clone());
                    count += 1;
                }
                assert!(child.wait().unwrap().success());
                assert_eq!(count, pts.len());
                emit(held.as_deref().unwrap(), picker.finish(), &mut got);
                assert_eq!(got.len(), want.len(), "{name} 网格 {} 的帧数", grid.fps());
                let (mut min_cos, mut max_mae) = (1.0f64, 0.0f64);
                for (i, (g, r)) in got.iter().zip(&want).enumerate() {
                    let (mut dot, mut na, mut nb, mut abs) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
                    for (&a, &b) in g.data.iter().zip(&r.data) {
                        let (a, b) = (f64::from(a), f64::from(b));
                        dot += a * b;
                        na += a * a;
                        nb += b * b;
                        abs += (a - b).abs();
                    }
                    let cos = dot / (na.sqrt() * nb.sqrt());
                    let mae = abs / g.data.len() as f64;
                    println!(
                        "{name} 网格 {}：第 {i} 帧 余弦 {cos:.5}，平均绝对误差 {mae:.2}",
                        grid.fps()
                    );
                    min_cos = min_cos.min(cos);
                    max_mae = max_mae.max(mae);
                }
                println!(
                    "{name}（{}×{} → {}×{}）网格 {}：{} 帧，最低余弦 {min_cos:.5}，最大平均绝对误差 {max_mae:.2}",
                    info.width,
                    info.height,
                    out.0,
                    out.1,
                    grid.fps(),
                    got.len()
                );
                // 矩阵选错时平均误差在 8 以上
                assert!(
                    min_cos >= 0.99 && max_mae < 4.0,
                    "{name} 与 ffmpeg 差异过大"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
