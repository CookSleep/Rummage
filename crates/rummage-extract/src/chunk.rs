//! 纯文本与 Markdown 的切片。
//!
//! - token 数由调用方的 `count` 给出：某段正文加上任务前缀与特殊 token 后的完整 token 数（实际由推理子进程的
//!   `count_tokens` 计算，核心进程不加载 llama.cpp）。切片上限 `MAX_TOKENS` 含前缀，每片都不超过它。
//! - 分段：Markdown 按标题行和空行（代码块中的 `#` 行不算标题），纯文本按空行。
//!   Markdown 的标题处强制断开，片段记录所在的标题路径；只有标题的部分并入下一段（门槛放宽到上限），
//!   仍放不下时标题不单独成片，只把标题路径带到下一片。
//! - 段落依次累积，加上下一段会超过目标长度（`chunk_tokens`）时成一片；单个段落不超过上限时保持完整。
//! - 超过上限的段落按句子（中文 `。！？；`，英文 `. ! ?` 后接空白，以及换行）再切；
//!   单句仍超过上限时按字符二分（二分出的片段之间不重叠，切口可能落在单词中间）。
//! - 前缀与特殊 token 占去上限的一半以上时返回 `ChunkError::PrefixTooLong`。
//! - 重叠：下一片以上一片末尾的若干句开头，其正文 token 数最接近目标长度的
//!   `chunk_overlap_pct`，且不超过它的两倍；放不下（加上新内容会超过上限）时不重叠。标题处断开时不重叠。
//! - 合并短片：正文 token 数小于 `chunk_min_tokens` 的片段（如首个标题前的页码、脚注编号）并入下一片，
//!   最后一片并入上一片，可以跨标题；合并后超过上限时保持原样，0 表示不合并。模型偏好短文本
//!   （ACL 2025 “Collapse of Dense Retrievers”），零碎片段容易排到前面。合并片取两片在原文中的
//!   连续范围（重叠部分不重复），标题路径取正文 token 数较多的一方（相等时取后一片）。
//! - 每片的正文是原文的连续切片（去掉首尾空白），`char_start` / `char_end` 按 Unicode 字符计、左闭右开，
//!   行号从 1 开始、两端都含。

use std::collections::BTreeMap;

use rummage_proto::params::{self, ParamValue};

/// 切片上限，含前缀与特殊 token。
pub const MAX_TOKENS: usize = 512;
/// 超过此字节数的片段不送去计数，直接视为超过上限：分词后平均每个 token 远不到 16 字节，
/// 这样也保证计数请求不会超过推理子进程的帧头上限（64 KiB）。
const COUNT_MAX_BYTES: usize = MAX_TOKENS * 16;
/// 中文句末标点后紧跟的这些字符仍属于同一句。
const CLOSERS: &[char] = &['”', '’', '」', '』', '）', '》', '"', '\''];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    Text,
    Markdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkOpts {
    /// 目标 token 数（参数表 `chunk_tokens`），不超过 `MAX_TOKENS`。
    pub target: usize,
    /// 相邻片段的重叠比例（参数表 `chunk_overlap_pct`）。
    pub overlap_pct: usize,
    /// 正文少于此 token 数的片段并入相邻片段（参数表 `chunk_min_tokens`），0 表示不合并。
    pub min: usize,
}

impl ChunkOpts {
    /// 缺少或不合法的取值用参数表的默认值。
    pub fn from_params(values: &BTreeMap<String, ParamValue>) -> Self {
        let int = |key: &str| params::int(values, key) as usize;
        ChunkOpts {
            target: int("chunk_tokens"),
            overlap_pct: int("chunk_overlap_pct"),
            min: int("chunk_min_tokens"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
    /// 偏移与行号都基于规范化后的文本（CRLF 已转为 `\n`、不含 BOM，即 `read_text` 的结果），
    /// 按 Unicode 标量计数、左闭右开。预览时要用 `read_text` 重新读取后再取，不能对原始字节直接使用。
    pub char_start: usize,
    pub char_end: usize,
    /// 从 1 开始，两端都含。
    pub line_start: usize,
    pub line_end: usize,
    /// Markdown 的标题路径，如“安装 > Windows”；没有时为空。
    pub heading: String,
}

/// 一个不超过上限的切分单元（字节范围），来自一个完整段落或超长段落切出的一部分。
struct Piece {
    start: usize,
    end: usize,
    heading: String,
    is_heading: bool,
}

struct Ctx<'a, E> {
    text: &'a str,
    /// 空正文的 token 数，即前缀与特殊 token。
    base: usize,
    count: &'a mut dyn FnMut(&str) -> Result<usize, E>,
}

/// 去掉 `text[s..e]` 首尾的空白；全是空白时返回 `None`。
fn trim(text: &str, s: usize, e: usize) -> Option<(usize, usize)> {
    let sub = &text[s..e];
    let t = sub.trim_start();
    if t.is_empty() {
        return None;
    }
    let s = s + sub.len() - t.len();
    Some((s, s + t.trim_end().len()))
}

/// Markdown ATX 标题：返回级别与标题文字。
fn md_heading(line: &str) -> Option<(usize, String)> {
    let t = line.trim_start();
    if line.len() - t.len() > 3 {
        return None;
    }
    let level = t.bytes().take_while(|&b| b == b'#').count();
    let rest = &t[level..];
    if !(1..=6).contains(&level) || !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    // 结尾的 `#` 只有前面是空白（或整个标题只有 `#`）时才是闭合序列（CommonMark），“C#”保留。
    let r = rest.trim();
    let stripped = r.trim_end_matches('#');
    let title = if stripped.is_empty() {
        ""
    } else if stripped.ends_with([' ', '\t']) {
        stripped.trim_end()
    } else {
        r
    };
    Some((level, title.to_string()))
}

/// 按标题行与空行分出的段落。
fn paragraphs(text: &str, kind: TextKind) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut path: Vec<(usize, String)> = Vec::new();
    let mut heading = String::new();
    // 当前代码块的围栏字符与长度：只有相同字符、不短于开头的围栏才关闭它（CommonMark）。
    let mut fence: Option<(char, usize)> = None;
    let mut para: Option<(usize, usize)> = None;
    let flush = |para: &mut Option<(usize, usize)>, heading: &str, out: &mut Vec<Piece>| {
        if let Some((s, e)) = para.take().and_then(|(s, e)| trim(text, s, e)) {
            out.push(Piece {
                start: s,
                end: e,
                heading: heading.to_string(),
                is_heading: false,
            });
        }
    };
    let mut start = 0;
    for line in text.split('\n') {
        let end = start + line.len();
        let t = line.trim();
        if kind == TextKind::Markdown {
            let mark = ['`', '~'].into_iter().find_map(|ch| {
                let n = t.chars().take_while(|&c| c == ch).count();
                (n >= 3).then_some((ch, n))
            });
            if let Some((ch, n)) = mark {
                match fence {
                    None => fence = Some((ch, n)),
                    Some((open, len)) if open == ch && n >= len => fence = None,
                    _ => {}
                }
            } else if fence.is_none()
                && let Some((level, title)) = md_heading(line)
            {
                flush(&mut para, &heading, &mut out);
                path.retain(|(l, _)| *l < level);
                path.push((level, title));
                let names: Vec<&str> = path
                    .iter()
                    .map(|(_, n)| n.as_str())
                    .filter(|n| !n.is_empty())
                    .collect();
                heading = names.join(" > ");
                if let Some((s, e)) = trim(text, start, end) {
                    out.push(Piece {
                        start: s,
                        end: e,
                        heading: heading.clone(),
                        is_heading: true,
                    });
                }
                start = end + 1;
                continue;
            }
        }
        if t.is_empty() {
            flush(&mut para, &heading, &mut out);
        } else {
            para = Some((para.map_or(start, |p| p.0), end));
        }
        start = end + 1;
    }
    flush(&mut para, &heading, &mut out);
    out
}

impl<E> Ctx<'_, E> {
    fn tokens(&mut self, s: usize, e: usize) -> Result<usize, E> {
        if e - s > COUNT_MAX_BYTES {
            return Ok(usize::MAX);
        }
        (self.count)(&self.text[s..e])
    }

    /// 范围内的句子，已去掉首尾空白。
    fn units(&self, s: usize, e: usize) -> Vec<(usize, usize)> {
        let sub = &self.text[s..e];
        let mut out = Vec::new();
        let mut from = 0;
        let mut it = sub.char_indices().peekable();
        while let Some((i, c)) = it.next() {
            let mut cut = match c {
                '\n' => true,
                '。' | '！' | '？' | '；' => true,
                '.' | '!' | '?' => it.peek().is_none_or(|(_, n)| n.is_whitespace()),
                _ => false,
            }
            .then_some(i + c.len_utf8());
            if cut.is_some() && c != '\n' {
                while let Some(&(j, n)) = it.peek() {
                    if !CLOSERS.contains(&n) {
                        break;
                    }
                    cut = Some(j + n.len_utf8());
                    it.next();
                }
            }
            if let Some(to) = cut {
                out.extend(trim(self.text, s + from, s + to));
                from = to;
            }
        }
        out.extend(trim(self.text, s + from, e));
        out
    }

    /// 把一段切成都不超过上限的单元：先按句（行），再按字符二分。
    fn fit(
        &mut self,
        s: usize,
        e: usize,
        by_units: bool,
        out: &mut Vec<(usize, usize)>,
    ) -> Result<(), E> {
        // 每个 token 至少对应 1 字节：正文字节数加前缀不超过上限时一定放得下，不必发计数请求。
        if e - s + self.base <= MAX_TOKENS || self.tokens(s, e)? <= MAX_TOKENS {
            out.push((s, e));
            return Ok(());
        }
        if by_units {
            let units = self.units(s, e);
            if units.len() > 1 {
                for (a, b) in units {
                    self.fit(a, b, false, out)?;
                }
                return Ok(());
            }
        }
        // 按字符二分：两半之间不重叠，切口可能落在单词中间。
        let sub = &self.text[s..e];
        let n = sub.chars().count();
        let Some((mid, _)) = sub.char_indices().nth(n / 2).filter(|_| n > 1) else {
            // 单个字符不可能超过上限，这里只为保证终止。
            out.push((s, e));
            return Ok(());
        };
        for (a, b) in [(s, s + mid), (s + mid, e)] {
            if let Some((a, b)) = trim(self.text, a, b) {
                self.fit(a, b, false, out)?;
            }
        }
        Ok(())
    }

    /// 片段 `[s, e)` 末尾用作下一片开头的若干句（行）的起点；不重叠时为 `None`。
    fn overlap(&mut self, s: usize, e: usize, budget: usize) -> Result<Option<usize>, E> {
        if budget == 0 {
            return Ok(None);
        }
        let mut best: Option<(usize, usize)> = None;
        // 跳过第一句：重叠不能是整片。
        for (a, _) in self.units(s, e).into_iter().skip(1).rev() {
            let t = self.tokens(a, e)?;
            if t == usize::MAX {
                break;
            }
            let content = t.saturating_sub(self.base);
            if content > budget * 2 {
                break;
            }
            let diff = content.abs_diff(budget);
            if best.is_none_or(|(d, _)| diff < d) {
                best = Some((diff, a));
            }
            if content >= budget {
                break;
            }
        }
        Ok(best.map(|(_, a)| a))
    }
}

/// 位置换算：字节偏移 → 字符数与此前的换行数。只向前移动，切片的起点与终点都单调不减。
#[derive(Default)]
struct Cursor {
    byte: usize,
    chars: usize,
    lines: usize,
}

impl Cursor {
    fn advance(&mut self, text: &str, to: usize) {
        for c in text[self.byte..to].chars() {
            self.chars += 1;
            if c == '\n' {
                self.lines += 1;
            }
        }
        self.byte = to;
    }
}

/// 把段落切成单元并依次累积成片，返回各片的（起点, 终点, 标题路径）。
fn pack<E>(
    ctx: &mut Ctx<'_, E>,
    paras: Vec<Piece>,
    opts: &ChunkOpts,
) -> Result<Vec<(usize, usize, String)>, E> {
    let target = opts.target.min(MAX_TOKENS);
    let budget = target * opts.overlap_pct / 100;
    let mut pieces = Vec::new();
    for p in paras {
        let mut parts = Vec::new();
        ctx.fit(p.start, p.end, true, &mut parts)?;
        pieces.extend(parts.into_iter().map(|(start, end)| Piece {
            start,
            end,
            heading: p.heading.clone(),
            is_heading: p.is_heading,
        }));
    }

    let mut spans = Vec::new();
    // (起点, 终点, 标题路径, 是否含标题以外的内容)
    let mut cur: Option<(usize, usize, String, bool)> = None;
    for p in pieces {
        let mut start = p.start;
        let mut heading = p.heading;
        if let Some(c) = &mut cur {
            let brk = p.is_heading && c.3;
            // 只有标题时门槛放宽到上限，尽量让标题与下面的正文在同一片。
            let limit = if c.3 { target } else { MAX_TOKENS };
            if !brk && ctx.tokens(c.0, p.end)? <= limit {
                c.1 = p.end;
                if !c.3 {
                    c.2 = heading;
                }
                c.3 |= !p.is_heading;
                continue;
            }
            let (s, e, h, body) = cur.take().expect("上面已确认有当前片段");
            if !body {
                // 只有标题仍放不下：标题不单独成片，标题路径带到下一片。
                if !p.is_heading {
                    heading = h;
                }
            } else {
                let ov = if brk {
                    None
                } else {
                    ctx.overlap(s, e, budget)?
                };
                if let Some(o) = ov
                    && ctx.tokens(o, p.end)? <= MAX_TOKENS
                {
                    start = o;
                }
                spans.push((s, e, h));
            }
        }
        cur = Some((start, p.end, heading, !p.is_heading));
    }
    // 结尾只剩标题时丢弃，除非全文只有标题。
    if let Some((s, e, heading, body)) = cur
        && (body || spans.is_empty())
    {
        spans.push((s, e, heading));
    }
    merge_short(ctx, spans, opts.min)
}

/// 把正文少于 `min` 个 token 的片段并入下一片（最后一片并入上一片），合并后超过上限时保持原样。
fn merge_short<E>(
    ctx: &mut Ctx<'_, E>,
    spans: Vec<(usize, usize, String)>,
    min: usize,
) -> Result<Vec<(usize, usize, String)>, E> {
    if min == 0 || spans.len() < 2 {
        return Ok(spans);
    }
    // (起点, 终点, 标题路径, 正文 token 数)
    let mut out: Vec<(usize, usize, String, usize)> = Vec::new();
    let mut cur: Option<(usize, usize, String, usize)> = None;
    for (s, e, h) in spans {
        let n = ctx.tokens(s, e)?.saturating_sub(ctx.base);
        let Some(c) = cur.take() else {
            cur = Some((s, e, h, n));
            continue;
        };
        if c.3 < min {
            let t = ctx.tokens(c.0, e)?;
            if t <= MAX_TOKENS {
                let heading = if c.3 > n { c.2 } else { h };
                cur = Some((c.0, e, heading, t.saturating_sub(ctx.base)));
                continue;
            }
        }
        out.push(c);
        cur = Some((s, e, h, n));
    }
    let c = cur.expect("至少有两片，循环后一定有当前片段");
    let t = match out.last() {
        Some(p) if c.3 < min => ctx.tokens(p.0, c.1)?,
        _ => usize::MAX,
    };
    if t <= MAX_TOKENS {
        let p = out.last_mut().expect("上面已确认有上一片");
        if c.3 >= p.3 {
            p.2 = c.2;
        }
        p.1 = c.1;
    } else {
        out.push(c);
    }
    Ok(out.into_iter().map(|(s, e, h, _)| (s, e, h)).collect())
}

/// 切片失败的原因：计数出错，或前缀本身就占去太多 token。
#[derive(Debug, PartialEq, Eq)]
pub enum ChunkError<E> {
    Count(E),
    PrefixTooLong,
}

impl<E: std::fmt::Display> std::fmt::Display for ChunkError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChunkError::Count(err) => err.fmt(f),
            ChunkError::PrefixTooLong => f.write_str("文件名过长，无法识别内容。"),
        }
    }
}

impl<E: std::fmt::Debug + std::fmt::Display> std::error::Error for ChunkError<E> {}

/// `text` 的换行须已统一成 `\n`（`read_text` 的结果）。空白文本返回空列表。
pub fn chunk<E>(
    text: &str,
    kind: TextKind,
    opts: &ChunkOpts,
    count: &mut dyn FnMut(&str) -> Result<usize, E>,
) -> Result<Vec<Chunk>, ChunkError<E>> {
    let paras = paragraphs(text, kind);
    if paras.is_empty() {
        return Ok(Vec::new());
    }
    // 只有前缀与特殊 token 时的 token 数，从计数中扣除后得到正文的 token 数。
    let base = count("").map_err(ChunkError::Count)?;
    // 前缀占去一半以上时正文放不下有意义的内容（标题已在 rummage-embed 中截断到 64 字，正常不会出现）。
    if base >= MAX_TOKENS / 2 {
        return Err(ChunkError::PrefixTooLong);
    }
    let mut ctx = Ctx { text, base, count };
    let spans = pack(&mut ctx, paras, opts).map_err(ChunkError::Count)?;

    let (mut a, mut b) = (Cursor::default(), Cursor::default());
    Ok(spans
        .into_iter()
        .map(|(s, e, heading)| {
            a.advance(text, s);
            b.advance(text, e);
            Chunk {
                text: text[s..e].to_string(),
                char_start: a.chars,
                char_end: b.chars,
                line_start: a.lines + 1,
                line_end: b.lines + 1,
                heading,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests;
