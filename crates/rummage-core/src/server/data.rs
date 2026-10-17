//! 设置页“数据”与搜索历史在核心状态上的部分：
//! - 数据库状态：打不开时的原因，以及清除后重建能否解决；清除本身由界面进程在核心不持有数据库时完成。
//! - 清空索引：先让流水线暂停领取（`Hold::Clear`）并等处理中的作业结束——文件 ID 会在清空后被新文件复用，
//!   在途的旧结果可能通过写库前的核对而写进新记录；再取消正在进行的扫描，分批删除（批与批之间释放数据库锁），
//!   同步内存向量索引，恢复流水线后重新扫描全部文件夹。
//! - 缩略图按 `quick_hash` 命名，自动清理：扫描和移除文件夹之后删除失去引用的（`drop_orphan_thumbs`），
//!   启动对账之后再全量清扫一次（`sweep_thumbs`）。清空索引不删缩略图，重建时直接复用。不建内容索引的文件在生成
//!   缩略图时已写回 `quick_hash`（`files`），清扫时同样算作有引用。视频画面帧与长图裁剪图在同一 `quick_hash` 的
//!   目录中（`files::derived_dir`），随缩略图一起删除。全量清扫时同时删除已没有文件使用的失败黑名单行。
//! - 搜索历史：参数 `save_history` 关闭时不记录，读取也返回空；条数上限为参数 `history_limit`。
use std::collections::HashSet;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use rummage_proto::{ApiError, Reply, Request, params};

use super::files::{derived_dir, thumb_path};
use super::setup::release_limit;
use super::{State, api_error, db, lock, opened, unavailable};
use crate::pipeline::{self, Hold};

/// 清空索引时每批删除的文件数；批与批之间释放数据库锁，搜索不必等整个清空完成
const CLEAR_BATCH: usize = 2000;

/// 参数 `save_history` 开启时为 `history_limit`，关闭时为空
fn history_limit(state: &State) -> Option<usize> {
    let values = state.params();
    if !params::flag(&values, "save_history") {
        return None;
    }
    Some(usize::try_from(pipeline::int(&values, "history_limit")).unwrap_or(0))
}

fn clear_index(state: &Arc<State>) -> Result<Reply, ApiError> {
    opened(state)?;
    let pipeline = state.pipeline.get();
    if let Some(p) = pipeline {
        let released = p.hold(Hold::Clear, true);
        if !p.wait_released(released, release_limit(&state.params())) {
            p.hold(Hold::Clear, false);
            return Err(api_error(
                ApiError::BAD_REQUEST,
                "还有文件正在处理，暂时无法清空索引，请稍后再试。",
            ));
        }
    }
    if let Some((_, cancel)) = &*state.scanning() {
        cancel.store(true, Ordering::Relaxed);
    }
    let res = delete_all(state);
    if let Some(p) = pipeline {
        p.hold(Hold::Clear, false);
    }
    res?;
    log::warn!("用户清空了索引，按现有文件夹重新建立。");
    state.queue_count_all();
    state.wake_pipeline();
    // 暂停时流水线不会自己广播，计数要在这里归零
    if let Some(p) = pipeline {
        p.broadcast();
    }
    Ok(Reply::Done)
}

fn delete_all(state: &State) -> Result<(), ApiError> {
    let core = opened(state)?;
    while lock(&core.db)
        .clear_index_batch(CLEAR_BATCH)
        .map_err(unavailable)?
        > 0
    {}
    // 与流水线写入时的加锁顺序一致：先向量索引、后数据库
    if let Err(err) = lock(&core.index).sync(&lock(&core.db)) {
        log::warn!("清空索引后同步内存向量索引失败，下次搜索时再同步：{err}");
    }
    Ok(())
}

/// 删除失去引用的缩略图（`rummage_index::thumbs`）：扫描、移除文件夹之后调用；删除文件放到后台线程，不占着调用方
pub(super) fn drop_orphan_thumbs(state: &State) {
    let Ok(core) = &state.db else {
        return;
    };
    let dir = core.dir.clone();
    let hashes = match lock(&core.db).take_orphan_thumbs() {
        Ok(hashes) => hashes,
        Err(err) => {
            log::warn!("无法取得失去引用的缩略图：{err}");
            return;
        }
    };
    if hashes.is_empty() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("thumb-clean".into())
        .spawn(move || {
            for qh in hashes {
                let path = thumb_path(&dir, &qh);
                let sub = derived_dir(&dir, &qh);
                for res in [fs::remove_file(&path), fs::remove_dir_all(&sub)] {
                    if let Err(err) = res
                        && err.kind() != std::io::ErrorKind::NotFound
                    {
                        log::warn!("无法删除 {} 的旧缩略图：{err}", path.display())
                    }
                }
            }
        });
    if let Err(err) = spawned {
        log::warn!("无法启动删除旧缩略图的线程：{err}");
    }
}

/// 启动对账之后的全量清扫，删除 `thumbs\` 中不对应任何文件的 `.webp`，处理上次运行中漏掉的（例如删除时程序退出）。
/// 还有未完成的作业、或已建好索引却没有 `quick_hash` 的文件时跳过（失败的文件不阻止），下次启动再试；在扫描线程中执行
pub(super) fn sweep_thumbs(state: &State) {
    let Ok(core) = &state.db else {
        return;
    };
    let dir = &core.dir;
    let db = lock(&core.db);
    let keep = match db.thumbs_settled().and_then(|ok| {
        ok.then(|| {
            // 同一时机清理黑名单中已没有文件使用的行（内容变化后扫描清空了 quick_hash 的）
            db.drop_unused_blacklist()?;
            db.quick_hashes()
        })
        .transpose()
    }) {
        Ok(Some(hashes)) => hashes,
        Ok(None) => {
            log::info!("索引尚未完成，本次启动不清扫缩略图。");
            return;
        }
        Err(err) => {
            log::warn!("无法检查缩略图的引用：{err}");
            return;
        }
    };
    drop(db);
    let keep: HashSet<_> = keep
        .iter()
        .flat_map(|qh| [thumb_path(dir, qh), derived_dir(dir, qh)])
        .filter_map(|p| p.file_name().map(|n| n.to_os_string()))
        .collect();
    let Ok(entries) = fs::read_dir(dir.join("thumbs")) else {
        return;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if keep.contains(&entry.file_name()) {
            continue;
        }
        // 画面帧与裁剪图的目录以 quick_hash 的十六进制命名
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        let ours = if is_dir {
            entry
                .file_name()
                .to_str()
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_hexdigit()))
        } else {
            path.extension().is_some_and(|e| e == "webp")
                && entry.file_type().is_ok_and(|t| t.is_file())
        };
        if !ours {
            continue;
        }
        let res = if is_dir {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        match res {
            Ok(()) => removed += 1,
            Err(err) => log::warn!("无法删除旧缩略图 {}：{err}", path.display()),
        }
    }
    if removed > 0 {
        log::info!("清扫了 {removed} 张不再被引用的缩略图。");
    }
}

/// 设置页“数据”与搜索历史的请求（`Request::DbStatus` 等，见 `rummage_proto::Request`）
pub(super) fn respond(state: &Arc<State>, body: Request) -> Result<Reply, ApiError> {
    match body {
        Request::DbStatus => Ok(Reply::DbStatus {
            error: state.db.as_ref().err().cloned(),
            rebuildable: state.db_rebuildable,
        }),
        Request::ClearIndex => clear_index(state),
        Request::GetHistory => {
            let queries = match history_limit(state) {
                Some(limit) => db(state)?.history(limit).map_err(unavailable)?,
                None => Vec::new(),
            };
            Ok(Reply::History { queries })
        }
        Request::AddHistory { query } => {
            let query = query.trim();
            if let Some(limit) = history_limit(state)
                && !query.is_empty()
            {
                db(state)?.add_history(query, limit).map_err(unavailable)?;
            }
            Ok(Reply::Done)
        }
        Request::RemoveHistory { query } => {
            db(state)?.remove_history(&query).map_err(unavailable)?;
            Ok(Reply::Done)
        }
        Request::ClearHistory => {
            db(state)?.clear_history().map_err(unavailable)?;
            Ok(Reply::Done)
        }
        other => unreachable!("server::respond 只把数据管理与搜索历史的请求交给这里：{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use rummage_index::Db;
    use rummage_proto::ParamValue;

    use super::*;
    use crate::server::respond as handle;

    fn open(dir: &Path) -> Arc<State> {
        State::new("t".into(), Db::open(dir))
    }

    fn history(state: &Arc<State>) -> Vec<String> {
        let Ok(Reply::History { queries }) = respond(state, Request::GetHistory) else {
            panic!("应返回搜索历史");
        };
        queries
    }

    fn set(state: &Arc<State>, key: &str, value: ParamValue) {
        let body = Request::SetParam {
            key: key.into(),
            value,
        };
        assert_eq!(handle(body, state), Ok(Reply::Done));
    }

    #[test]
    fn history_follows_switch_and_limit() {
        let dir = crate::test_dir();
        let state = open(dir.path());
        for q in ["  柯基 ", "", "旅行", "发票"] {
            let body = Request::AddHistory { query: q.into() };
            assert_eq!(respond(&state, body), Ok(Reply::Done));
        }
        assert_eq!(history(&state), ["发票", "旅行", "柯基"]);
        set(&state, "history_limit", ParamValue::Int(2));
        assert_eq!(history(&state), ["发票", "旅行"]);

        // 关闭后不记录，也不显示；重新开启后之前的记录仍在
        set(&state, "save_history", ParamValue::Bool(false));
        let body = Request::AddHistory {
            query: "不记录".into(),
        };
        assert_eq!(respond(&state, body), Ok(Reply::Done));
        assert!(history(&state).is_empty());
        set(&state, "save_history", ParamValue::Bool(true));
        assert_eq!(history(&state), ["发票", "旅行"]);

        let body = Request::RemoveHistory {
            query: "发票".into(),
        };
        assert_eq!(respond(&state, body), Ok(Reply::Done));
        // 调小上限后，超出的旧记录到下次记录时才删除
        assert_eq!(history(&state), ["旅行", "柯基"]);
        assert_eq!(respond(&state, Request::ClearHistory), Ok(Reply::Done));
        assert!(history(&state).is_empty());
    }

    fn scan(state: &Arc<State>, id: i64) {
        let Ok(shared) = &state.db else {
            panic!("数据库应可用");
        };
        let limits = rummage_index::ScanLimits {
            max_doc_mb: 10,
            ..crate::TEST_LIMITS
        };
        let cancel = std::sync::atomic::AtomicBool::new(false);
        rummage_index::scan_root(&shared.db, id, limits, &cancel, &mut |_| {}).unwrap();
    }

    #[test]
    fn removed_files_drop_their_own_thumbs() {
        let dir = crate::test_dir();
        let docs = crate::test_dir();
        for name in ["a.txt", "b.txt", "c.txt"] {
            fs::write(docs.path().join(name), name).unwrap();
        }
        let state = open(dir.path());
        let root = rummage_index::normalize_root(docs.path()).unwrap();
        let id = db(&state).unwrap().add_root(&root).unwrap();
        scan(&state, id);
        // a 独占一张缩略图，b、c 内容相同、共用一张
        rusqlite::Connection::open(dir.path().join("index.db"))
            .unwrap()
            .execute_batch(
                "UPDATE files SET quick_hash = x'01' WHERE name = 'a.txt';
                 UPDATE files SET quick_hash = x'02' WHERE name IN ('b.txt', 'c.txt');",
            )
            .unwrap();
        let own = thumb_path(dir.path(), &[0x01]);
        let shared = thumb_path(dir.path(), &[0x02]);
        fs::create_dir_all(own.parent().unwrap()).unwrap();
        fs::write(&own, b"a").unwrap();
        fs::write(&shared, b"bc").unwrap();
        let own_frame = derived_dir(dir.path(), &[0x01]).join("t1000.webp");
        let shared_frame = derived_dir(dir.path(), &[0x02]).join("t1000.webp");
        for f in [&own_frame, &shared_frame] {
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, b"t").unwrap();
        }

        fs::remove_file(docs.path().join("a.txt")).unwrap();
        fs::remove_file(docs.path().join("b.txt")).unwrap();
        scan(&state, id);
        drop_orphan_thumbs(&state);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while (own.exists() || own_frame.exists()) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!own.exists() && !own_frame.exists());
        assert!(shared.exists() && shared_frame.exists());
    }

    #[test]
    fn sweep_waits_for_index() {
        let dir = crate::test_dir();
        let docs = crate::test_dir();
        fs::write(docs.path().join("a.txt"), "a").unwrap();
        let state = open(dir.path());
        let root = rummage_index::normalize_root(docs.path()).unwrap();
        let id = db(&state).unwrap().add_root(&root).unwrap();
        scan(&state, id);
        let kept = thumb_path(dir.path(), &[0x01, 0xab]);
        let stale = thumb_path(dir.path(), &[0xff]);
        fs::create_dir_all(kept.parent().unwrap()).unwrap();
        fs::write(&kept, b"keep").unwrap();
        fs::write(&stale, b"stale").unwrap();
        fs::write(dir.path().join("thumbs").join("note.txt"), b"x").unwrap();
        let kept_frame = derived_dir(dir.path(), &[0x01, 0xab]).join("r0_0_10_10.webp");
        let stale_frame = derived_dir(dir.path(), &[0xff]).join("t5.webp");
        let other_dir = dir.path().join("thumbs").join("其他");
        for f in [&kept_frame, &stale_frame, &other_dir.join("a.webp")] {
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, b"x").unwrap();
        }
        // 作业未完成时跳过
        sweep_thumbs(&state);
        assert!(stale.exists());
        rusqlite::Connection::open(dir.path().join("index.db"))
            .unwrap()
            .execute_batch(
                "UPDATE files SET quick_hash = x'01ab'; UPDATE jobs SET stage = 'stored';",
            )
            .unwrap();
        sweep_thumbs(&state);
        assert!(kept.exists() && !stale.exists());
        assert!(kept_frame.exists() && !stale_frame.parent().unwrap().exists());
        assert!(dir.path().join("thumbs").join("note.txt").exists());
        assert!(other_dir.exists());
    }

    #[test]
    fn db_status_and_clear_index() {
        let dir = crate::test_dir();
        let docs = crate::test_dir();
        fs::write(docs.path().join("a.txt"), b"x").unwrap();
        let state = open(dir.path());
        assert_eq!(
            respond(&state, Request::DbStatus),
            Ok(Reply::DbStatus {
                error: None,
                rebuildable: false
            })
        );
        let root = rummage_index::normalize_root(docs.path()).unwrap();
        let id = db(&state).unwrap().add_root(&root).unwrap();
        scan(&state, id);
        assert_eq!(db(&state).unwrap().list_roots().unwrap()[0].files, 1);
        assert_eq!(respond(&state, Request::ClearIndex), Ok(Reply::Done));
        let roots = db(&state).unwrap().list_roots().unwrap();
        assert_eq!((roots.len(), roots[0].files), (1, 0));
    }

    #[test]
    fn broken_db_reported_as_rebuildable() {
        let dir = crate::test_dir();
        fs::write(dir.path().join("index.db"), vec![0x5a; 8192]).unwrap();
        let state = open(dir.path());
        let Ok(Reply::DbStatus { error, rebuildable }) = respond(&state, Request::DbStatus) else {
            panic!("应返回数据库状态");
        };
        assert!(error.is_some_and(|e| e.contains("损坏")));
        assert!(rebuildable);
        // 不可写等清除也无济于事的情况不建议重建
        let blocked = dir.path().join("data");
        fs::write(&blocked, b"").unwrap();
        let state = open(&blocked);
        assert_eq!(
            respond(&state, Request::DbStatus),
            Ok(Reply::DbStatus {
                error: Some(Db::open(&blocked).err().unwrap().to_string()),
                rebuildable: false
            })
        );
        assert_eq!(
            respond(&state, Request::ClearIndex).unwrap_err().code,
            ApiError::UNAVAILABLE
        );
    }
}
