//! 调度线程：按优先级领取一个 `pending` 作业（`Db::claim_job`，置为 `preprocessing`），
//! 交给空闲的预处理线程。`hold`、暂停或睡眠时不领取新作业，已领取的照常处理完；没有作业时等待唤醒，
//! 另每 60 秒兜底检查一次，不轮询数据库。领取在数据库锁内进行，并在锁内检查停止标志：放回之后不会再领出新作业。
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Sender};

use rummage_index::{Claimed, Db, DbError, Outcome};

use super::{Engine, Msg, Shared, Task, int, lock};

pub(super) enum Claim {
    Job(Claimed),
    /// 没有可领取的作业
    Empty,
    /// 领到了，但引擎的视觉 token 档位已与参数不一致，作业已放回
    Stale,
}

/// 领取一个作业，调用方持有数据库锁。调度线程先取引擎、后领取：两步之间若改了 `image_tokens`，`requeue` 时这个作业
/// 还没领出、不会记入 `stale`，用旧档位的引擎算出的结果会写成已完成。所以在锁内核对档位，不一致就放回、不计失败
pub(super) fn claim(db: &Db, shared: &Shared, engine: &Engine) -> Result<Claim, DbError> {
    let Some(job) = db.claim_job(engine.images, engine.audio, engine.video)? else {
        return Ok(Claim::Empty);
    };
    if int(&lock(&shared.params), "image_tokens") as u32 == engine.tier {
        return Ok(Claim::Job(job));
    }
    db.apply(&[(job, Outcome::Released)])?;
    Ok(Claim::Stale)
}

pub(super) fn schedule(shared: &Shared, tasks: &mpsc::SyncSender<Task>, out: &Sender<Msg>) {
    // 上次循环停下等待过（暂停、睡眠、空闲等）；再领到作业时清空剩余时间的估算窗口，不把停下的时间算进速度
    let mut waited = false;
    while !shared.stop.load(Ordering::Relaxed) {
        if shared.held() {
            if shared.busy.swap(false, Ordering::Relaxed) {
                let _ = out.send(Msg::Progress);
            }
            waited = true;
            shared.wait();
            continue;
        }
        let Some(engine) = shared.engine(out) else {
            shared.busy.store(false, Ordering::Relaxed);
            waited = true;
            shared.wait();
            continue;
        };
        // 暂停或睡眠时不领取新作业；已领取的照常处理完
        // 分开读取：锁的顺序是先 paused 后 sleep，这里不能同时持有。代数在领取前读出，
        // 之后才挂起时该作业算作跨过睡眠
        let (held, generation) = {
            let s = lock(&shared.sleep);
            (s.held(), s.generation())
        };
        if held || lock(&shared.paused).is_some() {
            if shared.busy.swap(false, Ordering::Relaxed) {
                let _ = out.send(Msg::Progress);
            }
            waited = true;
            shared.wait();
            continue;
        }
        let claimed = {
            let db = lock(&shared.db);
            if shared.stop.load(Ordering::Relaxed) {
                return;
            }
            claim(&db, shared, &engine)
        };
        let job = match claimed {
            Ok(Claim::Job(job)) => job,
            // 下一轮按新参数重建引擎后再领
            Ok(Claim::Stale) => continue,
            Ok(Claim::Empty) => {
                if shared.busy.swap(false, Ordering::Relaxed) {
                    let _ = out.send(Msg::Progress);
                }
                waited = true;
                shared.wait();
                continue;
            }
            Err(err) => {
                log::error!("无法读取索引作业：{err}");
                waited = true;
                shared.wait();
                continue;
            }
        };
        if waited {
            lock(&shared.recent).clear();
            waited = false;
        }
        shared.busy.store(true, Ordering::Relaxed);
        shared.inflight.fetch_add(1, Ordering::Relaxed);
        let task = Task {
            job,
            engine,
            generation,
            params: lock(&shared.params).clone(),
        };
        // 预处理线程都已退出
        if tasks.send(task).is_err() {
            return;
        }
    }
}
