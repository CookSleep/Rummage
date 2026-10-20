//! 实时监听与定期对账。
//! - 每个在线根目录一个递归监听（`notify`），由 `sync` 启停：添加、移除根目录时，以及每个扫描任务结束后（离线与恢复）。
//! - 事件只用来收集要局部对账的目录（`rummage_index::dirty_of`）：最后一个事件后 2 秒、且从第一个事件起最多 10 秒，
//!   把这批目录交给扫描线程（`ScanJob::Dirs`）。扫描线程忙时（启动对账、上一批还没做完）继续合并，空闲后再交，不并发扫描。
//!   监听报错、事件缓冲区溢出（`need_rescan`）或事件涉及根目录本身时，改为对该根目录完整对账。
//! - 数据目录在根目录之内时，其中的事件直接丢弃：扫描本来就排除它，写库、写缩略图不应引起对账。
//! - 同一线程每 30 秒检查一次离线的根目录能否访问（只列根目录，不读文件），能访问就完整对账，扫描结束后 `sync` 恢复监听；
//!   每隔 `rescan_hours` 小时对全部根目录完整对账一次（启动对账算第一次）。
//! - 线程只持有 `State` 的 `Weak`，State 释放后退出。
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{MAIN_SEPARATOR, Path};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use notify::event::{EventKind, ModifyKind};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use rummage_index::Dirty;
use rummage_proto::params;

use super::{ScanJob, State, lock};

/// 最后一个事件之后等这么久再对账
const QUIET: Duration = Duration::from_secs(2);
/// 一批事件从第一个起最多等这么久，持续不断的变化（如复制大量文件）也能及时反映
const MAX_WAIT: Duration = Duration::from_secs(10);
/// 离线根目录的检查间隔，也是检查定期对账是否到期的间隔
const OFFLINE_EVERY: Duration = Duration::from_secs(30);
const TICK: Duration = Duration::from_millis(500);

type Msg = (i64, notify::Result<Event>);

pub(super) struct Watches {
    /// 根目录 id → (库中的路径, 监听)
    map: Mutex<HashMap<i64, (String, RecommendedWatcher)>>,
    tx: Sender<Msg>,
    /// 小写的数据目录路径
    data_dir: String,
}

/// 等待交给扫描线程的一批变化
#[derive(Default)]
struct Batch {
    /// 要完整对账的根目录
    full: BTreeSet<i64>,
    /// 根目录 id → 相对路径 → 是否含子树
    dirs: BTreeMap<i64, HashMap<String, bool>>,
    first: Option<Instant>,
    last: Option<Instant>,
}

impl Batch {
    fn touch(&mut self, now: Instant) {
        self.first.get_or_insert(now);
        self.last = Some(now);
    }
}

/// 启动监听线程并监听全部在线根目录；由 `start_scanner` 调用
pub(super) fn start(state: &Arc<State>, data_dir: &Path) {
    let (tx, rx) = mpsc::channel();
    // 与扫描相同，按规范化后的路径比较（`rummage_index::scan`）
    let data_dir = {
        let d = data_dir
            .canonicalize()
            .unwrap_or_else(|_| data_dir.to_path_buf());
        let s = d.to_string_lossy();
        s.strip_prefix(r"\\?\").unwrap_or(&s).to_lowercase()
    };
    let watches = Watches {
        map: Mutex::new(HashMap::new()),
        tx,
        data_dir,
    };
    if state.watches.set(watches).is_err() {
        return;
    }
    let weak = Arc::downgrade(state);
    let spawned = std::thread::Builder::new()
        .name("watch".into())
        .spawn(move || run(&weak, &rx));
    if let Err(err) = spawned {
        log::error!("无法启动监听线程：{err}");
        return;
    }
    sync(state);
}

/// 让监听与在线的根目录一致
pub(super) fn sync(state: &State) {
    let (Some(w), Ok(core)) = (state.watches.get(), &state.db) else {
        return;
    };
    let roots = match lock(&core.db).root_states() {
        Ok(roots) => roots,
        Err(err) => {
            log::warn!("无法读取根目录列表，暂不更新监听：{err}");
            return;
        }
    };
    let mut map = lock(&w.map);
    map.retain(|id, (path, _)| roots.iter().any(|r| r.0 == *id && r.1 == *path && r.2 == 0));
    for (id, path, offline) in roots {
        if offline != 0 || map.contains_key(&id) {
            continue;
        }
        let tx = w.tx.clone();
        let res = notify::recommended_watcher(move |res| {
            // 接收端只在 State 释放后才关闭
            let _ = tx.send((id, res));
        })
        .and_then(|mut watcher| {
            watcher
                .watch(Path::new(&path), RecursiveMode::Recursive)
                .map(|()| watcher)
        });
        match res {
            Ok(watcher) => {
                map.insert(id, (path, watcher));
            }
            Err(err) => log::warn!("无法监听文件夹 {path} 的变化：{err}"),
        }
    }
}

fn run(weak: &Weak<State>, rx: &Receiver<Msg>) {
    let mut batch = Batch::default();
    let mut polled = Instant::now();
    let mut rescanned = Instant::now();
    loop {
        let msg = match rx.recv_timeout(TICK) {
            Ok(msg) => Some(msg),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let Some(state) = weak.upgrade() else {
            return;
        };
        if let Some((id, res)) = msg {
            collect(&state, &mut batch, id, res);
        }
        let now = Instant::now();
        let due = batch.first.is_some_and(|t| now - t >= MAX_WAIT)
            || batch.last.is_some_and(|t| now - t >= QUIET);
        if due && !state.scan_busy() {
            flush(&state, std::mem::take(&mut batch));
        }
        // 定期对账也在这里检查，不必每个事件都读参数
        if now - polled < OFFLINE_EVERY || state.scan_busy() {
            continue;
        }
        polled = now;
        poll_offline(&state);
        let hours = params::int(&state.params(), "rescan_hours").max(1) as u64;
        if now - rescanned >= Duration::from_secs(hours * 3600) && !state.scan_busy() {
            rescanned = now;
            state.queue_scan(ScanJob::All);
        }
    }
}

fn collect(state: &State, batch: &mut Batch, id: i64, res: notify::Result<Event>) {
    let Some(w) = state.watches.get() else {
        return;
    };
    // 已停止监听的根目录（刚移除或离线）
    let Some(root) = lock(&w.map).get(&id).map(|(p, _)| p.clone()) else {
        return;
    };
    let now = Instant::now();
    let event = match res {
        Ok(e) if e.need_rescan() => {
            log::warn!("监听 {root} 的事件过多、有遗漏，完整对账一次。");
            batch.full.insert(id);
            return batch.touch(now);
        }
        Ok(e) => e,
        Err(err) => {
            log::warn!("监听 {root} 时出错，完整对账一次：{err}");
            batch.full.insert(id);
            return batch.touch(now);
        }
    };
    if matches!(event.kind, EventKind::Access(_)) {
        return;
    }
    let modified = matches!(event.kind, EventKind::Modify(k) if !matches!(k, ModifyKind::Name(_)));
    for path in &event.paths {
        let data = &w.data_dir;
        let p = path.to_string_lossy().to_lowercase();
        if p == *data || p.starts_with(&format!("{data}{MAIN_SEPARATOR}")) {
            continue;
        }
        let Ok(rel) = path.strip_prefix(&root) else {
            continue;
        };
        if rel.as_os_str().is_empty() {
            batch.full.insert(id);
            batch.touch(now);
            continue;
        }
        // 名称不是 Unicode 的条目扫描也跳过
        let Some(rel) = rel.to_str() else {
            continue;
        };
        if let Some(d) = rummage_index::dirty_of(path, rel, modified) {
            *batch.dirs.entry(id).or_default().entry(d.rel).or_default() |= d.recursive;
            batch.touch(now);
        }
    }
}

fn flush(state: &State, batch: Batch) {
    for id in &batch.full {
        state.queue_scan(ScanJob::Root(*id));
    }
    let dirs: BTreeMap<i64, Vec<Dirty>> = batch
        .dirs
        .into_iter()
        .filter(|(id, _)| !batch.full.contains(id))
        .map(|(id, m)| {
            let dirs = m
                .into_iter()
                .map(|(rel, recursive)| Dirty { rel, recursive })
                .collect();
            (id, dirs)
        })
        .collect();
    if !dirs.is_empty() {
        state.queue_scan(ScanJob::Dirs(dirs));
    }
}

/// 离线的根目录能访问了就完整对账；仍不能访问时更新原因（设备连上了、文件夹仍不存在）
fn poll_offline(state: &State) {
    let Ok(core) = &state.db else {
        return;
    };
    let roots = match lock(&core.db).root_states() {
        Ok(roots) => roots,
        Err(err) => {
            log::warn!("无法读取根目录列表，暂不检查离线的文件夹：{err}");
            return;
        }
    };
    for (id, path, offline) in roots {
        if offline == 0 {
            continue;
        }
        if rummage_index::root_reachable(Path::new(&path)) {
            log::info!("文件夹 {path} 可以访问了，开始对账。");
            state.queue_scan(ScanJob::Root(id));
            continue;
        }
        let now = rummage_index::offline_state(Path::new(&path));
        if now != offline
            && let Err(err) = lock(&core.db).set_root_offline(id, now)
        {
            log::warn!("无法记下文件夹 {path} 不可访问的原因：{err}");
        }
    }
}
