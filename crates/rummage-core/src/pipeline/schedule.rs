//! 调度线程：按优先级领取一个 `pending` 作业（`Db::claim_job`，置为 `preprocessing`），
//! 交给空闲的预处理线程。`hold`、暂停或睡眠时不领取新作业，也不为建索引创建推理引擎；没有作业时等待唤醒，
//! 另每 60 秒兜底检查一次，不轮询数据库。领取在数据库锁内进行，并在锁内检查停止标志：放回之后不会再领出新作业。
//! 作业所用的参数（`Task::params`）也在这把锁内取出：核心修改参数时持有数据库锁，所以领取在修改之前的，作业在
//! `Pipeline::requeue` 时已在处理中、记入 `stale`，结果作废；在修改之后的，取到的就是新参数。
//! 降温（`Shared::slow`）时领取一个作业后先等处理中的作业全部完成，再休息与之相同的时长，占空比约 50%；
//! 等待期间降温解除或停止时立即继续。
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

use super::{Msg, Shared, Task, lock};

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
        // 暂停或睡眠时不领取新作业，先于创建引擎检查：暂停时丢弃的引擎不在这里又建出来
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
        let Some(engine) = shared.engine(out) else {
            shared.busy.store(false, Ordering::Relaxed);
            waited = true;
            shared.wait();
            continue;
        };
        // 检查之后、建引擎之前刚暂停：`set_paused` 丢弃时槽还是空的，刚建的引擎在这里丢弃
        if lock(&shared.paused).is_some() {
            let mut slot = lock(&shared.engine);
            if slot.as_ref().is_some_and(|e| Arc::ptr_eq(e, &engine)) {
                shared.retire(&mut slot);
            }
            continue;
        }
        let claimed = {
            let db = lock(&shared.db);
            if shared.stop.load(Ordering::Relaxed) {
                return;
            }
            db.claim_job(engine.images, engine.audio, engine.video)
                .map(|job| job.map(|j| (j, lock(&shared.params).clone())))
        };
        let (job, params) = match claimed {
            Ok(Some(claimed)) => claimed,
            Ok(None) => {
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
            params,
        };
        // 预处理线程都已退出
        if tasks.send(task).is_err() {
            return;
        }
        if shared.slow.load(Ordering::Relaxed) {
            cool_down(shared);
        }
    }
}

/// 降温：等处理中的作业完成，再休息同样长的时间（见模块说明）
fn cool_down(shared: &Shared) {
    let tick = Duration::from_millis(100);
    let going = || shared.slow.load(Ordering::Relaxed) && !shared.stop.load(Ordering::Relaxed);
    let started = Instant::now();
    while going() && shared.inflight.load(Ordering::Relaxed) > 0 {
        thread::sleep(tick);
    }
    let until = Instant::now() + started.elapsed();
    while going() {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        thread::sleep(left.min(tick));
    }
}
