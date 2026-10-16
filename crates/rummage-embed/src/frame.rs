//! 推理子进程的 stdin / stdout 协议。
//!
//! 一帧 = 4 字节小端长度 + JSON 头 + 4 字节小端长度 + 二进制负载（图像的 RGB 像素，音频采样或向量的 f32 小端；
//! 视频窗口为各画面帧与音频段按顺序首尾相接，长度由头中的 `MediaPart` 给出）。
//! JSON 头不超过 64 KiB，负载不超过 64 MiB；读到超限的长度立即报错，不按对端给的长度分配内存。
//! 流程：父进程把子进程放进 Job Object 后先发 `Start`，子进程收到后才加载运行库；就绪后子进程发 `Ready`
//! （含实际使用的设备）；之后父进程每次发一个请求，子进程回 `Vector` 或 `Error`；计数请求 `Count` 回 `Counted`。
//! 列出设备的模式收到 `Start` 后只发一帧 `Devices`。子进程的 stdout 只用于本协议，日志一律写 stderr。

use std::io::{self, ErrorKind, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const MAX_HEADER: usize = 64 * 1024;
pub const MAX_PAYLOAD: usize = 64 * 1024 * 1024;

/// 父进程发给子进程的请求。文本已在父进程拼好前缀。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToWorker {
    /// 子进程已放进 Job Object，可以开始加载运行库。
    Start,
    Text {
        text: String,
    },
    /// 只分词计数，不推理（切片用）。
    Count {
        text: String,
    },
    /// 负载为按行存放的 RGB 像素。
    Image {
        width: u32,
        height: u32,
    },
    /// 负载为 16 kHz 单声道采样（f32 小端）。
    Audio,
    /// 视频窗口：`parts` 依次拼成一个输入，负载为其中各图像与音频的数据；`mtmd_tokenize` 后超过 `max_tokens`
    /// 时回复 `too_long` 的错误。
    Video {
        parts: Vec<MediaPart>,
        max_tokens: usize,
    },
}

/// 视频窗口中的一段
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MediaPart {
    Text {
        text: String,
    },
    /// 负载中接下来 `width × height × 3` 字节
    Image {
        width: u32,
        height: u32,
    },
    /// 负载中接下来 `samples × 4` 字节
    Audio {
        samples: usize,
    },
}

/// ggml 设备的名称与描述，例如 `Vulkan0` 与 `NVIDIA GeForce RTX 4080 SUPER`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: String,
    pub description: String,
}

/// `worker --list-devices` 列出的一块 GPU。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuInfo {
    /// 列出设备的进程中的 ggml 设备序号，只供显示：另一个进程中的序号可能不同。
    pub ggml_index: usize,
    pub name: String,
    pub description: String,
    /// PCI 总线号；Intel 核显的 Vulkan 驱动不提供。
    pub pci_id: Option<String>,
    pub memory_total: u64,
    pub integrated: bool,
}

/// 子进程发给父进程的消息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromWorker {
    Ready {
        device: DeviceInfo,
    },
    /// 负载为池化后、未归一化的向量（f32 小端）。
    Vector,
    /// `Count` 的结果：含 `<bos>`、`<eos>` 的 token 数。
    Counted {
        tokens: usize,
    },
    /// 启动前出错时子进程随后退出；推理时出错只影响这一个请求。
    Error {
        message: String,
        /// 输入本身无效（过长、尺寸不对、未加载 mmproj 等），与后端是否正常无关。
        input: bool,
        /// 启动时找不到要求的 GPU（已拔出、被停用等），与显卡是否正常无关。
        not_found: bool,
        /// 启动时可见的 GPU 与要求的不符（两次枚举之间顺序变了），父进程换一个推理进程重试。
        mismatch: bool,
        /// 视频窗口超过 token 上限，同时 `input` 为是。
        too_long: bool,
    },
    Devices {
        devices: Vec<GpuInfo>,
    },
}

/// 超限时返回 `ErrorKind::InvalidInput`，此时还没有写出任何字节，流仍可继续使用。
pub fn write<T: Serialize>(w: &mut impl Write, header: &T, payload: &[u8]) -> io::Result<()> {
    let json = serde_json::to_vec(header).map_err(io::Error::other)?;
    if json.len() > MAX_HEADER || payload.len() > MAX_PAYLOAD {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            format!(
                "帧过大：头 {} 字节（上限 {MAX_HEADER}），负载 {} 字节（上限 {MAX_PAYLOAD}）",
                json.len(),
                payload.len()
            ),
        ));
    }
    w.write_all(&(json.len() as u32).to_le_bytes())?;
    w.write_all(&json)?;
    w.write_all(&(payload.len() as u32).to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

fn read_block(r: &mut impl Read, max: usize) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > max {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("帧长度 {len} 超过上限 {max}"),
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// 对端在两帧之间关闭时返回 `None`。
pub fn read<T: DeserializeOwned>(r: &mut impl Read) -> io::Result<Option<(T, Vec<u8>)>> {
    let json = match read_block(r, MAX_HEADER) {
        Ok(json) => json,
        Err(err) if err.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    };
    let header =
        serde_json::from_slice(&json).map_err(|err| io::Error::new(ErrorKind::InvalidData, err))?;
    let payload = read_block(r, MAX_PAYLOAD)?;
    Ok(Some((header, payload)))
}

pub fn f32_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn f32_from_bytes(b: &[u8]) -> Option<Vec<f32>> {
    let (chunks, rest) = b.as_chunks::<4>();
    if !rest.is_empty() {
        return None;
    }
    Some(chunks.iter().map(|c| f32::from_le_bytes(*c)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut buf = Vec::new();
        let req = ToWorker::Image {
            width: 2,
            height: 1,
        };
        write(&mut buf, &req, &[1, 2, 3, 4, 5, 6]).unwrap();
        write(&mut buf, &ToWorker::Audio, &f32_bytes(&[0.25, -1.0])).unwrap();
        let video = ToWorker::Video {
            parts: vec![
                MediaPart::Text {
                    text: "00:01".into(),
                },
                MediaPart::Audio { samples: 1 },
                MediaPart::Image {
                    width: 1,
                    height: 1,
                },
            ],
            max_tokens: 1024,
        };
        write(&mut buf, &video, &[0, 0, 128, 62, 1, 2, 3]).unwrap();
        write(&mut buf, &FromWorker::Vector, &f32_bytes(&[1.5, -2.0])).unwrap();
        let mut r = &buf[..];
        let (got, payload) = read::<ToWorker>(&mut r).unwrap().unwrap();
        assert_eq!(got, req);
        assert_eq!(payload, [1, 2, 3, 4, 5, 6]);
        let (got, payload) = read::<ToWorker>(&mut r).unwrap().unwrap();
        assert_eq!(got, ToWorker::Audio);
        assert_eq!(f32_from_bytes(&payload).unwrap(), [0.25, -1.0]);
        let (got, payload) = read::<ToWorker>(&mut r).unwrap().unwrap();
        assert_eq!(got, video);
        assert_eq!(payload.len(), 7);
        let (got, payload) = read::<FromWorker>(&mut r).unwrap().unwrap();
        assert_eq!(got, FromWorker::Vector);
        assert_eq!(f32_from_bytes(&payload).unwrap(), [1.5, -2.0]);
        // 两帧之间结束是正常关闭。
        assert!(read::<FromWorker>(&mut r).unwrap().is_none());
    }

    #[test]
    fn header_format() {
        let mut buf = Vec::new();
        write(&mut buf, &ToWorker::Text { text: "猫".into() }, &[]).unwrap();
        let json = r#"{"type":"text","text":"猫"}"#.as_bytes();
        assert_eq!(&buf[..4], (json.len() as u32).to_le_bytes());
        assert_eq!(&buf[4..4 + json.len()], json);
        assert_eq!(&buf[4 + json.len()..], [0, 0, 0, 0]);
    }

    #[test]
    fn rejects_oversized() {
        // 写：负载超限。
        let big = vec![0u8; MAX_PAYLOAD + 1];
        let mut buf = Vec::new();
        let err = write(&mut buf, &FromWorker::Vector, &big).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        // 写：头超限。超限时什么都不写。
        let long = ToWorker::Text {
            text: "a".repeat(MAX_HEADER),
        };
        let err = write(&mut buf, &long, &[]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(buf.is_empty());
        // 读：头长度超限，不分配内存。
        let mut buf = ((MAX_HEADER + 1) as u32).to_le_bytes().to_vec();
        buf.extend_from_slice(&[b'{'; 16]);
        let err = read::<FromWorker>(&mut &buf[..]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        // 读：负载长度超限。
        let mut buf = Vec::new();
        write(&mut buf, &FromWorker::Vector, &[]).unwrap();
        let n = buf.len();
        buf[n - 4..].copy_from_slice(&u32::MAX.to_le_bytes());
        let err = read::<FromWorker>(&mut &buf[..]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn rejects_truncated_and_garbage() {
        let mut buf = Vec::new();
        write(&mut buf, &FromWorker::Vector, &[1, 2, 3, 4]).unwrap();
        // 负载被截断。
        assert!(read::<FromWorker>(&mut &buf[..buf.len() - 1]).is_err());
        // 头不是合法 JSON。
        let mut bad = 3u32.to_le_bytes().to_vec();
        bad.extend_from_slice(b"abc\0\0\0\0");
        let err = read::<FromWorker>(&mut &bad[..]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert!(f32_from_bytes(&[0, 0, 0]).is_none());
    }
}
