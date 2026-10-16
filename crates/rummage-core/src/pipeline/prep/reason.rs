//! 失败原因：预处理失败时记入作业表、显示在“失败文件”列表中的文字。
//! 各错误类型的 `Display` 带路径，适合写日志，不适合放进列表（系统错误码、ffmpeg 与 Xberg 的英文输出等细节在产生错误处写日志）：
//! 列表中另有文件名与路径，原因只需一句中文说明出了什么问题、能怎么办，主语统一为“此文件”；涉及设置项时写出参数表中的名称。
//! 原始错误由 `prep::failed` 写日志。
use std::io;

use rummage_embed::Error as EmbedError;
use rummage_extract::chunk::ChunkError;
use rummage_extract::{DocError, ReadError};
use rummage_index::{DbError, ENCRYPTED, FsError, HashError, IoCause, io_kind};
use rummage_media::ffmpeg::MediaError;
use rummage_proto::params;

pub(in crate::pipeline) trait Reason: std::fmt::Display {
    /// 给用户看的中文整句，不含路径
    fn reason(&self) -> String;
}

/// 读写索引数据库出错时的原因，写库线程记失败时也用它
pub(in crate::pipeline) const DB_REASON: &str = "读写索引时出错，请稍后再试。";

/// 参数表中设置项的名称
pub(crate) fn label(key: &str) -> &str {
    params::def(key).map_or(key, |d| d.label)
}

fn io_reason(err: &io::Error) -> String {
    match io_kind(err) {
        IoCause::Busy => "此文件正被其他程序占用，无法读取。",
        IoCause::NotFound => "找不到此文件，它可能已被移动或删除。",
        IoCause::Denied => "没有读取此文件的权限。",
        IoCause::Other => "无法读取此文件。",
    }
    .into()
}

impl Reason for io::Error {
    fn reason(&self) -> String {
        io_reason(self)
    }
}

impl Reason for FsError {
    fn reason(&self) -> String {
        match self {
            FsError::Placeholder(_) => "此文件仅在云端，尚未下载到本机，不能读取内容。".into(),
            FsError::Unsupported { .. } => "不支持读取此文件所在的位置。".into(),
            FsError::Inaccessible(_) => "无法访问此文件，请确认它存在且可以打开。".into(),
        }
    }
}

impl Reason for HashError {
    fn reason(&self) -> String {
        match self {
            HashError::Fs(err) => err.reason(),
            HashError::Io { source, .. } => io_reason(source),
        }
    }
}

impl Reason for ReadError {
    fn reason(&self) -> String {
        match self {
            ReadError::Fs(err) => err.reason(),
            ReadError::Io { source, .. } => io_reason(source),
            ReadError::TooLarge { .. } => {
                format!("此文件超过设置中的“{}”，不读取正文。", label("max_doc_mb"))
            }
            ReadError::NotText(_) => "此文件不是文本文件，不读取正文。".into(),
        }
    }
}

impl Reason for DocError {
    fn reason(&self) -> String {
        match self {
            DocError::Read(err) => err.reason(),
            DocError::Encrypted(_) => ENCRYPTED.into(),
            DocError::Extract { .. } => {
                "无法提取此文件的正文，文件可能已损坏或格式不受支持。".into()
            }
        }
    }
}

impl Reason for rummage_media::Error {
    fn reason(&self) -> String {
        match self {
            rummage_media::Error::Io { source, .. } => io_reason(source),
            rummage_media::Error::Decode { .. } => {
                "无法解码此文件，文件可能已损坏或格式不受支持。".into()
            }
            rummage_media::Error::TooLarge { .. } => format!(
                "此文件的像素数超过设置中的“{}”，不识别内容。",
                label("max_image_mp")
            ),
            // 图片与视频画面都会遇到，统一说“画面”
            rummage_media::Error::UnsupportedSize { width, height } => {
                format!("此文件的画面尺寸为 {width}×{height}，过于细长或为空，无法识别内容。")
            }
            rummage_media::Error::InvalidPixels { .. } => {
                "此文件的画面数据异常，无法识别内容。".into()
            }
        }
    }
}

impl Reason for MediaError {
    fn reason(&self) -> String {
        match self {
            MediaError::MissingTool(_) => {
                "缺少读取音频和视频所需的组件，请重新安装 Rummage。".into()
            }
            MediaError::Spawn { .. } => {
                "无法启动读取音频和视频所需的组件，请重新启动 Rummage 后重试。".into()
            }
            MediaError::Timeout { what, secs } => {
                format!("读取此文件中的{what}超过 {secs} 秒仍未完成，已放弃。")
            }
            MediaError::Decode { what, .. } => {
                format!("无法读取此文件中的{what}，文件可能已损坏或格式不受支持。")
            }
            MediaError::NoAudio => "此文件中没有音轨。".into(),
            MediaError::NoVideo => "此文件中没有视频画面。".into(),
            MediaError::TooLong => format!(
                "此文件超过设置中的“{}”，不识别内容。",
                label("max_audio_min")
            ),
            MediaError::Unsupported => "当前系统不支持识别音频和视频的内容。".into(),
        }
    }
}

impl Reason for DbError {
    fn reason(&self) -> String {
        DB_REASON.into()
    }
}

impl Reason for ChunkError<EmbedError> {
    fn reason(&self) -> String {
        match self {
            // 主语为此文件，同 `embed`；`user_message` 以显卡为主语，留给设备提示用
            ChunkError::Count(EmbedError::OutOfMemory(_)) => "识别此文件时显存或内存不足。".into(),
            ChunkError::Count(err) => err.user_message(),
            ChunkError::PrefixTooLong => "此文件的文件名过长，无法识别内容。".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn reasons_have_no_path_or_detail() {
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        let hash = HashError::Io {
            path: r"D:\资料\a.txt".into(),
            source: denied,
        };
        assert_eq!(hash.reason(), "没有读取此文件的权限。");
        let decode = MediaError::Decode { what: "视频" };
        assert_eq!(
            decode.reason(),
            "无法读取此文件中的视频，文件可能已损坏或格式不受支持。"
        );
        // 设置项写出参数表中的名称
        let big = rummage_media::Error::TooLarge {
            path: PathBuf::from("a.png"),
        };
        assert_eq!(
            big.reason(),
            "此文件的像素数超过设置中的“图片像素上限（百万像素）”，不识别内容。"
        );
        assert_eq!(
            MediaError::TooLong.reason(),
            "此文件超过设置中的“音视频时长上限（分钟）”，不识别内容。"
        );
        let img = rummage_media::Error::Decode {
            path: PathBuf::from(r"D:\a.png"),
        };
        assert!(!img.reason().contains("a.png"));
        let doc = DocError::Extract(r"D:\a.pdf".into());
        assert_eq!(
            doc.reason(),
            "无法提取此文件的正文，文件可能已损坏或格式不受支持。"
        );
        assert_eq!(DbError::Busy.reason(), DB_REASON);
        let oom = ChunkError::Count(EmbedError::OutOfMemory("分词".into()));
        assert_eq!(oom.reason(), "识别此文件时显存或内存不足。");
        #[cfg(windows)]
        assert_eq!(
            io::Error::from_raw_os_error(32).reason(),
            "此文件正被其他程序占用，无法读取。"
        );
    }
}
