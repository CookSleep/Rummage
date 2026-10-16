//! 推理线程：作业置为 `embedding`，逐个片段计算向量，交给写库线程。内容相同的图片、音频、视频只推理一次：
//! 数据库中已有向量的由预处理直接复制；同时在处理、还没写入数据库的，用这里记住的最近结果。
//! 出错时整个文件计一次失败；推理后端失效时同时按驱动证据门槛换设备（`Shared::engine_failed`）：拔出、显存不足、
//! 显卡算出无效结果换剩下的设备中最快的，其余停用所有显卡、改用 CPU。之后取到的、还绑定已失效引擎的文件放回 `pending`，
//! 不计失败。显卡算出无效结果（NaN 等）时这个文件也放回、不计失败：不是文件的问题。
//! 带音频的视频：第一个窗口推理失败（后端失效除外），或任一窗口超过 token 上限时，整段视频改为纯画面从头再算，
//! 同一视频的向量格式一致，参数版本仍记带音频的版本（改了上限才重建，`rebuild`）；片段的模态为 `video_av` 或 `video_v`。
//! 文档的内嵌媒体（`docmedia`）在正文之后逐个推理，各自的模态与空间与单独的文件相同；某个媒体出错只跳过它，
//! 带音频的内嵌视频失败时只把这一个视频改为纯画面。
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Instant;

use rummage_embed::{Error as EmbedError, Input, Priority};
use rummage_index::{NewSegment, Outcome, Space};
use rummage_media::video;

use super::docmedia::Media;
use super::engine::backend_failed;
use super::{Engine, Item, Msg, Prepared, RECENT_IMAGES, Shared, Start};

/// (内容哈希, 空间, 参数版本, 定位) → 向量；长图、音频、视频的每一段各一项
type Recent = HashMap<(Vec<u8>, String, String, String), Vec<f32>>;

enum Fail {
    Error(String),
    /// 带音频的视频窗口失败，可以整段改为纯画面
    Retry(String),
    /// 显卡算出无效结果：放回 `pending` 换设备重算，不计失败
    Released,
}

/// 推理线程：逐个片段计算向量，交给写库线程
pub(super) fn run(shared: &Shared, ready: Receiver<Prepared>, out: &Sender<Msg>) {
    let mut recent = Recent::new();
    for p in ready {
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }
        // 预处理用的引擎已失效：放回去用新引擎重算，不计失败
        if p.engine.failed.load(Ordering::Relaxed) {
            if out.send(Msg::Outcome(p.job, Outcome::Released)).is_err() {
                return;
            }
            continue;
        }
        let embedding = Outcome::Embedding {
            quick_hash: p.quick_hash.clone(),
        };
        let _ = out.send(Msg::Outcome(p.job.clone(), embedding));
        // 片段要逐个交出去，其余字段还要用到；内存额度随 `_mem` 留到这个文件写出结果
        let Prepared {
            job,
            engine,
            generation,
            kind,
            mut space,
            fallback,
            quick_hash,
            content_hash,
            items,
            size,
            meta_json,
            _mem,
            media,
            replace,
            more,
            media_failed,
        } = p;
        let modality = match kind {
            "image" => "image",
            "audio" => "audio",
            "video" if fallback.is_some() => "video_av",
            "video" => "video_v",
            _ => "text",
        };
        // 纯画面的输入只共用画面帧，先备好，不必留着原来的输入
        let frames = fallback
            .as_ref()
            .map(|v| frames_only(&items, v, &space.preprocess));
        let ctx = Ctx {
            shared,
            engine: &engine,
            generation,
            content_hash: content_hash.as_deref(),
        };
        let mut res = embed_items(&ctx, items, &space, modality, &mut recent);
        if let (Err(Fail::Retry(err)), Some(v), Some(items)) = (&res, fallback, frames) {
            log::info!(
                "{} 带声音的输入无法计算（{err}），整段视频改为只看画面。",
                job.path.display()
            );
            res = embed_items(&ctx, items, &v, "video_v", &mut recent);
            space = v;
        }
        // 识别出错而跳过的内嵌媒体也计入失败个数。还没处理完的一批不记入最近结果（`Recent`）
        let mut skipped = 0;
        let mut scratch = Recent::new();
        if let Ok(segments) = &mut res {
            let rec = if more { &mut scratch } else { &mut recent };
            match embed_media(&ctx, &job.path, media, rec) {
                Ok((more, n)) => {
                    segments.extend(more);
                    skipped = n;
                }
                Err(err) => res = Err(err),
            }
        }
        let outcome = match res {
            Err(Fail::Released) => Outcome::Released,
            Err(Fail::Error(error) | Fail::Retry(error)) => Outcome::Failed {
                quick_hash: Some(quick_hash),
                error,
            },
            Ok(segments) => Outcome::Stored {
                kind,
                quick_hash,
                content_hash,
                space: Box::new(space),
                segments,
                size,
                meta_json,
                replace,
                more,
                media_failed: media_failed.map(|(n, resume)| (n + skipped, resume)),
            },
        };
        if out.send(Msg::Outcome(job, outcome)).is_err() {
            return;
        }
    }
}

/// 纯画面的输入：只保留画面帧，片段记带音频的参数版本 `version`、空间为纯画面空间 `v`
fn frames_only(items: &[Item], v: &Space, version: &str) -> Vec<Item> {
    let own = Box::new((v.clone(), version.to_string()));
    items
        .iter()
        .map(|it| Item {
            locator: it.locator.clone(),
            text: None,
            input: match &it.input {
                Input::Video { parts, .. } => Input::Video {
                    parts: video::frames_only(parts),
                    preprocess: v.preprocess.clone(),
                    max_tokens: usize::MAX,
                },
                other => other.clone(),
            },
            space: Some(own.clone()),
        })
        .collect()
}

/// 文档的内嵌媒体：逐个推理，带音频的视频失败时只把这一个视频改为纯画面。某个媒体出错只跳过它；
/// 返回片段与跳过的个数；推理后端失效（引擎已标记失效）时整个文件失败（原因为推理错误的 `user_message`），
/// 显卡算出无效结果时整个文件放回
fn embed_media(
    p: &Ctx,
    path: &std::path::Path,
    media: Vec<Media>,
    recent: &mut Recent,
) -> Result<(Vec<NewSegment>, u32), Fail> {
    // 内嵌媒体没有内容哈希，不用最近结果
    let ctx = Ctx {
        content_hash: None,
        ..*p
    };
    let (mut out, mut skipped) = (Vec::new(), 0);
    for m in media {
        let frames = m
            .fallback
            .as_ref()
            .map(|v| frames_only(&m.items, v, &m.space.preprocess));
        let mut res = embed_items(&ctx, m.items, &m.space, m.modality, recent);
        if let (Err(Fail::Retry(err)), Some(v), Some(items)) = (&res, &m.fallback, frames) {
            log::info!(
                "{} 中的一段视频带声音的输入无法计算（{err}），这段视频改为只看画面。",
                path.display()
            );
            res = embed_items(&ctx, items, v, "video_v", recent);
        }
        match res {
            Ok(segments) => out.extend(segments),
            Err(Fail::Released) => return Err(Fail::Released),
            Err(Fail::Error(err) | Fail::Retry(err)) if p.engine.failed.load(Ordering::Relaxed) => {
                return Err(Fail::Error(err));
            }
            Err(Fail::Error(err) | Fail::Retry(err)) => {
                log::warn!("{} 中的一个内嵌媒体无法识别，已跳过：{err}", path.display());
                skipped += 1;
            }
        }
    }
    Ok((out, skipped))
}

/// 一个文件的推理用到的共同信息
struct Ctx<'a> {
    shared: &'a Shared,
    engine: &'a Arc<Engine>,
    /// 领取时的挂起代数
    generation: u64,
    content_hash: Option<&'a [u8]>,
}

/// 逐个片段计算向量；全部成功后记入 `recent`
fn embed_items(
    p: &Ctx,
    items: Vec<Item>,
    space: &Space,
    modality: &'static str,
    recent: &mut Recent,
) -> Result<Vec<NewSegment>, Fail> {
    let av = modality == "video_av";
    let mut segments = Vec::new();
    let mut keys = Vec::new();
    for (i, item) in items.into_iter().enumerate() {
        let start = Start {
            at: Instant::now(),
            generation: p.generation,
        };
        let (sp, version) = match item.space.as_deref() {
            Some((sp, version)) => (sp, version),
            None => (space, &space.preprocess),
        };
        // 只有图片、音频与视频有内容哈希
        let key = p.content_hash.map(|h| {
            (
                h.to_vec(),
                sp.id.clone(),
                version.clone(),
                item.locator.clone(),
            )
        });
        let vector = match key.as_ref().and_then(|k| recent.get(k)) {
            Some(v) => v.clone(),
            None => match p.engine.backend.embed(item.input, Priority::Index) {
                Ok(e) if e.space.0 == sp.id => e.vector,
                Ok(e) => {
                    log::error!("推理结果的向量空间 {} 与预期的 {} 不符。", e.space, sp.id);
                    return Err(Fail::Error(
                        "识别结果与当前设置不符，无法保存。".to_string(),
                    ));
                }
                Err(err) => {
                    p.shared.engine_failed(p.engine, &err, start);
                    if p.engine.gpu.is_some() && matches!(err, EmbedError::BadVector(_)) {
                        log::warn!("显卡算出无效结果，换设备重算：{err}");
                        return Err(Fail::Released);
                    }
                    let backend = backend_failed(p.engine, &err);
                    let retry = av && !backend && (i == 0 || matches!(err, EmbedError::TooLong(_)));
                    log::warn!("识别失败：{err}");
                    // 失败原因的主语为此文件（`prep::reason`）；`user_message` 以显卡为主语，留给设备提示用
                    let msg = match err {
                        EmbedError::OutOfMemory(_) => "识别此文件时显存或内存不足。".to_string(),
                        _ => err.user_message(),
                    };
                    return Err(if retry {
                        Fail::Retry(msg)
                    } else {
                        Fail::Error(msg)
                    });
                }
            },
        };
        keys.extend(key);
        segments.push(NewSegment {
            modality,
            locator_json: item.locator,
            text: item.text,
            coarse: rummage_embed::vector::truncate(&vector),
            vector,
            space: item.space,
        });
    }
    if recent.len() + keys.len() > RECENT_IMAGES {
        recent.clear();
    }
    for (k, seg) in keys.into_iter().zip(&segments) {
        recent.insert(k, seg.vector.clone());
    }
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use rummage_embed::vector::{normalize, truncate};
    use rummage_index::quantize;

    /// 先 `truncate` 再 `quantize` 与在量化前自行截断归一化（`before`）逐字节相同
    #[test]
    fn coarse_unchanged_after_moving_truncate() {
        fn before(v: &[f32]) -> Vec<u8> {
            let head = &v[..v.len().min(256)];
            let norm = head.iter().map(|x| x * x).sum::<f32>().sqrt();
            let max = head.iter().fold(0f32, |m, x| m.max(x.abs()));
            let scale = if norm > 0.0 { max / norm / 127.0 } else { 0.0 };
            let mut out = vec![0u8; 256 + 4];
            if scale > 0.0 {
                for (o, x) in out.iter_mut().zip(head) {
                    *o = (x / norm / scale).round().clamp(-127.0, 127.0) as i8 as u8;
                }
            }
            out[256..].copy_from_slice(&scale.to_le_bytes());
            out
        }
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rand = move || {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((seed >> 33) as f32 / (1u64 << 31) as f32) - 0.5
        };
        let mut cases: Vec<Vec<f32>> = vec![
            Vec::new(),
            vec![0.0; 768],
            vec![0.0, 2.0],
            vec![1e-30; 768],
            vec![-0.25; 300],
        ];
        for len in [1, 7, 255, 256, 257, 768, 768, 768] {
            for _ in 0..200 {
                cases.push((0..len).map(|_| rand()).collect());
            }
        }
        // 已归一化的 768 维向量（入库与查询的实际输入）
        for _ in 0..1000 {
            let v: Vec<f32> = (0..768).map(|_| rand()).collect();
            cases.push(normalize(v).unwrap());
        }
        for v in &cases {
            assert_eq!(quantize(&truncate(v)), before(v), "{v:?}");
        }
    }
}
