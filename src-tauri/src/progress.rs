//! 本轮建立索引的进度，托盘提示与 Android 的进度通知共用，与界面的索引提示（`src/components/IndexHint.tsx`）一致：
//! 只数这一轮实际要建的文件，不数之前已建好的
use rummage_proto::IndexStatus;

/// 本轮的基数（本轮开始时已处理的文件数），与界面 `IndexHint` 的 `nextBase` 一致：有待处理的文件时才有，
/// 没有时从当前的已处理数算起。失败数减少（重试了失败的文件）时，基数减去减少的量：已处理数不倒退，
/// 重试的文件计入本轮的总数
pub fn next_base(base: Option<u64>, last_failed: Option<u64>, s: &IndexStatus) -> Option<u64> {
    if s.pending == 0 {
        return None;
    }
    let Some(b) = base else {
        return Some(s.done + s.failed);
    };
    let retried = last_failed.map_or(0, |f| f.saturating_sub(s.failed));
    Some(b.saturating_sub(retried))
}

/// 本轮的已处理数与总数；`base` 为本轮开始时已处理的文件数
pub fn round(s: &IndexStatus, base: u64) -> (u64, u64) {
    (
        (s.done + s.failed).saturating_sub(base),
        s.total.saturating_sub(base),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> IndexStatus {
        IndexStatus {
            state: rummage_proto::IndexState::Running,
            paused: None,
            slow: false,
            total: 5000,
            done: 1200,
            failed: 34,
            pending: 3766,
            by_kind: Default::default(),
            backend: None,
            error: None,
            eta_s: None,
            rebuild: None,
        }
    }

    #[test]
    fn retry_keeps_progress_and_extends_round() {
        let s = status();
        // 本轮开始：从当前已处理数算起
        let base = next_base(None, None, &s);
        assert_eq!(base, Some(1234));
        assert_eq!(round(&s, 1234), (0, 3766));
        // 处理中途重试了 10 个失败的文件：已处理数不倒退，总数加上它们
        let retried = IndexStatus {
            failed: 24,
            pending: 3771,
            done: 1205,
            ..s.clone()
        };
        let base = next_base(base, Some(34), &retried);
        assert_eq!(base, Some(1224));
        assert_eq!(round(&retried, 1224), (5, 3776));
        // 处理完清掉
        let done = IndexStatus {
            pending: 0,
            ..retried
        };
        assert_eq!(next_base(base, Some(24), &done), None);
    }
}
