//! 音频的下混与重采样（纯 Rust）：Android 不用 ffmpeg，由这里完成 ffmpeg `-ac 1 -ar 16000` 在电脑上做的事，结果与之接近而不逐位相同。
//!
//! - 下混系数取 ffmpeg 9.0（swresample，输出 f32）的实测值：前左、前右各 1/√2，前中 1，低音 0，其余（后置、侧置等）各 0.5；
//!   不归一化，和可能超过 1，ffmpeg 输出 f32 时同样不截断。各声道是什么由 Android 的声道掩码（`KEY_CHANNEL_MASK`）给出，
//!   交错顺序为掩码中的位从低到高；没有掩码或位数与声道数不符时按声道数取 Android 的默认掩码
//!   （3 = 2.1 即左右加低音，4 = 左右加两个后置，5 = 再加前中，6 = 5.1，7 = 6.1，8 = 7.1）。
//! - 重采样为 Kaiser 窗 sinc 多相滤波，参数取 swresample 的默认值：滤波器长度 32、截止频率 0.97、beta 9。
//!   降采样时截止频率按采样率之比降低、滤波器按比例加长，系数按相位归一化（直流增益为 1）。相位数为两采样率约分后的输出率，
//!   超过 `MAX_PHASES` 时取最近的等分相位（swresample 在这种情况下做线性插值）。首尾按镜像补齐。
//!   输出采样数为 ⌈输入 × 输出率 / 输入率⌉，与 ffmpeg 可能差 1 个。
//! - 原采样率等于目标采样率时不重采样（与 ffmpeg 相同），16 位输入的单声道采样与 ffmpeg 逐位相同。
//! - 解码是流式的（`push` 可多次调用），只保留尚未用完的输入，超过时长上限时可以随时停止。

use std::f64::consts::{FRAC_1_SQRT_2, PI};

const FILTER_SIZE: f64 = 32.0;
const CUTOFF: f64 = 0.97;
const KAISER_BETA: f64 = 9.0;
/// 相位表的上限，与 swresample 默认的 `phase_shift` 10 相同
const MAX_PHASES: u64 = 1024;
/// 已用完的输入超过这么多采样时才从缓冲区前端移除，避免每次都搬动
const DRAIN_AT: usize = 1 << 16;

/// Android 声道掩码的位（`AudioFormat.CHANNEL_OUT_*`）
const FL: u32 = 0x4;
const FR: u32 = 0x8;
const FC: u32 = 0x10;
const LFE: u32 = 0x20;
const BL: u32 = 0x40;
const BR: u32 = 0x80;
const BC: u32 = 0x400;
const SL: u32 = 0x800;
const SR: u32 = 0x1000;
const LFE2: u32 = 0x200_0000;

/// 各声道的下混系数，`mask` 为解码器给出的声道掩码（见模块说明）
pub(crate) fn coefficients(mask: Option<i32>, channels: usize) -> Vec<f32> {
    if channels == 1 {
        return vec![1.0];
    }
    let mask = mask
        .map(|m| m as u32)
        .filter(|m| m.count_ones() as usize == channels)
        .unwrap_or(match channels {
            2 => FL | FR,
            3 => FL | FR | LFE,
            4 => FL | FR | BL | BR,
            5 => FL | FR | FC | BL | BR,
            6 => FL | FR | FC | LFE | BL | BR,
            7 => FL | FR | FC | LFE | BL | BR | BC,
            _ => FL | FR | FC | LFE | BL | BR | SL | SR,
        });
    let mut coef: Vec<f32> = (0..32)
        .map(|i| 1u32 << i)
        .filter(|bit| mask & bit != 0)
        .map(|bit| match bit {
            FL | FR => FRAC_1_SQRT_2 as f32,
            FC => 1.0,
            LFE | LFE2 => 0.0,
            _ => 0.5,
        })
        .collect();
    // 超过 8 个声道而没有掩码时，多出的按其余声道算
    coef.resize(channels, 0.5);
    coef
}

/// 交错存放的多声道采样按 `coef`（每声道一个，见 `coefficients`）下混为单声道，追加到 `out`；不足一帧的尾部忽略
pub(crate) fn downmix(interleaved: &[f32], coef: &[f32], out: &mut Vec<f32>) {
    if coef.len() == 1 {
        out.extend_from_slice(interleaved);
        return;
    }
    out.extend(
        interleaved
            .chunks_exact(coef.len())
            .map(|f| f.iter().zip(coef).map(|(x, c)| x * c).sum::<f32>()),
    );
}

/// 第一类零阶修正贝塞尔函数，级数求和
fn bessel_i0(x: f64) -> f64 {
    let q = x * x / 4.0;
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > sum * 1e-17 {
        term *= q / (k * k);
        sum += term;
        k += 1.0;
    }
    sum
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// 单声道流式重采样器，见模块说明
pub(crate) struct Resampler {
    in_rate: u32,
    /// 输出第 k 个采样位于输入的 `k × num / den` 处
    num: u64,
    den: u64,
    taps: usize,
    center: usize,
    phases: u64,
    /// `phases` 行、每行 `taps` 个系数
    table: Vec<f32>,
    buf: Vec<f32>,
    /// `buf[0]` 对应的输入下标；开头镜像补齐的部分为负
    base: i64,
    /// 已输入的采样数
    len: u64,
    /// 下一个输出采样的序号
    next: u64,
    started: bool,
}

impl Resampler {
    pub(crate) fn new(in_rate: u32, out_rate: u32) -> Self {
        let (a, b) = (u64::from(in_rate.max(1)), u64::from(out_rate.max(1)));
        let g = gcd(a, b);
        let (num, den) = (a / g, b / g);
        let factor = (b as f64 * CUTOFF / a as f64).min(1.0);
        let taps = ((FILTER_SIZE / factor).ceil() as usize).max(1);
        let center = (taps - 1) / 2;
        let phases = den.min(MAX_PHASES);
        let mut table = Vec::with_capacity(phases as usize * taps);
        if num != den {
            for ph in 0..phases {
                let frac = ph as f64 / phases as f64;
                let row: Vec<f64> = (0..taps)
                    .map(|i| {
                        let x = PI * ((i as f64 - center as f64) - frac) * factor;
                        let y = if x == 0.0 { 1.0 } else { x.sin() / x };
                        let w = 2.0 * x / (factor * taps as f64 * PI);
                        y * bessel_i0(KAISER_BETA * (1.0 - w * w).max(0.0).sqrt())
                    })
                    .collect();
                let norm: f64 = row.iter().sum();
                table.extend(row.iter().map(|v| (v / norm) as f32));
            }
        }
        Self {
            in_rate,
            num,
            den,
            taps,
            center,
            phases,
            table,
            buf: Vec::new(),
            base: 0,
            len: 0,
            next: 0,
            started: false,
        }
    }

    pub(crate) fn in_rate(&self) -> u32 {
        self.in_rate
    }

    /// 送入一段单声道采样，算得出的输出追加到 `out`
    pub(crate) fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.num == self.den {
            out.extend_from_slice(input);
            return;
        }
        self.buf.extend_from_slice(input);
        self.len += input.len() as u64;
        if !self.started {
            if self.buf.len() <= self.center {
                return;
            }
            self.mirror_front();
        }
        self.emit(out, None);
    }

    /// 输入结束：末尾镜像补齐，输出剩下的采样
    pub(crate) fn finish(&mut self, out: &mut Vec<f32>) {
        if self.num == self.den || self.buf.is_empty() {
            return;
        }
        if !self.started {
            self.mirror_front();
        }
        // 与 swresample 的 `resample_flush` 相同：第 N + j 个取第 N − 1 − j 个（输入太短时取第一个）
        let last = self.buf.len() - 1;
        for j in 0..self.taps - 1 {
            let v = self.buf[last - j.min(last)];
            self.buf.push(v);
        }
        let total = (self.len * self.den).div_ceil(self.num);
        self.emit(out, Some(total));
    }

    /// 开头补 `center` 个镜像采样：第 -j 个取第 j 个（输入太短时取最后一个）
    fn mirror_front(&mut self) {
        let n = self.buf.len() - 1;
        let front: Vec<f32> = (1..=self.center)
            .rev()
            .map(|j| self.buf[j.min(n)])
            .collect();
        self.buf.splice(0..0, front);
        self.base = -(self.center as i64);
        self.started = true;
    }

    fn emit(&mut self, out: &mut Vec<f32>, total: Option<u64>) {
        let avail = self.base + self.buf.len() as i64;
        let mut start = 0usize;
        while total.is_none_or(|t| self.next < t) {
            let pos = self.next * self.num;
            let (mut q, r) = (pos / self.den, pos % self.den);
            let mut ph = if self.phases == self.den {
                r
            } else {
                (r * self.phases + self.den / 2) / self.den
            };
            if ph == self.phases {
                q += 1;
                ph = 0;
            }
            let s = q as i64 - self.center as i64;
            if s + self.taps as i64 > avail {
                break;
            }
            start = (s - self.base) as usize;
            let row = &self.table[ph as usize * self.taps..][..self.taps];
            let src = &self.buf[start..start + self.taps];
            out.push(src.iter().zip(row).map(|(x, c)| x * c).sum());
            self.next += 1;
        }
        // 末尾至少留一个滤波器长度，供结束时镜像补齐
        let drop = start.min(self.buf.len().saturating_sub(self.taps));
        if drop >= DRAIN_AT {
            self.buf.drain(..drop);
            self.base += drop as i64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resample(x: &[f32], from: u32, to: u32) -> Vec<f32> {
        let mut r = Resampler::new(from, to);
        let mut out = Vec::new();
        r.push(x, &mut out);
        r.finish(&mut out);
        out
    }

    fn sine(rate: u32, hz: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (0.5 * (2.0 * PI * hz * i as f64 / f64::from(rate)).sin()) as f32)
            .collect()
    }

    #[test]
    fn downmix_matches_ffmpeg_coefficients() {
        let s = FRAC_1_SQRT_2 as f32;
        let stereo = coefficients(None, 2);
        assert_eq!(stereo, [s, s]);
        let mut out = Vec::new();
        downmix(&[0.5, 0.25, 1.0, 0.0], &stereo, &mut out);
        assert_eq!(out, [0.75 * s, s]);
        out.clear();
        // 5.1：低音不计入
        downmix(&[1.0; 6], &coefficients(None, 6), &mut out);
        assert!((out[0] - (2.0 * s + 2.0)).abs() < 1e-6);
        // 没有掩码的 3 声道是 2.1（左右加低音）
        assert_eq!(coefficients(None, 3), [s, s, 0.0]);
        // AAC 4 声道：左右、前中、后中（掩码 FL | FR | FC | BC）
        assert_eq!(coefficients(Some(0x41c), 4), [s, s, 1.0, 0.5]);
        // 掩码的位数与声道数不符时按默认掩码
        assert_eq!(coefficients(Some(0x41c), 3), [s, s, 0.0]);
        // 7.1 的侧置
        assert_eq!(coefficients(None, 8), [s, s, 1.0, 0.0, 0.5, 0.5, 0.5, 0.5]);
        assert_eq!(coefficients(Some(0x0c), 1), [1.0]);
        out.clear();
        // 单声道原样；不足一帧的尾部忽略
        downmix(&[0.1, 0.2, 0.3], &[1.0], &mut out);
        downmix(&[0.1, 0.2, 0.3], &stereo, &mut out);
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn same_rate_is_copy() {
        assert_eq!(Resampler::new(44_100, 16_000).in_rate(), 44_100);
        let x = sine(16_000, 440.0, 1000);
        assert_eq!(resample(&x, 16_000, 16_000), x);
    }

    /// 直流不变，长度为 ⌈n × 16000 / 原采样率⌉
    #[test]
    fn keeps_dc_and_length() {
        for (rate, n) in [
            (44_100, 132_307),
            (48_000, 47_999),
            (8000, 8001),
            (22_050, 100),
            (44_100, 3),
        ] {
            let out = resample(&vec![0.5; n], rate, 16_000);
            assert_eq!(
                out.len() as u64,
                (n as u64 * 16_000).div_ceil(u64::from(rate)),
                "{rate}"
            );
            assert!(out.iter().all(|v| (v - 0.5).abs() < 1e-4), "{rate}");
        }
        assert!(resample(&[], 44_100, 16_000).is_empty());
    }

    /// 通带内的正弦与 16 kHz 上的理论值一致，截止频率以上的被滤掉
    #[test]
    fn passband_and_stopband() {
        for rate in [44_100, 48_000, 22_050, 8000, 44_056] {
            let n = rate as usize * 2;
            let out = resample(&sine(rate, 1000.0, n), rate, 16_000);
            let want = sine(16_000, 1000.0, out.len());
            // 去掉首尾各 10 毫秒（镜像补齐处）
            let err = out[160..out.len() - 160]
                .iter()
                .zip(&want[160..])
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(err < 2e-3, "{rate}：最大误差 {err}");
        }
        let out = resample(&sine(44_100, 12_000.0, 44_100), 44_100, 16_000);
        let rms = (out[160..out.len() - 160].iter().map(|v| v * v).sum::<f32>()
            / (out.len() - 320) as f32)
            .sqrt();
        assert!(rms < 1e-3, "{rms}");
    }

    /// 分块送入与一次送入的结果逐位相同
    #[test]
    fn chunked_equals_whole() {
        let x: Vec<f32> = (0..300_000u64)
            .map(|i| ((i * 7919) % 2001) as f32 / 1000.0 - 1.0)
            .collect();
        let whole = resample(&x, 44_100, 16_000);
        let mut r = Resampler::new(44_100, 16_000);
        let mut out = Vec::new();
        for c in x.chunks(1153) {
            r.push(c, &mut out);
        }
        r.finish(&mut out);
        assert_eq!(out.len(), whole.len());
        assert!(
            out.iter()
                .zip(&whole)
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
        // 第一块比滤波器的一半还短
        let mut r = Resampler::new(48_000, 16_000);
        let mut out = Vec::new();
        for c in x[..5000].chunks(3) {
            r.push(c, &mut out);
        }
        r.finish(&mut out);
        let whole = resample(&x[..5000], 48_000, 16_000);
        assert!(
            out.iter()
                .zip(&whole)
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
        assert_eq!(out.len(), whole.len());
    }

    /// 与 ffmpeg 对照（仅本机，需要 `scripts/fetch-ffmpeg.ps1`）：黄金音频转成多种采样率的立体声 16 位 WAV，
    /// 分别用 ffmpeg 直接转 16 kHz 单声道、和 ffmpeg 只解码为原采样率 PCM 再由这里下混与重采样，比较两者（另加白噪声）。
    #[test]
    #[ignore]
    fn compare_with_ffmpeg() {
        let Some(dir) = crate::ffmpeg::tests::ffmpeg_dir() else {
            return;
        };
        let exe = dir.join("ffmpeg.exe");
        let golden =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/golden/audio");
        let tmp = std::env::temp_dir().join(format!("rummage-resample-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let ffmpeg = |args: &[&str]| -> Vec<f32> {
            let out = std::process::Command::new(&exe)
                .args(["-v", "error", "-y"])
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            out.stdout
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| f32::from_le_bytes(*c))
                .collect()
        };
        let mut worst = 1.0f64;
        for name in ["aud01_zh.wav", "aud07_en.wav", "aud09_env.wav", "noise"] {
            for rate in [44_100u32, 48_000, 22_050, 8000] {
                let src = tmp.join(format!("{rate}-{name}.wav"));
                let src_s = src.to_str().unwrap();
                // 两个声道不同：右声道减弱并延迟 7 毫秒
                let filter = format!("aresample={rate},pan=stereo|c0=c0|c1=0.6*c0,adelay=0|7");
                let input = golden.join(name);
                let input = input.to_str().unwrap();
                // 另加满频带的白噪声
                let (fmt, input) = match name {
                    "noise" => ("lavfi", "anoisesrc=r=48000:a=0.3:d=5"),
                    _ => ("wav", input),
                };
                ffmpeg(&[
                    "-f",
                    fmt,
                    "-i",
                    input,
                    "-af",
                    &filter,
                    "-c:a",
                    "pcm_s16le",
                    src_s,
                ]);
                let want = ffmpeg(&["-i", src_s, "-ac", "1", "-ar", "16000", "-f", "f32le", "-"]);
                let pcm = ffmpeg(&["-i", src_s, "-f", "f32le", "-"]);
                let mut mono = Vec::new();
                downmix(&pcm, &coefficients(None, 2), &mut mono);
                let got = resample(&mono, rate, 16_000);
                let n = got.len().min(want.len());
                let (mut dot, mut ga, mut wa, mut max) = (0.0f64, 0.0f64, 0.0f64, 0.0f32);
                for (a, b) in got[..n].iter().zip(&want[..n]) {
                    dot += f64::from(*a) * f64::from(*b);
                    ga += f64::from(*a) * f64::from(*a);
                    wa += f64::from(*b) * f64::from(*b);
                    max = max.max((a - b).abs());
                }
                let cos = dot / (ga.sqrt() * wa.sqrt());
                worst = worst.min(cos);
                println!(
                    "{name} {rate} Hz：余弦 {cos:.8}，最大误差 {max:.6}，长度 {} / ffmpeg {}",
                    got.len(),
                    want.len()
                );
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(worst >= 0.999, "最低余弦 {worst}");
    }
}
