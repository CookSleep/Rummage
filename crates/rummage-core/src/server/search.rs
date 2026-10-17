//! 搜索请求：计算查询向量后交给 `rummage_index::search`，在阻塞线程中执行。
//! - 文本查询：去掉首尾空白，为空时返回空结果；查询向量用 `Input::Query` 计算，按“文本 + 查询空间”缓存（LRU），
//!   同时走文件名通道。分数校准用 `text` 类型的背景分布，以图搜图与“找相似的”用 `similar`（`rummage_index::calib`）。
//! - 找相似的：直接取索引中该片段已存的向量与空间（长图切片与整图档位不同时取同一文件的整图向量），只检索同一模态，排除该文件本身；不推理，推理引擎不可用时也能用。
//!   文件没有向量（还没建好索引、不做内容索引）时回 `bad_request`。附了文字描述时（只限图片）改为重新读取这张图片，
//!   与以图搜图一样计算查询向量，只检索图片、排除该文件本身。
//! - 以图搜图：先检查网盘占位文件（`ensure_local`），再按图片识别精度（`image_tokens`）缩放（`rummage_media::prepare`），
//!   与文字描述（`text`，可为空）一起用 `Input::ImageQuery` 计算查询向量，按“图片路径、大小、修改时间 + 文字”与图像空间缓存；
//!   不走文件名与正文通道。路径只用于计算查询向量。
//! - 推理引擎不可用（没有模型、创建失败、推理出错）时只返回文件名结果，并标记 `semantic_unavailable` 与原因
//!   （`unavailable`）：引擎暂停中（引导、设备检测、模型下载）优先，其次是缺少文字模型，以图搜图再区分图片识别未开启与缺少 mmproj。
//!   搜索内容（文字或以图搜图的文字描述）超过长度上限时原因为 `QueryTooLong`。
//! - 以图搜图只接受本机磁盘上的绝对路径：读取前用 `check_volume` 拒绝 UNC 路径和网络位置，设备路径直接拒绝。
//! - `partial`：正在扫描、流水线有处理中的作业，或作业表中还有未完成的作业（含卡在没有模型时）。
//! - 匹配度（`match_pct`）：校准分数在两个锚点之间线性换算，限制在 0–100%。文字查询 50% 处为门槛
//!   `min_relevance`、90% 处为门槛加 `span_90`；以图搜图与“找相似的”用另一组锚点。低于 50% 锚点的最多 49%。
//!   锚点按查询向量所在空间的运行库选（`GGUF` / `LITERT`）：两者的 z 分布不同；门槛的默认值两者相同（3.0）。
//! - 正文片段（`snippets`，命令行用）：文档结果取第一个带文字的命中片段，空白合并后截取前 `SNIPPET_CHARS` 个字符。
//! - 日志中不记录查询文本；以图搜图的图片路径可能出现在读取失败的日志中，与索引文件的日志相同。
use std::collections::VecDeque;
use std::path::{Component, Path, Prefix};
use std::time::Instant;

use rummage_embed::space;
use rummage_embed::{Error as EmbedError, Input};
use rummage_index::{
    Filter, FsError, Query, QueryKind, SearchParams, Space, check_volume, ensure_local,
};
use rummage_proto::{
    ApiError, ParamKind, Reply, SearchHit, SegmentHit, SemanticUnavailable, SimilarTo, Snippet,
    params,
};

use super::{State, api_error, lock, opened};
use crate::pipeline::{self, Pipeline, QueryEngine};

/// 查询向量缓存的容量
const CACHE_SIZE: usize = 256;

/// 匹配度的换算锚点（见模块说明）
struct Anchors {
    /// 文字查询匹配度 90% 处比门槛高出的校准分数；50% 处就是门槛（参数 `min_relevance`），同一门槛也用来折叠结果
    span_90: f64,
    /// 以图搜图与“找相似的”50% 与 90% 处的 z：z 按本次查询自身的分数分布换算，与文字查询不是同一尺度，也不折叠，
    /// 所以不跟随门槛
    similar_50: f64,
    similar_90: f64,
}

/// llama.cpp 与 GGUF：门槛默认值 3.0 时 90% 处为 4.2，即评测中混合库相关文件 z 的中位数 4.20（599 条）；
/// 以图搜图：其他子集结果 z 的 90% 分位 2.54 处为 50%，第 1 名 z 的中位数 3.56 处为 90%
const GGUF: Anchors = Anchors {
    span_90: 1.2,
    similar_50: 2.5,
    similar_90: 3.6,
};
/// LiteRT-LM（图片 140 档，CPU）：混合库相关文件 z 的中位数 3.67（581 条）；以图搜图：其他子集结果 z 的 90% 分位 2.60、
/// 第 1 名 z 的中位数 3.53
const LITERT: Anchors = Anchors {
    span_90: 0.7,
    similar_50: 2.6,
    similar_90: 3.5,
};

/// 搜索结果中正文片段的字符数
const SNIPPET_CHARS: usize = 160;

/// 匹配度百分比（见模块说明）；`similar` 为以图搜图与“找相似的”
fn match_pct(z: Option<f32>, similar: bool, min_relevance: f64, a: &Anchors) -> Option<u8> {
    let z = f64::from(z?);
    let (at50, at90) = if similar {
        (a.similar_50, a.similar_90)
    } else {
        (min_relevance, min_relevance + a.span_90)
    };
    let pct = (50.0 + (z - at50) * 40.0 / (at90 - at50))
        .clamp(0.0, 100.0)
        .round();
    // 略低于门槛的结果已被折叠，不能四舍五入成 50%
    Some(if z < at50 { pct.min(49.0) } else { pct } as u8)
}

/// 空白合并为一个空格，过长时截断并加省略号
fn snippet_text(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= SNIPPET_CHARS {
        return flat;
    }
    let head: String = flat.chars().take(SNIPPET_CHARS).collect();
    format!("{}…", head.trim_end())
}

/// 查询向量的 LRU 缓存，键为（查询文本, 空间 ID），最近用过的在前
#[derive(Default)]
pub(super) struct Cache(VecDeque<((String, String), Vec<f32>)>);

impl Cache {
    fn get(&mut self, text: &str, space: &str) -> Option<Vec<f32>> {
        let idx = self
            .0
            .iter()
            .position(|((t, s), _)| t == text && s == space)?;
        let item = self.0.remove(idx)?;
        let v = item.1.clone();
        self.0.push_front(item);
        Some(v)
    }

    fn put(&mut self, text: &str, space: &str, v: Vec<f32>) {
        self.0
            .push_front(((text.to_string(), space.to_string()), v));
        self.0.truncate(CACHE_SIZE);
    }
}

/// 以图搜图的图片须在本机磁盘上：拒绝相对路径、设备路径、UNC 路径与网络位置
fn check_image_path(path: &Path) -> Result<(), ApiError> {
    let bad = |msg: String| api_error(ApiError::BAD_REQUEST, msg);
    if let Some(Component::Prefix(p)) = path.components().next()
        && matches!(p.kind(), Prefix::DeviceNS(_) | Prefix::Verbatim(_))
    {
        return Err(bad(
            "不支持这个位置的图片，请先把它复制到本机磁盘后再搜索。".into(),
        ));
    }
    if !path.is_absolute() {
        return Err(bad("请提供图片的完整路径。".into()));
    }
    check_volume(path).map_err(|err| match err {
        FsError::Unsupported { reason, .. } => bad(format!(
            "这张图片所在的位置不受支持（{reason}），请先把它复制到本机磁盘后再搜索。"
        )),
        other => bad(other.to_string()),
    })
}

/// 以图搜图的查询向量：先检查网盘占位文件，按当前档位缩放，与文字描述（可为空）合成一条输入。
/// 按“图片路径、大小、修改时间 + 文字”与图像空间缓存；推理失败时为空，文字描述过长时另置 `too_long`
fn image_vector(
    state: &State,
    p: &Pipeline,
    e: &mut QueryEngine,
    path: &Path,
    text: &str,
    too_long: &mut bool,
) -> Result<Option<(Vec<f32>, Space)>, ApiError> {
    ensure_local(path).map_err(|err| api_error(ApiError::BAD_REQUEST, err.to_string()))?;
    let meta = std::fs::metadata(path).map_err(|err| {
        log::warn!("无法读取以图搜图的图片 {}：{err}", path.display());
        api_error(
            ApiError::BAD_REQUEST,
            format!("无法读取这张图片：{}", rummage_index::io_cause(&err)),
        )
    })?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let key = format!("{}\n{}\n{mtime}\n{text}", path.display(), meta.len());
    let values = state.params();
    // 与库中普通图片的整图向量同一档位
    let tier = params::int(&values, "image_tokens") as u32;
    let space_key = |e: &QueryEngine| e.image_space(tier).map(|s| s.id).unwrap_or_default();
    if let Some(v) = lock(&state.queries).get(&key, &space_key(e)) {
        return Ok(e.image_space(tier).map(|s| (v, s)));
    }
    let rgb =
        rummage_media::prepare(path, tier, params::max_image_pixels(&values)).map_err(|err| {
            let msg = match err {
                // 错误本身说“不识别内容”，是给建立索引用的
                rummage_media::Error::TooLarge { .. } => format!(
                    "这张图片的像素数超过设置中的“{}”，无法用于搜索。",
                    pipeline::label("max_image_mp")
                ),
                err => err.to_string(),
            };
            api_error(ApiError::BAD_REQUEST, msg)
        })?;
    let input = Input::ImageQuery {
        rgb,
        tokens: tier,
        text: text.to_string(),
    };
    // GPU 失效时 embed_query 换用新引擎，向量空间从调用后的引擎取
    match p.embed_query(e, input) {
        Ok(v) => {
            lock(&state.queries).put(&key, &space_key(e), v.clone());
            Ok(e.image_space(tier).map(|s| (v, s)))
        }
        Err(err) => {
            log::warn!("以图搜图的查询向量计算失败：{err}");
            *too_long = matches!(err, EmbedError::QueryTooLong(_));
            Ok(None)
        }
    }
}

/// 请求中的 `limit` 收紧到参数 `result_limit` 的允许范围
fn clamp_limit(limit: u32) -> usize {
    match params::def("result_limit").map(|d| d.kind) {
        Some(ParamKind::Int { min, max }) => i64::from(limit).clamp(min, max) as usize,
        _ => unreachable!("result_limit 登记为整数参数"),
    }
}

pub(super) fn respond(
    state: &State,
    text: Option<String>,
    image_path: Option<String>,
    similar: Option<SimilarTo>,
    mut filter: Filter,
    limit: Option<u32>,
    snippets: bool,
) -> Result<Reply, ApiError> {
    let start = Instant::now();
    let core = opened(state)?;
    let (db, index) = (&core.db, &core.index);
    let values = state.params();
    let params = SearchParams {
        candidates: params::int(&values, "semantic_candidates").max(0) as usize,
        limit: limit.map_or_else(
            || params::int(&values, "result_limit").max(0) as usize,
            clamp_limit,
        ),
        rrf_k: params::int(&values, "rrf_k") as f64,
        weight_semantic: params::float(&values, "weight_semantic"),
        weight_name: params::float(&values, "weight_name"),
        weight_text: params::float(&values, "weight_text"),
        video_windows: params::int(&values, "video_top_windows").max(0) as usize,
        video_gap_ms: params::int(&values, "video_merge_gap_ms").max(0) as u64,
        min_relevance: params::float(&values, "min_relevance"),
    };
    let pipeline = state.pipeline.get();
    // 只在确实要推理时取引擎：取引擎会等唤醒验证、可能新建引擎
    let query_engine = || pipeline.and_then(|p| p.query_engine());
    let text = text.as_deref().map(str::trim).unwrap_or_default();

    let t = Instant::now();
    // “找相似的”限定的模态
    let mut modality: Option<String> = None;
    // 搜索内容超过长度上限，说明原因而不是笼统的“内容识别不可用”
    let mut too_long = false;
    let (vector, query_text): (Option<(Vec<f32>, Space)>, Option<&str>) =
        match (similar, &image_path) {
            // 普通图片加了文字描述：重新读取这张图片，与文字合成一条查询
            (Some(sim), _) if !text.is_empty() => {
                let file = lock(db)
                    .file(sim.file_id)
                    .map_err(|err| api_error(ApiError::UNAVAILABLE, err.to_string()))?;
                let Some(file) = file.filter(|f| f.kind == "image") else {
                    return Err(api_error(
                        ApiError::BAD_REQUEST,
                        "只有图片找相似时可以加描述。",
                    ));
                };
                filter.exclude_file = Some(sim.file_id);
                modality = Some("image".to_string());
                let mut engine = query_engine();
                let target = engine.as_mut().filter(|e| e.images());
                let Some((p, e)) = pipeline.zip(target) else {
                    return Ok(empty(
                        start,
                        state,
                        pipeline,
                        Some(unavailable_reason(state, pipeline, true)),
                    ));
                };
                (
                    image_vector(state, p, e, &file.path, text, &mut too_long)?,
                    None,
                )
            }
            (Some(sim), _) => {
                let found = rummage_index::stored_vector(&lock(db), sim.file_id, sim.segment_id)
                    .map_err(|err| api_error(ApiError::UNAVAILABLE, err.to_string()))?;
                let Some((v, space, m)) = found else {
                    return Err(api_error(
                        ApiError::BAD_REQUEST,
                        "这个文件还没有建好索引，暂时不能找相似。",
                    ));
                };
                filter.exclude_file = Some(sim.file_id);
                modality = Some(m);
                (Some((v, space)), None)
            }
            (None, Some(path)) => {
                let path = Path::new(path);
                let mut engine = query_engine();
                let target = engine.as_mut().filter(|e| e.images());
                let Some((p, e)) = pipeline.zip(target) else {
                    return Ok(empty(
                        start,
                        state,
                        pipeline,
                        Some(unavailable_reason(state, pipeline, true)),
                    ));
                };
                check_image_path(path)?;
                (image_vector(state, p, e, path, text, &mut too_long)?, None)
            }
            (None, None) => {
                if text.is_empty() {
                    return Ok(empty(start, state, pipeline, None));
                }
                let mut engine = query_engine();
                let vector = pipeline.zip(engine.as_mut()).and_then(|(p, e)| {
                    // 先查缓存，没有时计算并放进缓存；GPU 失效时 embed_query 换用新引擎，放进缓存时按调用后的引擎取空间
                    if let Some(v) = lock(&state.queries).get(text, &e.text_space().id) {
                        return Some((v, e.text_space().clone()));
                    }
                    match p.embed_query(e, Input::Query(text.to_string())) {
                        Ok(v) => {
                            let space = e.text_space().clone();
                            lock(&state.queries).put(text, &space.id, v.clone());
                            Some((v, space))
                        }
                        Err(err) => {
                            log::warn!("查询向量计算失败：{err}");
                            too_long = matches!(err, EmbedError::QueryTooLong(_));
                            None
                        }
                    }
                });
                (vector, Some(text))
            }
        };
    log::debug!(
        "查询向量用时 {:.1} 毫秒",
        t.elapsed().as_secs_f64() * 1000.0
    );

    // 找相似的只有加了描述时才推理，此时同以图搜图
    let by_example = image_path.is_some() || similar.is_some();
    let unavailable = vector.is_none().then(|| {
        if too_long {
            SemanticUnavailable::QueryTooLong
        } else {
            unavailable_reason(state, pipeline, by_example)
        }
    });
    let min_relevance = params.min_relevance;
    let anchors = match &vector {
        Some((_, s)) if s.runtime == space::LITERT_RUNTIME => &LITERT,
        _ => &GGUF,
    };
    let coarse = vector
        .as_ref()
        .map(|(v, _)| rummage_embed::vector::truncate(v))
        .unwrap_or_default();
    let query = Query {
        vector: vector.as_ref().map(|(v, s)| (v.as_slice(), s)),
        coarse: &coarse,
        kind: if by_example {
            QueryKind::Similar
        } else {
            QueryKind::Text
        },
        modality: modality.as_deref(),
        text: query_text,
        filter: &filter,
        params,
    };
    let hits = rummage_index::search(db, index, &query)
        .map_err(|err| api_error(ApiError::UNAVAILABLE, err.to_string()))?;
    let mut hits: Vec<SearchHit> = hits
        .into_iter()
        .map(|h| SearchHit {
            file_id: h.file_id,
            path: h.path,
            name: h.name,
            kind: h.kind,
            size: h.size,
            mtime: h.mtime,
            cloud: h.cloud,
            offline: h.offline,
            score: h.score,
            cosine: h.cosine,
            z: h.z,
            match_pct: match_pct(h.z, by_example, min_relevance, anchors),
            low_relevance: h.low,
            segments: h
                .segments
                .into_iter()
                .map(|s| SegmentHit {
                    segment_id: s.segment_id,
                    locator_json: s.locator_json,
                    cosine: s.cosine,
                })
                .collect(),
            name_hit: h.name_hit,
            text_segments: h.text_segments,
            width: h.width,
            height: h.height,
            snippet: None,
        })
        .collect();
    if snippets {
        let db = lock(db);
        for hit in hits.iter_mut().filter(|h| h.kind == "document") {
            let ids = hit.segments.iter().map(|s| s.segment_id);
            for id in ids.chain(hit.text_segments.iter().copied()) {
                let row = db
                    .segment(id)
                    .map_err(|err| api_error(ApiError::UNAVAILABLE, err.to_string()))?;
                if let Some(text) = row.and_then(|r| r.text).filter(|t| !t.trim().is_empty()) {
                    hit.snippet = Some(Snippet {
                        segment_id: id,
                        text: snippet_text(&text),
                    });
                    break;
                }
            }
        }
    }
    Ok(Reply::Results {
        hits,
        took_ms: start.elapsed().as_millis() as u64,
        partial: partial(state, pipeline),
        semantic_unavailable: unavailable.is_some(),
        unavailable,
    })
}

/// 语义检索不可用的原因（见模块说明）；`image` 为以图搜图
fn unavailable_reason(
    state: &State,
    pipeline: Option<&Pipeline>,
    image: bool,
) -> SemanticUnavailable {
    if pipeline.is_some_and(Pipeline::held) {
        return SemanticUnavailable::Paused;
    }
    let config = state.config.get();
    if !config.is_some_and(|c| space::MODEL.check(&c.model).is_ok()) {
        return SemanticUnavailable::NoModel;
    }
    if !image {
        return SemanticUnavailable::Engine;
    }
    if !params::flag(&state.params(), "index_images") {
        return SemanticUnavailable::ImagesOff;
    }
    if !config.is_some_and(|c| space::MMPROJ.check(&c.mmproj).is_ok()) {
        return SemanticUnavailable::NoMmproj;
    }
    SemanticUnavailable::Engine
}

/// 索引尚未完成：有排队或进行中的扫描、有处理中的作业，或作业表中还有未完成的作业
fn partial(state: &State, pipeline: Option<&Pipeline>) -> bool {
    if state.scan_busy() || pipeline.is_some_and(Pipeline::busy) {
        return true;
    }
    let Ok(core) = &state.db else {
        return false;
    };
    lock(&core.db).has_unfinished_jobs().unwrap_or_else(|err| {
        log::warn!("无法检查索引是否完成：{err}");
        false
    })
}

fn empty(
    start: Instant,
    state: &State,
    pipeline: Option<&Pipeline>,
    unavailable: Option<SemanticUnavailable>,
) -> Reply {
    Reply::Results {
        hits: Vec::new(),
        took_ms: start.elapsed().as_millis() as u64,
        partial: partial(state, pipeline),
        semantic_unavailable: unavailable.is_some(),
        unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_is_lru() {
        let mut c = Cache::default();
        for i in 0..CACHE_SIZE {
            c.put(&i.to_string(), "t", vec![i as f32]);
        }
        // 用过的 0 移到最前，再放一条时挤掉最久没用的 1
        assert_eq!(c.get("0", "t"), Some(vec![0.0]));
        assert_eq!(c.get("0", "other"), None);
        c.put("new", "t", vec![9.0]);
        assert_eq!(c.0.len(), CACHE_SIZE);
        assert_eq!(c.get("1", "t"), None);
        assert_eq!(c.get("0", "t"), Some(vec![0.0]));
        assert_eq!(c.get("2", "t"), Some(vec![2.0]));
        assert_eq!(c.get("new", "t"), Some(vec![9.0]));
    }

    #[test]
    fn match_pct_follows_threshold() {
        // 门槛默认值处为 50%，相关文件的中位数处为 90%，之间线性换算
        assert_eq!(match_pct(Some(3.0), false, 3.0, &GGUF), Some(50));
        assert_eq!(match_pct(Some(4.2), false, 3.0, &GGUF), Some(90));
        assert_eq!(match_pct(Some(3.6), false, 3.0, &GGUF), Some(70));
        assert_eq!(match_pct(Some(2.7), false, 3.0, &GGUF), Some(40));
        // 50% 跟随门槛，90% 为门槛加 1.2
        assert_eq!(match_pct(Some(3.5), false, 3.5, &GGUF), Some(50));
        assert_eq!(match_pct(Some(4.7), false, 3.5, &GGUF), Some(90));
        assert_eq!(match_pct(Some(3.0), false, 3.5, &GGUF), Some(33));
        // 略低于门槛的已被折叠，不显示 50%
        assert_eq!(match_pct(Some(2.99), false, 3.0, &GGUF), Some(49));
        // 以图搜图与找相似的用另一组锚点，不跟随门槛
        assert_eq!(match_pct(Some(2.5), true, 3.5, &GGUF), Some(50));
        assert_eq!(match_pct(Some(3.6), true, 3.5, &GGUF), Some(90));
        assert_eq!(match_pct(Some(2.49), true, 3.5, &GGUF), Some(49));
        // 限制在 0–100%
        assert_eq!(match_pct(Some(5.0), false, 3.0, &GGUF), Some(100));
        assert_eq!(match_pct(Some(1.0), false, 3.0, &GGUF), Some(0));
        // 没有校准分数时为空
        assert_eq!(match_pct(None, false, 3.0, &GGUF), None);
        // LiteRT 空间：90% 处为门槛加 0.7，以图搜图锚点为 2.6 与 3.5
        assert_eq!(match_pct(Some(3.7), false, 3.0, &LITERT), Some(90));
        assert_eq!(match_pct(Some(3.5), true, 3.0, &LITERT), Some(90));
    }

    #[test]
    fn snippet_flattens_and_truncates() {
        assert_eq!(
            snippet_text("  第一段。\n\n第二段\t结尾 "),
            "第一段。 第二段 结尾"
        );
        let long = "字".repeat(SNIPPET_CHARS + 5);
        let s = snippet_text(&long);
        assert_eq!(s.chars().count(), SNIPPET_CHARS + 1);
        assert!(s.ends_with('…'));
        assert_eq!(
            snippet_text(&"字".repeat(SNIPPET_CHARS)),
            "字".repeat(SNIPPET_CHARS)
        );
    }

    #[test]
    fn limit_clamped() {
        assert_eq!(clamp_limit(0), 10);
        assert_eq!(clamp_limit(50), 50);
        assert_eq!(clamp_limit(100_000), 500);
    }

    #[cfg(windows)]
    #[test]
    fn image_path_must_be_local() {
        for p in [
            r"\\server\share\a.jpg",
            r"\\?\UNC\server\share\a.jpg",
            r"\\.\C:\a.jpg",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\a.jpg",
            r"a.jpg",
        ] {
            let err = check_image_path(Path::new(p)).unwrap_err();
            assert_eq!(err.code, ApiError::BAD_REQUEST, "{p}");
            assert!(err.message.ends_with('。'), "{}", err.message);
        }
        let err = check_image_path(Path::new(r"\\server\share\a.jpg")).unwrap_err();
        assert!(err.message.contains("网络位置"), "{}", err.message);
        let dir = std::env::temp_dir();
        check_image_path(&dir.join("a.jpg")).unwrap();
    }
}
