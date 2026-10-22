//! 写库线程：收集各作业的结果，每 32 个文件或每 500 毫秒在一个事务中提交
//! （`Db::apply`，写入前核对文件没有变化、根目录没有被移除）。提交后有作业回到 `pending` 时唤醒调度线程。
//! 提交前在数据库锁内把按旧参数算出的结果改为放回（`drop_stale`，见 `Pipeline::requeue`）。
//! 整批提交失败时（事务已回滚，作业仍在处理中）记 error 日志，改为逐个文件单独提交；单个文件仍然失败时只给它计一次
//! 失败，连失败也写不进去时放回 `pending`、不计失败。每次提交后同步内存向量索引（先取索引锁再取数据库锁）。
//! 进度：状态或计数变了之后广播 `IndexProgress`，两次之间至少隔 500 毫秒；所有发送端关闭时写完剩下的结果后退出。
//! 剩余时间：收到作业的最终结果时记下时刻（`Recent`，只记推理流程中真正处理完的作业），广播时按滑动窗口内的速度估算；
//! 调度线程停下（暂停、睡眠、空闲等）后再领取作业时清空窗口，不把停下的时间算进速度。
use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use rummage_index::{Claimed, Outcome};

use super::{Msg, Shared, lock};

/// 攒够这么多个文件的最终结果就提交
const FLUSH_FILES: usize = 32;
/// 最早的未提交结果等了这么久就提交
const FLUSH_EVERY: Duration = Duration::from_millis(500);
/// 两次进度广播的最小间隔
const PROGRESS_EVERY: Duration = Duration::from_millis(500);
// 估算剩余时间的滑动窗口。属于实现细节，不进参数表：窗口太短时数字跳动，太长时跟不上速度变化（如从文本换到图片）。
// 手机上长图与普通图混在一起时每分钟完成的文件数相差几倍，60 秒的窗口下一分钟内的估计常相差 2 倍以上，5 分钟时约 1.2 倍
/// 只看这么久以内完成的作业
const ETA_WINDOW: Duration = Duration::from_secs(300);
/// 最多记这么多条
const ETA_MAX: usize = 1000;
/// 窗口内少于这么多条时不给出估计
const ETA_MIN: usize = 10;
/// 窗口跨度不足这么久时不给出估计；已记满 `ETA_MAX` 条时不看跨度（处理得很快时 1000 条可能不到 10 秒）
const ETA_MIN_SPAN: Duration = Duration::from_secs(10);

/// 最近完成的作业的时刻，从早到晚
#[derive(Default)]
pub(super) struct Recent(pub(super) VecDeque<Instant>);

impl Recent {
    fn push(&mut self, at: Instant) {
        self.0.push_back(at);
        if self.0.len() > ETA_MAX {
            self.0.pop_front();
        }
    }

    pub(super) fn clear(&mut self) {
        self.0.clear();
    }

    /// 剩余 `remaining` 个作业按窗口内的速度还需多少秒；先丢掉滑出窗口的记录。数据不足时为空
    pub(super) fn eta(&mut self, now: Instant, remaining: u64) -> Option<u64> {
        while self
            .0
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) > ETA_WINDOW)
        {
            self.0.pop_front();
        }
        let span = now.saturating_duration_since(*self.0.front()?);
        let len = self.0.len();
        if len < ETA_MIN || (len < ETA_MAX && span < ETA_MIN_SPAN) {
            return None;
        }
        Some((remaining as f64 * span.as_secs_f64() / self.0.len() as f64).round() as u64)
    }
}

/// 逐个文件提交；返回是否有作业回到了 `pending`
fn retry_each(shared: &Shared, batch: &[(Claimed, Outcome)]) -> bool {
    let mut ids: Vec<i64> = Vec::new();
    for (job, _) in batch {
        if !ids.contains(&job.file_id) {
            ids.push(job.file_id);
        }
    }
    let mut requeued = false;
    for id in ids {
        let items: Vec<(Claimed, Outcome)> = batch
            .iter()
            .filter(|(job, _)| job.file_id == id)
            .cloned()
            .collect();
        let db = lock(&shared.db);
        let err = match db.apply(&items) {
            Ok(r) => {
                requeued |= r;
                continue;
            }
            Err(err) => err,
        };
        let job = items[0].0.clone();
        log::error!("{} 的索引结果写入数据库失败：{err}", job.path.display());
        let failed = Outcome::Failed {
            quick_hash: items
                .iter()
                .find_map(|(_, o)| o.quick_hash())
                .map(<[u8]>::to_vec),
            error: super::prep::reason::DB_REASON.into(),
        };
        match db.apply(&[(job.clone(), failed)]) {
            Ok(r) => requeued |= r,
            Err(err) => {
                log::error!("无法记录 {} 的失败，放回待处理：{err}", job.path.display());
                match db.apply(&[(job, Outcome::Released)]) {
                    Ok(r) => requeued |= r,
                    Err(err) => log::error!("无法放回作业：{err}"),
                }
            }
        }
    }
    requeued
}

/// 按旧参数算出的结果（`Pipeline::requeue`）改为放回 `pending`，不计失败；该作业的任何最终结果都把它移出 `stale`
fn drop_stale(stale: &mut Vec<i64>, batch: &mut [(Claimed, Outcome)]) {
    for (job, outcome) in batch.iter_mut() {
        if !outcome.is_final() {
            continue;
        }
        let Some(i) = stale.iter().position(|id| *id == job.file_id) else {
            continue;
        };
        stale.swap_remove(i);
        if matches!(outcome, Outcome::Stored { .. } | Outcome::Copied { .. }) {
            *outcome = Outcome::Released;
        }
    }
}

fn flush(shared: &Shared, batch: &mut Vec<(Claimed, Outcome)>, finals: usize) {
    let res = {
        let db = lock(&shared.db);
        drop_stale(&mut lock(&shared.stale), batch);
        db.apply(batch)
    };
    let requeued = res.unwrap_or_else(|err| {
        log::error!("索引结果整批写入数据库失败，改为逐个文件提交：{err}");
        retry_each(shared, batch)
    });
    if requeued {
        shared.wake();
    }
    let mut index = lock(&shared.index);
    if let Err(err) = index.sync(&lock(&shared.db)) {
        log::warn!("无法同步内存向量索引，下次搜索时重试：{err}");
    }
    drop(index);
    shared.inflight.fetch_sub(finals, Ordering::Relaxed);
    batch.clear();
}

pub(super) fn run(shared: &Shared, rx: Receiver<Msg>) {
    let mut batch = Vec::new();
    let mut finals = 0;
    // 最早的未提交结果到达的时间
    let mut since: Option<Instant> = None;
    let mut dirty = false;
    let mut last_emit: Option<Instant> = None;
    loop {
        let flush_at = since.map(|t| t + FLUSH_EVERY);
        let emit_at = dirty.then(|| last_emit.map_or_else(Instant::now, |t| t + PROGRESS_EVERY));
        let msg = match flush_at.into_iter().chain(emit_at).min() {
            Some(at) => rx.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let closed = match msg {
            Ok(Msg::Outcome(job, outcome)) => {
                finals += usize::from(outcome.is_final());
                // 跳过的文档、改判为视频或超限、命中黑名单、放回的作业几乎不花时间，算进去会高估速度
                if matches!(
                    outcome,
                    Outcome::Stored { .. } | Outcome::Copied { .. } | Outcome::Failed { .. }
                ) {
                    lock(&shared.recent).push(Instant::now());
                }
                since.get_or_insert_with(Instant::now);
                batch.push((job, outcome));
                false
            }
            Ok(Msg::Progress) => {
                dirty = true;
                false
            }
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => true,
        };
        if !batch.is_empty()
            && (closed
                || finals >= FLUSH_FILES
                || since.is_some_and(|t| t.elapsed() >= FLUSH_EVERY))
        {
            flush(shared, &mut batch, finals);
            finals = 0;
            since = None;
            dirty = true;
        }
        if dirty && (closed || last_emit.is_none_or(|t| t.elapsed() >= PROGRESS_EVERY)) {
            // 在暂停状态的锁内读取并发出，不会晚于暂停状态变化时立即发出的事件
            shared.broadcast();
            dirty = false;
            last_emit = Some(Instant::now());
        }
        if closed {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn claimed(file_id: i64) -> Claimed {
        Claimed {
            file_id,
            root_id: 1,
            rel_path: "a".into(),
            path: "a".into(),
            name: "a".into(),
            ext: "png".into(),
            size: 1,
            mtime: 1,
            volume_file_id: None,
            parts: 3,
        }
    }

    #[test]
    fn stale_results_released() {
        let copied = || Outcome::Copied {
            kind: "image",
            quick_hash: vec![1],
            content_hash: vec![1],
            space_id: "s".into(),
            slice_version: "v".into(),
            from: 9,
        };
        let failed = || Outcome::Failed {
            quick_hash: None,
            error: "坏了。".into(),
        };
        let mut stale = vec![1, 2, 3];
        let mut batch = vec![
            (
                claimed(1),
                Outcome::Embedding {
                    quick_hash: vec![1],
                },
            ),
            (claimed(1), copied()),
            (claimed(2), failed()),
            (claimed(4), copied()),
        ];
        drop_stale(&mut stale, &mut batch);
        assert!(matches!(batch[0].1, Outcome::Embedding { .. }));
        assert_eq!(batch[1].1, Outcome::Released);
        assert!(matches!(batch[2].1, Outcome::Failed { .. }));
        assert!(matches!(batch[3].1, Outcome::Copied { .. }));
        // 最终结果都移出，没有结果的留着
        assert_eq!(stale, [3]);
        // 重新领取后的新结果照常写入
        let mut batch = vec![(claimed(1), copied())];
        drop_stale(&mut stale, &mut batch);
        assert!(matches!(batch[0].1, Outcome::Copied { .. }));
    }

    #[test]
    fn eta_needs_enough_records_and_span() {
        let t0 = Instant::now();
        let mut r = Recent::default();
        assert_eq!(r.eta(t0, 100), None);
        // 跨度够但条数不足
        for i in 0..9 {
            r.push(t0 + secs(i * 2));
        }
        assert_eq!(r.eta(t0 + secs(20), 100), None);
        // 条数够但跨度不足
        let mut r = Recent::default();
        for _ in 0..20 {
            r.push(t0);
        }
        assert_eq!(r.eta(t0 + secs(5), 100), None);
    }

    #[test]
    fn eta_from_rate() {
        let t0 = Instant::now();
        let mut r = Recent::default();
        // 20 秒内 10 条：每秒 0.5 条，剩 100 条需 200 秒
        for i in 0..10 {
            r.push(t0 + secs(i * 2));
        }
        assert_eq!(r.eta(t0 + secs(20), 100), Some(200));
        assert_eq!(r.eta(t0 + secs(20), 0), Some(0));
        // 超过上限时只留最近的 ETA_MAX 条
        let mut r = Recent::default();
        for _ in 0..ETA_MAX + 50 {
            r.push(t0);
        }
        assert_eq!(r.0.len(), ETA_MAX);
    }

    #[test]
    fn eta_when_full_despite_short_span() {
        let t0 = Instant::now();
        let mut r = Recent::default();
        // 8 秒内完成 2000 个：只留最近 1000 个（4～8 秒），跨度不足 10 秒也给出估计
        for i in 0..2000 {
            r.push(t0 + Duration::from_millis(i * 4));
        }
        assert_eq!(r.0.len(), ETA_MAX);
        // 1000 个用了 4 秒（从第 1000 个的 4 秒到现在的 8 秒），剩 2500 个需 10 秒
        assert_eq!(r.eta(t0 + secs(8), 2500), Some(10));
        // 不满 1000 条时仍要求跨度
        r.0.pop_front();
        assert_eq!(r.eta(t0 + secs(8), 2500), None);
    }

    #[test]
    fn old_records_slide_out() {
        let t0 = Instant::now();
        let mut r = Recent::default();
        // 前 10 条在开头，后 10 条在 310～328 秒
        for i in 0..10 {
            r.push(t0 + secs(i));
        }
        for i in 0..10 {
            r.push(t0 + secs(310 + i * 2));
        }
        // 第 330 秒时开头的 10 条已超过 300 秒，只按后 10 条算：20 秒 10 条
        assert_eq!(r.eta(t0 + secs(330), 10), Some(20));
        assert_eq!(r.0.len(), 10);
        // 全部滑出后不给出估计
        assert_eq!(r.eta(t0 + secs(700), 10), None);
        assert!(r.0.is_empty());
    }

    #[test]
    fn cleared_after_pause() {
        let t0 = Instant::now();
        let mut r = Recent::default();
        for i in 0..10 {
            r.push(t0 + secs(i * 2));
        }
        assert!(r.eta(t0 + secs(20), 10).is_some());
        r.clear();
        assert_eq!(r.eta(t0 + secs(20), 10), None);
        // 恢复后重新积累，不含暂停的时间
        for i in 0..10 {
            r.push(t0 + secs(100 + i * 2));
        }
        assert_eq!(r.eta(t0 + secs(120), 10), Some(20));
    }
}
