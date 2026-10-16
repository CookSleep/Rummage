//! 扫描线程：按顺序处理排队的扫描任务，同一时间只做一个；启动时对全部根目录对账一次，
//! 进度以 `ScanProgress` 事件广播。监听到的变化以 `ScanJob::Dirs` 排进同一个线程做局部对账（`watch`）。
//! 线程只持有 `State` 的 `Weak`，扫描一个根目录期间才临时升级，发出结束事件前已释放。
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, MutexGuard, Weak, mpsc as std_mpsc};

use rummage_index::{Dirty, ScanError, ScanLimits};
use rummage_proto::{Event, ParamValue, ScanStats, params};
use tokio::sync::broadcast;

use super::{State, data, lock, watch};

pub(super) enum ScanJob {
    All,
    Root(i64),
    /// 监听到变化的目录，局部对账（`watch`）
    Dirs(BTreeMap<i64, Vec<Dirty>>),
    /// 全量清扫缩略图（`data::sweep_thumbs`），启动对账之后排一次
    SweepThumbs,
}

impl State {
    /// 启动扫描线程，并排队一次全部根目录的扫描（启动对账），然后开始监听（`watch`）；数据库不可用时不启动
    pub fn start_scanner(self: &Arc<Self>) {
        let Ok(core) = &self.db else {
            return;
        };
        let (tx, rx) = std_mpsc::channel();
        if self.scan_tx.set(tx).is_err() {
            return;
        }
        let weak = Arc::downgrade(self);
        let events = self.events.clone();
        // State 释放后发送端随之释放，recv 返回错误，线程退出
        let spawned = std::thread::Builder::new()
            .name("scan".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    scan_job(&weak, &events, job);
                    if let Some(state) = weak.upgrade() {
                        state.scan_pending.fetch_sub(1, Ordering::Relaxed);
                    }
                }
            });
        if let Err(err) = spawned {
            log::error!("无法启动扫描线程：{err}");
            return;
        }
        self.queue_scan(ScanJob::All);
        self.queue_scan(ScanJob::SweepThumbs);
        watch::start(self, &core.dir);
    }

    pub(super) fn queue_scan(&self, job: ScanJob) {
        if let Some(tx) = self.scan_tx.get() {
            self.scan_pending.fetch_add(1, Ordering::Relaxed);
            // 接收端只在 State 释放后才关闭
            let _ = tx.send(job);
        }
    }

    pub(super) fn scanning(&self) -> MutexGuard<'_, Option<(Vec<i64>, Arc<AtomicBool>)>> {
        lock(&self.scanning)
    }

    /// 还有排队或进行中的扫描
    pub fn scan_busy(&self) -> bool {
        self.scan_pending.load(Ordering::Relaxed) > 0
    }
}

fn proto_stats(s: &rummage_index::ScanStats) -> ScanStats {
    ScanStats {
        added: s.added,
        updated: s.updated,
        removed: s.removed,
        cloud: s.cloud,
        too_large: s.too_large,
        excluded: s.excluded,
    }
}

fn scan_job(weak: &Weak<State>, events: &broadcast::Sender<Event>, job: ScanJob) {
    let ids = match job {
        ScanJob::SweepThumbs => {
            if let Some(state) = weak.upgrade() {
                data::sweep_thumbs(&state);
            }
            return;
        }
        ScanJob::Root(id) => vec![id],
        ScanJob::Dirs(dirs) => {
            scan_dirs(weak, events, dirs);
            vec![]
        }
        ScanJob::All => {
            let Some(state) = weak.upgrade() else {
                return;
            };
            let Ok(core) = &state.db else {
                return;
            };
            match lock(&core.db).list_roots() {
                Ok(roots) => roots.iter().map(|r| r.id).collect(),
                Err(err) => {
                    log::error!("无法读取根目录列表，本次不扫描：{err}");
                    return;
                }
            }
        }
    };
    for id in ids {
        // 没有事件的情况：State 已释放，或根目录在排队期间被移除
        if let Some(stats) = scan_one(weak, events, id) {
            // 没有订阅者时 send 返回错误，忽略
            let _ = events.send(Event::ScanProgress {
                root_id: id,
                done: true,
                stats,
            });
        }
    }
    if let Some(state) = weak.upgrade() {
        data::drop_orphan_thumbs(&state);
        state.wake_pipeline();
        // 暂停或空闲时流水线不会自己广播，扫描新增、删除的计数要在这里更新
        if let Some(p) = state.pipeline.get() {
            p.broadcast();
        }
        watch::sync(&state);
    }
}

/// 参数表中的大小、时长、像素上限与开启内容索引的类型
fn scan_limits(values: &BTreeMap<String, ParamValue>) -> ScanLimits {
    let mb = |key: &str| params::int(values, key).max(0) as u64;
    let on = |key: &str| params::flag(values, key);
    ScanLimits {
        max_doc_mb: mb("max_doc_mb"),
        max_image_mb: mb("max_image_mb"),
        max_audio_min: mb("max_audio_min"),
        max_image_mp: mb("max_image_mp"),
        documents: on("index_documents"),
        images: on("index_images"),
        audio: on("index_audio"),
        video: on("index_video"),
    }
}

/// 局部对账一批监听到的变化，有变化的根目录广播结束事件。被 `RemoveRoot` 取消时整批重新排队，已移除的根目录届时跳过
fn scan_dirs(
    weak: &Weak<State>,
    events: &broadcast::Sender<Event>,
    dirs: BTreeMap<i64, Vec<Dirty>>,
) {
    let Some(state) = weak.upgrade() else {
        return;
    };
    let Ok(core) = &state.db else {
        return;
    };
    let cancel = Arc::new(AtomicBool::new(false));
    *state.scanning() = Some((dirs.keys().copied().collect(), cancel.clone()));
    let res = rummage_index::scan_dirs(&core.db, &dirs, scan_limits(&state.params()), &cancel);
    *state.scanning() = None;
    match res {
        Ok(list) => {
            for (id, s) in list {
                if s.added + s.updated + s.removed == 0 {
                    continue;
                }
                let _ = events.send(Event::ScanProgress {
                    root_id: id,
                    done: true,
                    stats: proto_stats(&s),
                });
            }
        }
        Err(ScanError::Cancelled) => state.queue_scan(ScanJob::Dirs(dirs)),
        Err(err) => log::error!("对账监听到的变化时出错：{err}"),
    }
}

/// 扫描一个根目录，返回最终统计（离线、出错、取消时为已有的统计）；返回时已释放对 State 的强引用
fn scan_one(weak: &Weak<State>, events: &broadcast::Sender<Event>, id: i64) -> Option<ScanStats> {
    let state = weak.upgrade()?;
    let Ok(core) = &state.db else {
        return None;
    };
    let cancel = Arc::new(AtomicBool::new(false));
    *state.scanning() = Some((vec![id], cancel.clone()));
    let limits = scan_limits(&state.params());
    let mut last = rummage_index::ScanStats::default();
    let res = rummage_index::scan_root(&core.db, id, limits, &cancel, &mut |s| {
        last = *s;
        let _ = events.send(Event::ScanProgress {
            root_id: id,
            done: false,
            stats: proto_stats(s),
        });
    });
    *state.scanning() = None;
    match res {
        Ok(s) => last = s,
        Err(ScanError::NoRoot) => return None,
        Err(ScanError::Cancelled) => {}
        Err(err) => log::error!("扫描根目录 {id} 时出错：{err}"),
    }
    Some(proto_stats(&last))
}
