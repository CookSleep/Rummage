//! OpenDocument 文本与表格（ODT、ODS）中的内嵌媒体：只看 `content.xml`，
//! 页眉页脚与母版页在 `styles.xml` 中，不看。
//! - 图片为 `draw:image`，音视频为 `draw:plugin`，`xlink:href` 指向包内的条目；指向包外（`../`、绝对路径、网址）的不处理。
//! - 图表、公式等对象的替代图（`ObjectReplacements/`）与缩略图（`Thumbnails/`）不是用户插入的图片，跳过。
//! - 表格记所在的工作表名（`table:table` 的 `table:name`）；文本只记序号。
use super::{MediaError, Ref, Zip, parse, read_xml};

pub(super) fn refs(zip: &mut Zip, max: u64) -> Result<Vec<Ref>, MediaError> {
    let part = "content.xml";
    let Some(xml) = read_xml(zip, part, max)? else {
        return Ok(Vec::new());
    };
    let doc = parse(part, &xml)?;
    let mut out = Vec::new();
    let mut sheets = Vec::new();
    for n in doc.descendants().filter(|n| n.is_element()) {
        let name = n.tag_name().name();
        if name == "table" {
            sheets.push(n);
        }
        if name != "image" && name != "plugin" {
            continue;
        }
        let Some(href) = n
            .attributes()
            .find(|a| a.name() == "href")
            .map(|a| a.value())
        else {
            continue;
        };
        if href.starts_with("../")
            || href.starts_with('/')
            || href.contains(':')
            || href.starts_with("ObjectReplacements/")
            || href.starts_with("Thumbnails/")
        {
            continue;
        }
        let table = n.ancestors().find(|a| a.tag_name().name() == "table");
        let sheet = table.and_then(|t| {
            t.attributes()
                .find(|a| a.name() == "name")
                .map(|a| a.value().to_string())
        });
        let place = match table {
            Some(t) => sheets.iter().position(|s| *s == t).unwrap_or(0) as u32 + 1,
            None => out.len() as u32 + 1,
        };
        out.push(Ref {
            part: href.trim_start_matches("./").to_string(),
            slide: None,
            sheet,
            place,
            display: None,
        });
    }
    Ok(out)
}
