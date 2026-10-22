//! 索引：SQLite 数据库的模式、迁移与类型化访问（`db`），网盘占位文件与卷检查（`fsx`），
//! 根目录管理（`roots`），扫描与对账（`scan`），文件哈希（`hash`），索引作业队列（`jobs`），设置改了以后找出要重建的文件（`rebuild`），
//! 内存向量索引（`vindex`）与检索（`search`），分数校准的背景分布（`calib`），按 ID 读取单个文件与片段（`files`），失败文件的列表与重试（`failures`），搜索历史（`history`），缩略图的引用（`thumbs`）
pub mod calib;
pub mod db;
pub mod failures;
pub mod files;
pub mod fsx;
pub mod hash;
pub mod history;
pub mod jobs;
pub mod rebuild;
pub mod roots;
pub mod scan;
pub mod search;
pub mod thumbs;
pub mod vindex;

#[cfg(not(windows))]
#[path = "other.rs"]
mod platform;
#[cfg(windows)]
#[path = "windows.rs"]
mod platform;

pub use calib::{CalibError, QueryKind, Rng, TextQueries};
pub use db::{Db, DbError, NOTICE_DB_RESTORED, Space, blob_to_vec, vec_to_blob};
pub use failures::{FailedFile, FailedPage};
pub use files::{FileRow, IndexInfo, JobRow, SegmentRow};
pub use fsx::{FsError, IoCause, check_volume, ensure_local, io_cause, io_kind};
pub use hash::{HashError, content_hash, quick_hash, quick_hash_with_id};
pub use jobs::{
    Applied, Claimed, ENCRYPTED, JobCounts, MAX_ATTEMPTS, NewSegment, Outcome, PART_MEDIA,
    PART_TEXT,
};
pub use rebuild::{MEDIA_EXTS, Wanted};
pub use roots::{Root, RootError, normalize_root};
pub use scan::{
    Dirty, ScanError, ScanLimits, ScanStats, dirty_of, kind_of, offline_state, root_reachable,
    scan_dirs, scan_root, wants_job,
};
pub use search::{
    Cancel, Filter, Hit, Query, SearchParams, SegmentHit, comparable, search, stored_vector,
};
pub use vindex::{VIndex, quantize};

/// 取得锁；持锁的线程 panic 过也照常使用
pub(crate) fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
