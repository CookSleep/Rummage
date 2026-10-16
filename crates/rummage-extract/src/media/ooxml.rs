//! Office Open XML（PPTX、DOCX、XLSX）中的内嵌媒体：按关系文件（`_rels`）把正文中的
//! 引用对应到 zip 中的条目。
//! - PPTX：按 `presentation.xml` 中幻灯片的顺序编号；只看幻灯片里的图片框（`p:pic`），母版、版式、背景、备注不看。
//!   音视频的图片框带 `p14:media`（或 `a:videoFile`、`a:audioFile`），它的海报帧不另作图片；显示尺寸取 `a:xfrm/a:ext`，
//!   与 `p:sldSz` 相比。
//! - DOCX：只看 `word/document.xml`，页眉页脚不看；Word 没有可靠的页码，只记序号。内嵌音视频在 Word 中是 OLE 对象，不处理。
//! - XLSX：按工作表找到绘图部件（`xl/drawings/*.xml`）中的图片，记工作表名。
//! - 关系带 `TargetMode="External"` 的是外链，不处理。
use std::collections::HashMap;

use roxmltree::Node;

use super::{MediaError, Ref, Zip, parse, read_xml, resolve};

/// 关系 ID → 包内的目标条目；外链的关系不列出
fn rels(zip: &mut Zip, part: &str, max: u64) -> Result<HashMap<String, String>, MediaError> {
    let (dir, name) = part.rsplit_once('/').unwrap_or(("", part));
    let path = match dir {
        "" => format!("_rels/{name}.rels"),
        d => format!("{d}/_rels/{name}.rels"),
    };
    let Some(xml) = read_xml(zip, &path, max)? else {
        return Ok(HashMap::new());
    };
    let doc = parse(&path, &xml)?;
    Ok(doc
        .descendants()
        .filter(|n| n.tag_name().name() == "Relationship")
        .filter(|n| n.attribute("TargetMode") != Some("External"))
        .filter_map(|n| {
            let target = resolve(part, n.attribute("Target")?)?;
            Some((n.attribute("Id")?.to_string(), target))
        })
        .collect())
}

/// 关系命名空间（过渡与严格两种写法）中的属性，如 `r:embed`
fn rattr<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.attributes()
        .find(|a| {
            a.name() == name
                && a.namespace()
                    .is_some_and(|ns| ns.ends_with("relationships"))
        })
        .map(|a| a.value())
}

fn local(n: &Node, name: &str) -> bool {
    n.is_element() && n.tag_name().name() == name
}

fn emu(n: Node, name: &str) -> Option<f64> {
    n.attribute(name)?.parse().ok()
}

pub(super) fn pptx(zip: &mut Zip, max: u64) -> Result<Vec<Ref>, MediaError> {
    let pres = "ppt/presentation.xml";
    let Some(xml) = read_xml(zip, pres, max)? else {
        return Ok(Vec::new());
    };
    let doc = parse(pres, &xml)?;
    let size = doc
        .descendants()
        .find(|n| local(n, "sldSz"))
        .and_then(|n| Some((emu(n, "cx")?, emu(n, "cy")?)))
        .filter(|s| s.0 > 0.0 && s.1 > 0.0);
    let prels = rels(zip, pres, max)?;
    let slides: Vec<String> = doc
        .descendants()
        .filter(|n| local(n, "sldId"))
        .filter_map(|n| prels.get(rattr(n, "id")?).cloned())
        .collect();
    let mut out = Vec::new();
    for (i, part) in slides.iter().enumerate() {
        let no = i as u32 + 1;
        let Some(xml) = read_xml(zip, part, max)? else {
            continue;
        };
        let sdoc = parse(part, &xml)?;
        let srels = rels(zip, part, max)?;
        for pic in sdoc.descendants().filter(|n| local(n, "pic")) {
            // 音视频：p14:media 指向内嵌的文件；没有它时看 a:videoFile / a:audioFile 指向的是否在包内
            let media = pic
                .descendants()
                .find(|n| local(n, "media"))
                .and_then(|n| rattr(n, "embed"))
                .or_else(|| {
                    pic.descendants()
                        .find(|n| local(n, "videoFile") || local(n, "audioFile"))
                        .and_then(|n| rattr(n, "link"))
                });
            let is_media = media.is_some()
                || pic
                    .descendants()
                    .any(|n| local(&n, "videoFile") || local(&n, "audioFile"));
            let rid = if is_media {
                media
            } else {
                pic.descendants()
                    .find(|n| local(n, "blip"))
                    .and_then(|n| rattr(n, "embed"))
            };
            let Some(target) = rid.and_then(|r| srels.get(r)) else {
                continue;
            };
            let ext = pic
                .descendants()
                .find(|n| local(n, "ext") && n.parent().is_some_and(|p| local(&p, "xfrm")))
                .and_then(|n| Some((emu(n, "cx")?, emu(n, "cy")?)));
            let display = match (ext, size) {
                (Some((w, h)), Some((sw, sh))) => Some((w / sw, h / sh)),
                _ => None,
            };
            out.push(Ref {
                part: target.clone(),
                slide: Some(no),
                sheet: None,
                place: no,
                display,
            });
        }
    }
    Ok(out)
}

pub(super) fn docx(zip: &mut Zip, max: u64) -> Result<Vec<Ref>, MediaError> {
    let part = "word/document.xml";
    let Some(xml) = read_xml(zip, part, max)? else {
        return Ok(Vec::new());
    };
    let doc = parse(part, &xml)?;
    let drels = rels(zip, part, max)?;
    let mut out = Vec::new();
    for n in doc.descendants() {
        let rid = if local(&n, "blip") {
            rattr(n, "embed")
        } else if local(&n, "imagedata") {
            rattr(n, "id")
        } else {
            None
        };
        let Some(target) = rid.and_then(|r| drels.get(r)) else {
            continue;
        };
        out.push(Ref {
            part: target.clone(),
            slide: None,
            sheet: None,
            place: out.len() as u32 + 1,
            display: None,
        });
    }
    Ok(out)
}

pub(super) fn xlsx(zip: &mut Zip, max: u64) -> Result<Vec<Ref>, MediaError> {
    let book = "xl/workbook.xml";
    let Some(xml) = read_xml(zip, book, max)? else {
        return Ok(Vec::new());
    };
    let doc = parse(book, &xml)?;
    let brels = rels(zip, book, max)?;
    let sheets: Vec<(String, String)> = doc
        .descendants()
        .filter(|n| local(n, "sheet"))
        .filter_map(|n| {
            let part = brels.get(rattr(n, "id")?)?;
            Some((n.attribute("name")?.to_string(), part.clone()))
        })
        .collect();
    let mut out = Vec::new();
    for (i, (name, sheet)) in sheets.iter().enumerate() {
        let mut drawings: Vec<String> = rels(zip, sheet, max)?
            .into_values()
            .filter(|t| t.contains("/drawings/") && t.ends_with(".xml"))
            .collect();
        drawings.sort();
        for drawing in drawings {
            let Some(xml) = read_xml(zip, &drawing, max)? else {
                continue;
            };
            let ddoc = parse(&drawing, &xml)?;
            let drels = rels(zip, &drawing, max)?;
            for pic in ddoc.descendants().filter(|n| local(n, "pic")) {
                let target = pic
                    .descendants()
                    .find(|n| local(n, "blip"))
                    .and_then(|n| rattr(n, "embed"))
                    .and_then(|r| drels.get(r));
                if let Some(target) = target {
                    out.push(Ref {
                        part: target.clone(),
                        slide: None,
                        sheet: Some(name.clone()),
                        place: i as u32 + 1,
                        display: None,
                    });
                }
            }
        }
    }
    Ok(out)
}
