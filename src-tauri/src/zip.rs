//! 只写不读的最小 ZIP 生成器，供诊断包使用：每项以 Deflate 压缩，文件名按 UTF-8 标记
//! （通用标志第 11 位），不支持 ZIP64，单项与整个压缩包都必须小于 4 GB（诊断包只有日志与几个 JSON，远小于此）。
//! 压缩用已有的 `flate2`，不另引入 ZIP 库。格式见 PKWARE 的 APPNOTE.TXT 第 4.3 节。
use std::io::{self, Write};

use flate2::Compression;
use flate2::write::DeflateEncoder;

/// 已写入的一项，生成中央目录时用
struct Entry {
    name: Vec<u8>,
    crc: u32,
    compressed: u32,
    size: u32,
    offset: u32,
}

pub struct ZipWriter {
    out: Vec<u8>,
    entries: Vec<Entry>,
    /// MS-DOS 格式的修改时间与日期
    time: u16,
    date: u16,
}

fn too_large() -> io::Error {
    io::Error::other("诊断包超过 4 GB")
}

/// 本地时间 `[年, 月, 日, 时, 分, 秒]` 转为 MS-DOS 格式的（时间, 日期）
fn dos(t: [u16; 6]) -> (u16, u16) {
    let [year, month, day, hour, minute, second] = t;
    let time = (hour << 11) | (minute << 5) | (second / 2);
    let date = (year.saturating_sub(1980).min(127) << 9) | (month << 5) | day;
    (time, date)
}

impl ZipWriter {
    /// `now` 为本地时间 `[年, 月, 日, 时, 分, 秒]`，作为每一项的修改时间
    pub fn new(now: [u16; 6]) -> Self {
        let (time, date) = dos(now);
        Self {
            out: Vec::new(),
            entries: Vec::new(),
            time,
            date,
        }
    }

    /// 写入一项；`name` 用 `/` 分隔目录
    pub fn add(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
        enc.write_all(data)?;
        let packed = enc.finish()?;
        let mut crc = flate2::Crc::new();
        crc.update(data);
        let entry = Entry {
            name: name.as_bytes().to_vec(),
            crc: crc.sum(),
            compressed: u32::try_from(packed.len()).map_err(|_| too_large())?,
            size: u32::try_from(data.len()).map_err(|_| too_large())?,
            offset: u32::try_from(self.out.len()).map_err(|_| too_large())?,
        };
        let o = &mut self.out;
        o.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        // 解压所需版本 2.0、UTF-8 文件名、Deflate
        for v in [20u16, 0x0800, 8, self.time, self.date] {
            o.extend_from_slice(&v.to_le_bytes());
        }
        for v in [entry.crc, entry.compressed, entry.size] {
            o.extend_from_slice(&v.to_le_bytes());
        }
        o.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
        o.extend_from_slice(&0u16.to_le_bytes());
        o.extend_from_slice(&entry.name);
        o.extend_from_slice(&packed);
        self.entries.push(entry);
        Ok(())
    }

    /// 写出中央目录与结尾记录，返回整个压缩包
    pub fn finish(mut self) -> io::Result<Vec<u8>> {
        let start = u32::try_from(self.out.len()).map_err(|_| too_large())?;
        let o = &mut self.out;
        for e in &self.entries {
            o.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            for v in [20u16, 20, 0x0800, 8, self.time, self.date] {
                o.extend_from_slice(&v.to_le_bytes());
            }
            for v in [e.crc, e.compressed, e.size] {
                o.extend_from_slice(&v.to_le_bytes());
            }
            // 文件名长度、扩展字段、注释、起始磁盘号、内部属性
            for v in [e.name.len() as u16, 0, 0, 0, 0] {
                o.extend_from_slice(&v.to_le_bytes());
            }
            // 外部属性、本地文件头的位置
            for v in [0u32, e.offset] {
                o.extend_from_slice(&v.to_le_bytes());
            }
            o.extend_from_slice(&e.name);
        }
        let size = u32::try_from(o.len()).map_err(|_| too_large())? - start;
        let count = u16::try_from(self.entries.len()).map_err(|_| too_large())?;
        o.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        for v in [0u16, 0, count, count] {
            o.extend_from_slice(&v.to_le_bytes());
        }
        for v in [size, start] {
            o.extend_from_slice(&v.to_le_bytes());
        }
        o.extend_from_slice(&0u16.to_le_bytes());
        Ok(self.out)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use flate2::read::DeflateDecoder;

    use super::*;

    fn u16_at(b: &[u8], i: usize) -> usize {
        u16::from_le_bytes([b[i], b[i + 1]]) as usize
    }

    fn u32_at(b: &[u8], i: usize) -> usize {
        u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as usize
    }

    /// 按中央目录逐项读回并解压、核对 CRC
    fn read_back(zip: &[u8]) -> Vec<(String, Vec<u8>)> {
        let end = zip.len() - 22;
        assert_eq!(u32_at(zip, end), 0x0605_4b50);
        let count = u16_at(zip, end + 10);
        let mut at = u32_at(zip, end + 16);
        let mut out = Vec::new();
        for _ in 0..count {
            assert_eq!(u32_at(zip, at), 0x0201_4b50);
            assert_eq!(u16_at(zip, at + 8) & 0x0800, 0x0800);
            let crc = u32_at(zip, at + 16) as u32;
            let packed = u32_at(zip, at + 20);
            let name_len = u16_at(zip, at + 28);
            let local = u32_at(zip, at + 42);
            let name = String::from_utf8(zip[at + 46..at + 46 + name_len].to_vec()).unwrap();
            assert_eq!(u32_at(zip, local), 0x0403_4b50);
            let data_at = local + 30 + u16_at(zip, local + 26) + u16_at(zip, local + 28);
            let mut data = Vec::new();
            DeflateDecoder::new(&zip[data_at..data_at + packed])
                .read_to_end(&mut data)
                .unwrap();
            let mut check = flate2::Crc::new();
            check.update(&data);
            assert_eq!(check.sum(), crc);
            out.push((name, data));
            at += 46 + name_len;
        }
        out
    }

    #[test]
    fn writes_readable_archive() {
        let mut zip = ZipWriter::new([2026, 10, 13, 9, 5, 30]);
        let log = "警告：无法读取文件。\n".repeat(1000);
        zip.add("logs/app.log", log.as_bytes()).unwrap();
        zip.add("说明.json", b"{}").unwrap();
        zip.add("empty.txt", b"").unwrap();
        let bytes = zip.finish().unwrap();
        // 重复的日志压缩后应明显变小
        assert!(bytes.len() < log.len() / 10);
        let entries = read_back(&bytes);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], ("logs/app.log".into(), log.into_bytes()));
        assert_eq!(entries[1], ("说明.json".into(), b"{}".to_vec()));
        assert_eq!(entries[2], ("empty.txt".into(), Vec::new()));
    }

    #[test]
    fn dos_time_fields() {
        let (time, date) = dos([2026, 10, 13, 9, 5, 31]);
        assert_eq!((time >> 11, time >> 5 & 0x3f, time & 0x1f), (9, 5, 15));
        assert_eq!((date >> 9, date >> 5 & 0xf, date & 0x1f), (46, 10, 13));
    }
}
