//! 文档的内嵌媒体：列出文档中嵌入的图片、音频、视频，过滤、去重、截到上限，
//! 再按需逐个取出。正文提取（`doc`）不受影响。
//!
//! - 来源：PDF 用 `xberg-native-pdf` 的图像句柄（`pdf`），解码前即可得到像素尺寸、页面上的显示框与压缩流；
//!   PPTX、DOCX、XLSX、ODT、ODS 直接读 zip，按关系文件对应到幻灯片、工作表（`ooxml`、`odf`）。其他格式不抽取。
//!   只看正文部分：母版、版式、页眉页脚、样式中的图片不读，它们多是 logo、背景与装饰。外链的媒体不处理。
//! - 过滤（都在解码前判断，`Filter`）：
//!   - 像素数超过 `max_pixels`（参数 `max_image_mp`）、短边小于 `min_px`（`embed_min_px`）；
//!   - 宽高比超出 0.2–5（Xberg 1.3.9 `image_kind.rs` 把这个区间外的小图归为装饰条，细长的图也难以识别）；
//!   - PDF 页面或幻灯片上显示的宽或高不超过对应边的 5%（PyMuPDF4LLM `image_size_limit` 的默认值）；
//!   - 同一张图出现在 `repeat_pages`（`embed_repeat_pages`）个页面、幻灯片或位置以上：logo、页眉、背景。
//!
//!   解码后近乎纯色的图由调用方判断（`rummage_media::is_flat`）。
//! - 去重：图片按字节的 BLAKE3（PDF 为压缩流），同一张图只取第一次出现的位置；音视频按 zip 条目去重。
//! - 上限：最多 `max_count`（`embed_max_per_doc`）个；扫描页（PDF 上占页面 85% 以上的整页图，Xberg
//!   `FULL_PAGE_IMAGE_AREA_RATIO`）不计入上限。
//! - 列出时不解码、不保留数据；`Source::load` 每次取一个，调用方处理完即释放，扫描件再多页也不会同时占用内存。
//! - zip 条目解压时按 `max_entry_bytes` 限制大小，不信任声明的大小；不压缩的音视频条目只给出字节范围，
//!   由 ffmpeg 的 `subfile` 协议直接读文档（`Loaded::Range`）。

mod odf;
mod ooxml;
mod pdf;

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::sync::{Mutex, PoisonError};

use zip::ZipArchive;

/// 宽高比的允许范围（短边 / 长边不小于它），见模块说明
const MIN_ASPECT: f64 = 0.2;
/// 在页面上显示的宽、高至少占对应边的比例，见模块说明
const MIN_DISPLAY: f64 = 0.05;
/// 扫描页：图片占页面面积的比例，见模块说明
const SCAN_AREA: f64 = 0.85;
/// 关系文件、幻灯片等 XML 部件的大小上限；超过时按损坏处理
const MAX_XML_BYTES: u64 = 32 * 1024 * 1024;

/// 消息是给日志看的；内嵌媒体出错不影响文档本身
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("无法读取 PDF：{0}")]
    Pdf(String),
    #[error("无法读取压缩包：{0}")]
    Zip(String),
    #[error("无法解析 {part}：{msg}")]
    Xml { part: String, msg: String },
    #[error("{0} 超过大小上限")]
    TooLarge(String),
    #[error("找不到内嵌媒体 {0}")]
    Missing(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Image,
    Audio,
    Video,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::Audio => "audio",
            MediaKind::Video => "video",
        }
    }
}

/// 内嵌媒体在文档中的位置，写进片段的 locator，`/crop` 据此重新取出
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum At {
    /// PDF 第 `page` 页（从 1 起）上按绘制顺序的第 `pos` 个图像对象（从 0 起）
    Pdf { page: u32, pos: u32 },
    /// zip 中的条目
    Part(String),
}

/// 列出的一个内嵌媒体
#[derive(Debug, Clone, PartialEq)]
pub struct Embedded {
    pub kind: MediaKind,
    /// 通过过滤的媒体在文档中的序号，从 1 起
    pub n: u32,
    pub at: At,
    /// PDF 的页码（从 1 起）
    pub page: Option<u32>,
    /// 演示文稿的幻灯片号（从 1 起）
    pub slide: Option<u32>,
    /// Excel、OpenDocument 表格的工作表名
    pub sheet: Option<String>,
    /// 图片的像素宽高；音视频为空
    pub size: Option<(u32, u32)>,
    /// 扫描页：PDF 上占页面 85% 以上的整页图，不计入上限
    pub scan: bool,
}

impl Embedded {
    /// 片段 locator 中的 `embed` 部分，对应前端 `src/lib/media.ts` 的说明
    pub fn locator(&self) -> serde_json::Value {
        let mut v = serde_json::json!({ "kind": self.kind.as_str(), "n": self.n });
        match &self.at {
            At::Pdf { pos, .. } => v["pos"] = (*pos).into(),
            At::Part(p) => v["part"] = p.as_str().into(),
        }
        if let Some(p) = self.page {
            v["page"] = p.into();
        }
        if let Some(s) = self.slide {
            v["slide"] = s.into();
        }
        if let Some(s) = &self.sheet {
            v["sheet"] = s.as_str().into();
        }
        if let Some((w, h)) = self.size {
            v["width"] = w.into();
            v["height"] = h.into();
        }
        v
    }

    /// 从 locator 的 `embed` 部分还原位置；不是内嵌媒体时为空
    pub fn at_of(embed: &serde_json::Value) -> Option<At> {
        if let Some(part) = embed.get("part").and_then(|p| p.as_str()) {
            return Some(At::Part(part.to_string()));
        }
        let n = |k: &str| u32::try_from(embed.get(k)?.as_u64()?).ok();
        Some(At::Pdf {
            page: n("page")?,
            pos: n("pos")?,
        })
    }
}

/// 取出的内容
#[derive(Debug, Clone, PartialEq)]
pub enum Loaded {
    /// 已解码、透明部分合成到白底的 8 位 RGB（PDF 的图像对象）
    Rgb {
        width: u32,
        height: u32,
        data: Vec<u8>,
    },
    /// 文件内容：zip 中的图片，或压缩存放的音视频
    File(Vec<u8>),
    /// 不压缩的 zip 条目在文档中的字节范围 `[start, end)`
    Range(u64, u64),
}

#[derive(Debug, Clone)]
pub struct MediaOpts {
    pub images: bool,
    pub audio: bool,
    pub video: bool,
    pub min_px: u32,
    pub max_count: usize,
    pub repeat_pages: usize,
    pub max_pixels: u64,
    /// 解压单个 zip 条目的上限（字节）
    pub max_entry_bytes: u64,
}

impl MediaOpts {
    fn wants(&self, kind: MediaKind) -> bool {
        match kind {
            MediaKind::Image => self.images,
            MediaKind::Audio => self.audio,
            MediaKind::Video => self.video,
        }
    }
}

pub use rummage_index::MEDIA_EXTS;

/// zip 包，每份文档只打开一次，之后列出与取出都复用
type Zip = ZipArchive<Cursor<Vec<u8>>>;

/// 已打开的文档。zip 包读条目时要可变借用，放在锁里，多个任务线程共用同一个 `Source`
pub enum Source {
    Pdf(Box<xberg_native_pdf::PdfDocument>),
    Zip {
        ext: String,
        /// 文档的字节数，字节范围不能超出它
        len: u64,
        zip: Mutex<Zip>,
    },
}

impl Source {
    /// 不抽取内嵌媒体的格式为空
    pub fn open(ext: &str, bytes: Vec<u8>) -> Result<Option<Source>, MediaError> {
        if !MEDIA_EXTS.contains(&ext) {
            return Ok(None);
        }
        if ext == "pdf" {
            let doc = xberg_native_pdf::PdfDocument::from_bytes(bytes)
                .map_err(|err| MediaError::Pdf(err.to_string()))?;
            return Ok(Some(Source::Pdf(Box::new(doc))));
        }
        let len = bytes.len() as u64;
        let zip =
            ZipArchive::new(Cursor::new(bytes)).map_err(|err| MediaError::Zip(err.to_string()))?;
        Ok(Some(Source::Zip {
            ext: ext.to_string(),
            len,
            zip: Mutex::new(zip),
        }))
    }

    /// 按文档中的顺序列出通过过滤的内嵌媒体（见模块说明）
    pub fn list(&self, opts: &MediaOpts) -> Result<Vec<Embedded>, MediaError> {
        let cands = match self {
            Source::Pdf(doc) => {
                if !opts.images {
                    return Ok(Vec::new());
                }
                pdf::candidates(doc)?
            }
            Source::Zip { ext, zip, .. } => {
                let mut zip = zip.lock().unwrap_or_else(PoisonError::into_inner);
                let refs = match ext.as_str() {
                    "pptx" => ooxml::pptx(&mut zip, opts.max_entry_bytes)?,
                    "docx" => ooxml::docx(&mut zip, opts.max_entry_bytes)?,
                    "xlsx" => ooxml::xlsx(&mut zip, opts.max_entry_bytes)?,
                    _ => odf::refs(&mut zip, opts.max_entry_bytes)?,
                };
                zip_candidates(&mut zip, refs, opts)
            }
        };
        Ok(select(cands, opts))
    }

    /// 取出一个内嵌媒体
    pub fn load(&self, at: &At, opts: &MediaOpts) -> Result<Loaded, MediaError> {
        match (self, at) {
            (Source::Pdf(doc), At::Pdf { page, pos }) => {
                pdf::load(doc, *page, *pos, opts.max_pixels)
            }
            (Source::Zip { len, zip, .. }, At::Part(part)) => {
                let mut zip = zip.lock().unwrap_or_else(PoisonError::into_inner);
                let mut f = zip
                    .by_name(part)
                    .map_err(|_| MediaError::Missing(part.clone()))?;
                let media = !matches!(kind_of_part(part), Some(MediaKind::Image));
                if media
                    && f.compression() == zip::CompressionMethod::Stored
                    && let Some(start) = f.data_start()
                {
                    // 字节范围来自文档中的记录，不能越过文档末尾
                    return match start.checked_add(f.compressed_size()) {
                        Some(end) if end <= *len => Ok(Loaded::Range(start, end)),
                        _ => Err(MediaError::Zip(format!("{part} 的位置超出文档"))),
                    };
                }
                read_limited(&mut f, part, opts.max_entry_bytes).map(Loaded::File)
            }
            _ => Err(MediaError::Missing(format!("{at:?}"))),
        }
    }
}

/// 读出一个条目，超过 `max` 字节时报错（不信任声明的大小）
fn read_limited(r: &mut impl Read, part: &str, max: u64) -> Result<Vec<u8>, MediaError> {
    let mut out = Vec::new();
    r.take(max + 1)
        .read_to_end(&mut out)
        .map_err(|err| MediaError::Zip(format!("{part}：{err}")))?;
    if out.len() as u64 > max {
        return Err(MediaError::TooLarge(part.to_string()));
    }
    Ok(out)
}

/// 读出一个 XML 部件；没有这个部件时为空
fn read_xml(zip: &mut Zip, part: &str, max: u64) -> Result<Option<String>, MediaError> {
    let Ok(mut f) = zip.by_name(part) else {
        return Ok(None);
    };
    let bytes = read_limited(&mut f, part, max.min(MAX_XML_BYTES))?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|err| MediaError::Xml {
            part: part.to_string(),
            msg: err.to_string(),
        })
}

fn parse<'a>(part: &str, xml: &'a str) -> Result<roxmltree::Document<'a>, MediaError> {
    roxmltree::Document::parse(xml).map_err(|err| MediaError::Xml {
        part: part.to_string(),
        msg: err.to_string(),
    })
}

/// 按扩展名判断条目的种类；不是图片、音频、视频时为空
fn kind_of_part(part: &str) -> Option<MediaKind> {
    let ext = part.rsplit_once('.')?.1.to_lowercase();
    match rummage_index::kind_of(&ext) {
        "image" => Some(MediaKind::Image),
        "audio" => Some(MediaKind::Audio),
        "video" => Some(MediaKind::Video),
        _ => None,
    }
}

/// 过滤前的一个候选
#[derive(Debug, Clone)]
struct Cand {
    kind: MediaKind,
    at: At,
    page: Option<u32>,
    slide: Option<u32>,
    sheet: Option<String>,
    /// 计算重复次数的位置（页、幻灯片、工作表；没有页面的文档每次出现各算一处）
    place: u32,
    /// 去重的键
    key: Vec<u8>,
    /// 图片的像素宽高
    size: Option<(u32, u32)>,
    /// 在页面或幻灯片上显示的宽、高占对应边的比例；不知道时为空
    display: Option<(f64, f64)>,
    scan: bool,
}

/// zip 中一处引用：条目与位置
#[derive(Debug, Clone)]
struct Ref {
    part: String,
    slide: Option<u32>,
    sheet: Option<String>,
    place: u32,
    display: Option<(f64, f64)>,
}

/// 读出 zip 引用的条目，得到候选；读不出、认不出格式（EMF、WMF、SVG 等）的跳过
fn zip_candidates(zip: &mut Zip, refs: Vec<Ref>, opts: &MediaOpts) -> Vec<Cand> {
    // 条目 → 去重的键与图片的像素宽高；读不出、认不出格式的为空
    type Info = Option<(Vec<u8>, Option<(u32, u32)>)>;
    let mut known: HashMap<String, Info> = HashMap::new();
    let mut out = Vec::new();
    for r in refs {
        let Some(kind) = kind_of_part(&r.part) else {
            continue;
        };
        if !opts.wants(kind) {
            continue;
        }
        let info = known.entry(r.part.clone()).or_insert_with(|| {
            if kind != MediaKind::Image {
                // 音视频按条目去重，不必读出内容
                return zip
                    .by_name(&r.part)
                    .is_ok()
                    .then(|| (r.part.as_bytes().to_vec(), None));
            }
            let mut f = zip.by_name(&r.part).ok()?;
            let bytes = read_limited(&mut f, &r.part, opts.max_entry_bytes).ok()?;
            let size = image::ImageReader::new(Cursor::new(&bytes))
                .with_guessed_format()
                .ok()?
                .into_dimensions()
                .ok()?;
            Some((blake3::hash(&bytes).as_bytes().to_vec(), Some(size)))
        });
        let Some((key, size)) = info.clone() else {
            continue;
        };
        out.push(Cand {
            kind,
            at: At::Part(r.part),
            page: None,
            slide: r.slide,
            sheet: r.sheet,
            place: r.place,
            key,
            size,
            display: r.display,
            scan: false,
        });
    }
    out
}

/// 过滤、去重、截到上限，按文档中的顺序编号（见模块说明）
fn select(cands: Vec<Cand>, opts: &MediaOpts) -> Vec<Embedded> {
    let mut places: HashMap<&[u8], HashSet<u32>> = HashMap::new();
    for c in &cands {
        places.entry(&c.key).or_default().insert(c.place);
    }
    let repeated: HashSet<Vec<u8>> = places
        .into_iter()
        .filter(|(_, p)| p.len() >= opts.repeat_pages.max(1))
        .map(|(k, _)| k.to_vec())
        .collect();
    let mut seen = HashSet::new();
    let (mut out, mut counted) = (Vec::new(), 0);
    for c in cands {
        if repeated.contains(&c.key) || seen.contains(&c.key) {
            continue;
        }
        if let Some((w, h)) = c.size {
            let (short, long) = (w.min(h), w.max(h));
            if u64::from(w) * u64::from(h) > opts.max_pixels
                || short < opts.min_px
                || f64::from(short) < f64::from(long) * MIN_ASPECT
            {
                continue;
            }
        }
        if c.display
            .is_some_and(|(w, h)| w <= MIN_DISPLAY || h <= MIN_DISPLAY)
        {
            continue;
        }
        // 同一张图在别处显示得够大时仍可取那一处，所以过滤之后才记为已见
        seen.insert(c.key.clone());
        if !c.scan {
            if counted >= opts.max_count {
                continue;
            }
            counted += 1;
        }
        out.push(Embedded {
            kind: c.kind,
            n: out.len() as u32 + 1,
            at: c.at,
            page: c.page,
            slide: c.slide,
            sheet: c.sheet,
            size: c.size,
            scan: c.scan,
        });
    }
    out
}

/// 相对于 `base` 部件解析关系文件中的目标，得到 zip 中的条目名；指向包外时为空
fn resolve(base: &str, target: &str) -> Option<String> {
    if target.contains("://") {
        return None;
    }
    let mut parts: Vec<&str> = match target.strip_prefix('/') {
        Some(_) => Vec::new(),
        None => base.rsplit_once('/').map_or(Vec::new(), |(dir, _)| {
            dir.split('/').filter(|s| !s.is_empty()).collect()
        }),
    };
    for seg in target.trim_start_matches('/').split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests;
