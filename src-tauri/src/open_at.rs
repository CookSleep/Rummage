//! 打开到命中位置：系统没有统一的“打开到某时刻 / 某页”接口，只能看扩展名的默认程序
//! （Windows 用 `AssocQueryString`，见 `windows.rs`）是不是已知程序，是就按它的命令行参数启动，否则由调用方按普通方式打开。
//! 已知程序与参数：
//! - 音视频（毫秒）：VLC `--start-time=<秒>`、mpv `--start=<秒>`、PotPlayer `/seek=<时:分:秒>`、MPC-HC / MPC-BE `/start <毫秒>`；
//! - PDF（页码）：SumatraPDF `-page <页>`、Adobe Acrobat / Reader `/A "page=<页>"`、Microsoft Edge `<文件 URL>#page=<页>`。
//!
//! 按可执行文件名（不分大小写）识别。WPS Office、Windows 媒体播放器等没有公开的跳转参数，按普通方式打开。
//! 时间只用于音频与视频、页码只用于 PDF，前端传来的位置只是数字，不会变成程序名或任意参数。
use std::ffi::OsString;
use std::path::Path;

use serde::Deserialize;

/// 前端给出的命中位置，对应 `src/types.ts` 的 `OpenAt`：音视频为起点毫秒，PDF 为页码
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenAt {
    pub ms: Option<u64>,
    pub page: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Ms(u64),
    Page(u32),
}

/// 这种文件能跳转到的位置：音视频取时间，PDF 取页码（从 1 起），其他为空
pub fn target(kind: &str, ext: &str, at: OpenAt) -> Option<Target> {
    match (kind, ext) {
        ("audio" | "video", _) => at.ms.map(Target::Ms),
        (_, "pdf") => at.page.filter(|p| *p >= 1).map(Target::Page),
        _ => None,
    }
}

/// 默认程序为 `exe` 时打开 `file` 并跳到 `to` 的命令行参数；不是已知程序、或程序不支持这类位置时为空
pub fn args(exe: &Path, file: &Path, to: Target) -> Option<Vec<OsString>> {
    let name = exe.file_stem()?.to_str()?.to_ascii_lowercase();
    let file = file.as_os_str().to_os_string();
    let s = |v: String| OsString::from(v);
    Some(match (name.as_str(), to) {
        ("vlc", Target::Ms(ms)) => vec![s(format!("--start-time={}", secs(ms))), file],
        ("mpv", Target::Ms(ms)) => vec![s(format!("--start={}", secs(ms))), file],
        ("potplayermini64" | "potplayermini" | "potplayer64" | "potplayer", Target::Ms(ms)) => {
            let t = ms / 1000;
            let seek = format!("/seek={}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60);
            vec![file, s(seek)]
        }
        ("mpc-hc64" | "mpc-hc" | "mpc-be64" | "mpc-be", Target::Ms(ms)) => {
            vec![file, s("/start".into()), s(ms.to_string())]
        }
        ("sumatrapdf", Target::Page(p)) => vec![s("-page".into()), s(p.to_string()), file],
        ("acrobat" | "acrord32", Target::Page(p)) => {
            vec![s("/A".into()), s(format!("page={p}")), file]
        }
        ("msedge", Target::Page(p)) => vec![s(format!("{}#page={p}", file_url(Path::new(&file))))],
        _ => return None,
    })
}

/// 秒数，保留到毫秒
fn secs(ms: u64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

/// Windows 路径的 `file:` URL：分隔符换成 `/`，非 ASCII 字母数字与少数安全符号之外的字节按 UTF-8 百分号编码；
/// 网络路径（`\\服务器\共享`）写成 `file://服务器/共享`
fn file_url(path: &Path) -> String {
    let p = path.to_string_lossy().replace('\\', "/");
    let (prefix, rest) = match p.strip_prefix("//") {
        Some(unc) => ("file://", unc.to_string()),
        None => ("file:///", p),
    };
    let mut out = String::from(prefix);
    for b in rest.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(v: Option<Vec<OsString>>) -> Option<Vec<String>> {
        v.map(|v| {
            v.into_iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        })
    }

    #[test]
    fn known_programs() {
        let f = Path::new(r"D:\视频\a b.mp4");
        let at = |exe: &str, to| strs(args(Path::new(exe), f, to));
        let ms = Target::Ms(3_723_500);
        assert_eq!(
            at(r"C:\Program Files\VideoLAN\VLC\vlc.exe", ms).unwrap(),
            ["--start-time=3723.500", r"D:\视频\a b.mp4"]
        );
        assert_eq!(at("mpv.exe", ms).unwrap()[0], "--start=3723.500");
        assert_eq!(at("PotPlayerMini64.exe", ms).unwrap()[1], "/seek=1:02:03");
        assert_eq!(at("mpc-be64.exe", ms).unwrap()[1..], ["/start", "3723500"]);
        assert_eq!(
            at("SumatraPDF.exe", Target::Page(5)).unwrap()[..2],
            ["-page", "5"]
        );
        assert_eq!(
            at("AcroRd32.exe", Target::Page(5)).unwrap()[..2],
            ["/A", "page=5"]
        );
        // 程序不支持这类位置、或不是已知程序
        assert_eq!(at("vlc.exe", Target::Page(2)), None);
        assert_eq!(at("SumatraPDF.exe", ms), None);
        assert_eq!(at("wps.exe", Target::Page(2)), None);
        assert_eq!(at("Microsoft.Media.Player.exe", ms), None);
    }

    #[test]
    fn edge_file_url() {
        let a = strs(args(
            Path::new("msedge.exe"),
            Path::new(r"D:\资料\年报 #1.pdf"),
            Target::Page(3),
        ))
        .unwrap();
        assert_eq!(
            a,
            ["file:///D:/%E8%B5%84%E6%96%99/%E5%B9%B4%E6%8A%A5%20%231.pdf#page=3"]
        );
        assert_eq!(
            file_url(Path::new(r"\\nas\共享\a.pdf")),
            "file://nas/%E5%85%B1%E4%BA%AB/a.pdf"
        );
    }

    #[test]
    fn targets_by_kind() {
        let at = OpenAt {
            ms: Some(1000),
            page: Some(2),
        };
        assert_eq!(target("video", "mkv", at), Some(Target::Ms(1000)));
        assert_eq!(target("audio", "mp3", at), Some(Target::Ms(1000)));
        assert_eq!(target("document", "pdf", at), Some(Target::Page(2)));
        assert_eq!(target("document", "docx", at), None);
        assert_eq!(target("image", "png", at), None);
        let zero = OpenAt {
            ms: None,
            page: Some(0),
        };
        assert_eq!(target("document", "pdf", zero), None);
    }
}
