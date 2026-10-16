//! 失败文件：列表按游标分页读取；重试后唤醒流水线，并立即广播一次进度，
//! 暂停中、没有作业在写入时界面的失败计数与托盘也能马上更新。
//! 忽略（`Db::ignore_failed`）把文件加入所属文件夹的排除规则并从索引中移除，同样立即广播进度，并广播 `RootsChanged`；不重新扫描，
//! 之后的扫描与监听按新规则跳过它。
use rummage_proto::{ApiError, Event, FailedCursor, FailedFile, Reply};

use super::{State, db, unavailable};

/// 一页最多的条数
const MAX_PAGE: u32 = 500;

pub(super) fn list(
    state: &State,
    after: Option<FailedCursor>,
    limit: u32,
) -> Result<Reply, ApiError> {
    let page = db(state)?
        .failed_files(
            after.map(|c| (c.failed_at, c.file_id)),
            limit.min(MAX_PAGE).into(),
        )
        .map_err(unavailable)?;
    Ok(Reply::FailedFiles {
        files: page
            .files
            .into_iter()
            .map(|f| FailedFile {
                file_id: f.file_id,
                name: f.name,
                path: f.path.to_string_lossy().into_owned(),
                kind: f.kind,
                reason: f.reason,
                failed_at: f.failed_at,
                retryable: f.retryable,
            })
            .collect(),
        total: page.total,
        retryable: page.retryable,
    })
}

/// `file_id` 为空时重试全部
pub(super) fn retry(state: &State, file_id: Option<i64>) -> Result<Reply, ApiError> {
    let n = db(state)?.retry_failed(file_id).map_err(unavailable)?;
    if n > 0 {
        state.wake_pipeline();
        if let Some(p) = state.pipeline.get() {
            p.broadcast();
        }
    }
    Ok(Reply::Done)
}

/// `file_id` 为空时忽略全部
pub(super) fn ignore(state: &State, file_id: Option<i64>) -> Result<Reply, ApiError> {
    let n = db(state)?.ignore_failed(file_id).map_err(unavailable)?;
    if n == 0 {
        return Ok(Reply::Done);
    }
    if let Some(p) = state.pipeline.get() {
        p.broadcast();
    }
    // 排除规则与文件数变了，已打开的文件夹页刷新
    let _ = state.events.send(Event::RootsChanged);
    Ok(Reply::Done)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;

    use rummage_index::{Db, Outcome, normalize_root, scan_root};
    use rummage_proto::Request;

    use super::*;

    #[test]
    fn list_and_retry_requests() {
        let dir = crate::test_dir();
        let docs = crate::test_dir();
        for name in ["a.txt", "b.txt"] {
            std::fs::write(docs.path().join(name), name).unwrap();
        }
        {
            let db = Db::open(dir.path()).unwrap();
            let id = db.add_root(&normalize_root(docs.path()).unwrap()).unwrap();
            let db = Mutex::new(db);
            let limits = crate::TEST_LIMITS;
            scan_root(&db, id, limits, &AtomicBool::new(false), &mut |_| {}).unwrap();
            let db = db.lock().unwrap();
            for hash in [1u8, 2] {
                for _ in 0..rummage_index::MAX_ATTEMPTS {
                    let job = db.claim_job(true, true, true).unwrap().unwrap();
                    let out = Outcome::Failed {
                        quick_hash: Some(vec![hash]),
                        error: "无法读取此文件。".into(),
                    };
                    db.apply(&[(job, out)]).unwrap();
                }
            }
        }
        let state = State::new("t".into(), Db::open(dir.path()));
        // 经 server::respond 分派
        let ask = |body| super::super::respond(body, &state);
        let list = |after, limit| match ask(Request::FailedFiles { after, limit }) {
            Ok(Reply::FailedFiles {
                files,
                total,
                retryable,
            }) => (files, total, retryable),
            other => panic!("应返回失败文件：{other:?}"),
        };
        let (files, total, retryable) = list(None, 10);
        assert_eq!((files.len(), total, retryable), (2, 2, 2));
        assert_eq!(files[0].reason.as_deref(), Some("无法读取此文件。"));
        assert!(files[0].path.ends_with(&files[0].name) && files[0].retryable);
        // 游标分页
        let cursor = FailedCursor {
            failed_at: files[0].failed_at,
            file_id: files[0].file_id,
        };
        assert_eq!(list(None, 1).0, files[..1]);
        assert_eq!(list(Some(cursor), 10).0, files[1..]);

        let body = Request::RetryFailed {
            file_id: files[0].file_id,
        };
        assert_eq!(ask(body), Ok(Reply::Done));
        let (left, total, _) = list(None, 10);
        assert_eq!((left.len(), total), (1, 1));
        assert_eq!(left[0].file_id, files[1].file_id);

        // 忽略：从列表与索引中移除，加入排除规则
        let body = Request::IgnoreFailed {
            file_id: files[1].file_id,
        };
        assert_eq!(ask(body), Ok(Reply::Done));
        assert_eq!(list(None, 10), (Vec::new(), 0, 0));
        let roots = db(&state).unwrap().list_roots().unwrap();
        assert_eq!(roots[0].excludes.len(), 1);
        assert_eq!(ask(Request::IgnoreAllFailed), Ok(Reply::Done));
        assert_eq!(ask(Request::RetryAllFailed), Ok(Reply::Done));
    }
}
