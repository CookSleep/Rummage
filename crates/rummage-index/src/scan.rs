//! 扫描与对账。
//! - 两种范围：`scan_root` 对账整个根目录（启动、定期、添加文件夹、监听溢出时）；`scan_dirs` 只对账监听到变化的目录
//!   （`Dirty`，由 `dirty_of` 从事件路径得出），可同时涉及多个根目录。两者共用同一遍历（`walk`）：局部对账也从根目录
//!   往下走，只进入目标目录及其上级，上级照常套排除规则，范围外的文件既不读取也不当作消失。
//! - 遍历只用 `fs::read_dir`，文件的大小、修改时间和属性只取自 `DirEntry::metadata()`：Windows 上它来自目录枚举
//!   （`FindNextFileW`）的数据，不打开文件。**不要对文件调用 `fs::metadata` 或打开文件**：打开带 `RECALL_ON_OPEN`
//!   的网盘占位文件会触发下载。读取文件内容的步骤一律先调用 `fsx::ensure_local`。
//! - 不跟随符号链接和目录联接（`FileType::is_symlink` 在 Windows 上包括这两种名称代理重解析点；OneDrive 文件夹
//!   也带重解析点但不是名称代理，照常进入）；带 `RECALL_ON_OPEN` 的目录不进入。
//! - 排除规则见 `exclude` 模块。
//! - 对账按“大小 + 修改时间”：新文件插入（`pending`），变化的清空哈希并把作业重置为 `pending`，消失的删除
//!   （级联删除片段、向量、作业），未变化的不动。
//! - 改名与移动（`pair`）：消失的、有向量的记录先与同一次对账中新出现的文件配对（大小、扩展名相同，再比文件 ID 或
//!   `quick_hash`），配对成功就只改原记录的路径，不重算向量。可能配对的新文件（局部对账的全部新文件；整个根目录对账时
//!   与有向量的记录大小相同的）留到遍历结束后再写入，免得先被索引流水线领走。整个根目录对账只在该根目录内配对，
//!   程序未运行期间跨根目录的移动按删除 + 新增处理。文件 ID 由索引流水线算 `quick_hash` 时顺便记下（`hash`），
//!   遍历本身不打开文件。只为 `wants_job` 的文件建作业（语义索引范围：图片、
//!   文档、音频、视频，以及要判断是否为视频的 `.ts` / `.mts`），并且该类在引导或设置中开启（`ScanLimits::documents` / `images` / `audio` / `video`）。
//!   关闭的类只按文件名索引；开关改变后重新扫描时，未变化的文件按新开关补建或删除未完成的作业，
//!   已完成索引（`stored`）的保留向量，与修改大小上限的处理相同。
//! - `other` 的 `status` 为 `pending` 但没有作业，索引进度要按作业计。
//! - 占位文件 `cloud = 1`、`status = 'cloud'`，不建作业；已有向量、内容未变的文件变成占位文件时保留向量和已完成的
//!   作业，重新下载回本机后恢复为 `stored`。内容变了且不再建作业（变成占位文件或超过大小上限）时，
//!   删除旧片段（级联删除向量），旧向量已不代表文件内容。
//! - 文档、图片超过大小上限时 `status = 'too_large'`，不建作业；修改上限后重新扫描时，未完成索引的文件按新上限
//!   重新判定，已完成索引（`stored`）的保持不变。
//! - 音频与视频不限大小、限时长（共用 `max_audio_min`）：时长由索引流水线用 ffprobe 读出，写在 `meta_json` 的 `duration_ms` 中，超过上限的
//!   `status = 'too_large'`。扫描不读文件，内容未变时按库中记下的时长与当前上限重新判定，改了上限照样生效；
//!   没有记下时长的（新文件、内容变了）先建作业，由流水线判定。内容变化时清空 `meta_json`，旧元数据已不代表文件。
//!   图片的像素数同样由流水线判定，超过 `max_image_mp` 的 `status = 'too_large'`，`meta_json` 的 `pixel_limit` 记下当时的上限；
//!   内容未变、当前上限不高于它时仍算超过，调高后重新建作业。
//! - 根目录不可访问时标为离线，保留全部记录；可访问后恢复并照常对账。
//! - 数据库锁只在每批写入时持有（每 1000 个文件一个事务），遍历磁盘时不占锁；每批写入和删除前在锁内确认根目录
//!   仍在（id 与路径都相同），否则按取消处理。子目录读不出或不进入（`RECALL_ON_OPEN`）时保留其下已有记录，
//!   取消时不删除任何记录。
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, DirEntry};
use std::path::{MAIN_SEPARATOR, MAIN_SEPARATOR_STR, Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use rusqlite::{Connection, OptionalExtension, params};

use crate::db::{Db, DbError};
use crate::fsx::{
    self, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_RECALL_ON_OPEN,
    FILE_ATTRIBUTE_SYSTEM,
};
use crate::{lock, platform};

mod exclude;
mod pair;
mod write;

use exclude::{BUILD_DIRS, EXCLUDED_DIRS, PROJECT_EXTS, PROJECT_FILES, TEMP_SUFFIXES, user_globs};
pub(crate) use exclude::{exact_rule, exclude_glob, slashes};
use write::{flush, root_exists};

/// 每批写入的文件数，也是进度回调的间隔
const BATCH: usize = 1000;

/// 种类 → 扩展名（小写、不含点），覆盖支持的全部格式；不在表中的为 `other`。
/// `.ts` / `.mts` 也常是 TypeScript 代码：先记为 `other`，开启视频时建作业，由索引流水线读文件头判断是否为 MPEG-TS 视频，
/// 是视频就改为 `video`，否则仍为 `other` 并记下 `quick_hash` 表示已判定（`jobs::Outcome::NotVideo`）。
/// `.m2ts` 几乎只用于蓝光和摄像机录像，直接记为视频
pub const KINDS: &[(&str, &[&str])] = &[
    (
        "document",
        &[
            "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "rtf", "txt", "md",
            "html", "htm", "xml", "csv", "json",
        ],
    ),
    (
        "image",
        &[
            "jpg", "jpeg", "png", "webp", "bmp", "tif", "tiff", "gif", "heic", "heif", "avif",
        ],
    ),
    (
        "audio",
        &[
            "mp3", "m4a", "aac", "wav", "flac", "ogg", "opus", "wma", "amr",
        ],
    ),
    (
        "video",
        &[
            "mp4", "mov", "mkv", "webm", "avi", "wmv", "flv", "3gp", "m2ts",
        ],
    ),
];

/// 做语义索引、要建作业的文件：图片、文档（文档中只有 `.txt`、`.md` 直接读取，其余经 Xberg 提取）、音频与视频，
/// 以及要判断是否为视频的 `.ts` / `.mts`（见 `KINDS`）。扫描与索引流水线共用；`ext` 为小写、不含点的扩展名
pub fn wants_job(kind: &str, ext: &str) -> bool {
    matches!(kind, "image" | "document" | "audio" | "video") || (kind == "other" && is_ts(ext))
}

/// 可能是 MPEG-TS 视频、也可能是 TypeScript 代码的扩展名（见 `KINDS`）
pub(crate) fn is_ts(ext: &str) -> bool {
    ext == "ts" || ext == "mts"
}

/// `ext` 为小写、不含点的扩展名
pub fn kind_of(ext: &str) -> &'static str {
    KINDS
        .iter()
        .find(|(_, exts)| exts.contains(&ext))
        .map_or("other", |(kind, _)| kind)
}

/// 作业优先级，数值大的先处理（图片 → 文档 → 音频 → 视频，同类中修改时间越新越先）：
/// `priority = 档位 × 2^42 + clamp(mtime 毫秒, 0, 2^42 − 1)`，档位为图片 2、文档（及待判定的 `.ts` / `.mts`）1、音频 0、视频 −1。
/// 2^42 毫秒约 139 年，1970–2109 年之间的修改时间不会越过档位
pub fn priority(kind: &str, mtime: i64) -> i64 {
    let tier: i64 = match kind {
        "image" => 2,
        "audio" => 0,
        "video" => -1,
        _ => 1,
    };
    (tier << 42) + mtime.clamp(0, (1 << 42) - 1)
}

/// 一次扫描的统计。`cloud`、`too_large`、`excluded` 是本次遇到的数量（含未变化的），
/// `excluded` 包括被排除的目录（按一个计）和文件
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanStats {
    pub added: u64,
    pub updated: u64,
    pub removed: u64,
    pub cloud: u64,
    pub too_large: u64,
    pub excluded: u64,
}

/// 参数表中的 `max_doc_mb`、`max_image_mb`、`max_audio_min`（音频与视频共用），以及开启内容索引的类型
/// `index_documents`、`index_images`、`index_audio`、`index_video`
#[derive(Debug, Clone, Copy)]
pub struct ScanLimits {
    pub max_doc_mb: u64,
    pub max_image_mb: u64,
    pub max_audio_min: u64,
    pub max_image_mp: u64,
    pub documents: bool,
    pub images: bool,
    pub audio: bool,
    pub video: bool,
}

/// 测试用：参数表的默认上限，所有类型都开启
#[cfg(test)]
pub(crate) const TEST_LIMITS: ScanLimits = ScanLimits {
    max_doc_mb: 200,
    max_image_mb: 100,
    max_audio_min: 240,
    max_image_mp: 100,
    documents: true,
    images: true,
    audio: true,
    video: true,
};

impl ScanLimits {
    /// 该类文件开启了内容索引，且属于语义索引范围（`wants_job`）
    fn wants(&self, kind: &str, ext: &str) -> bool {
        let on = match kind {
            "image" => self.images,
            "audio" => self.audio,
            // 待判定的 `.ts` / `.mts` 只有处理视频时才需要判断
            "video" | "other" => self.video,
            "document" => self.documents,
            _ => false,
        };
        on && wants_job(kind, ext)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("找不到该文件夹，它可能已被移除。")]
    NoRoot,
    #[error("扫描已取消。")]
    Cancelled,
    #[error(transparent)]
    Db(#[from] DbError),
}

/// 库中已有的记录
struct Old {
    id: i64,
    /// 库中的种类：`.ts` / `.mts` 被判定为视频后为 `video`
    kind: String,
    size: i64,
    mtime: i64,
    cloud: bool,
    status: String,
    too_large: bool,
    /// 有作业（任何阶段）
    has_job: bool,
    /// 索引流水线记下的音频、视频时长（`meta_json` 的 `duration_ms`）
    duration_ms: Option<i64>,
    /// 索引流水线判定图片超过像素上限时所用的上限（`meta_json` 的 `pixel_limit`）
    pixel_limit: Option<i64>,
    /// 有片段（向量）：消失时参与改名与移动的配对
    segs: bool,
    /// 已算过 `quick_hash`：`other` 的 `.ts` / `.mts` 据此知道已判定不是视频（见 `KINDS`）
    hashed: bool,
}

/// 本次扫描看到的文件
struct Rec {
    rel: String,
    name: String,
    ext: String,
    kind: &'static str,
    size: i64,
    mtime: i64,
    cloud: bool,
    too_large: bool,
    /// 要建作业：不是占位文件、不超过上限，且该类开启了内容索引
    wants_job: bool,
}

impl Rec {
    fn status(&self) -> &'static str {
        if self.cloud {
            return "cloud";
        }
        if self.too_large {
            return "too_large";
        }
        "pending"
    }
}

/// 相对路径 `rel` 是否位于 `prefix`（或就是它）之下；空前缀表示整个根目录
fn under(rel: &str, prefix: &str) -> bool {
    prefix.is_empty()
        || rel == prefix
        || rel
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with(MAIN_SEPARATOR))
}

/// 局部对账的范围（监听到变化的目录）：`rel` 相对根目录，空串为根目录本身；`recursive` 为假时只看该目录的
/// 直接子项（及其中消失的），为真时含整个子树。路径已不存在时，库中其下的记录都按消失处理
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirty {
    pub rel: String,
    pub recursive: bool,
}

fn parent_of(rel: &str) -> &str {
    rel.rfind(MAIN_SEPARATOR).map_or("", |i| &rel[..i])
}

/// 文件 `rel` 在局部对账的范围内
fn file_in(scope: &[Dirty], rel: &str) -> bool {
    scope.iter().any(|d| {
        if d.recursive {
            under(rel, &d.rel)
        } else {
            parent_of(rel) == d.rel
        }
    })
}

/// 要进入目录 `rel`：它是某个范围的上级或就是它（经过上级时照常套排除规则），或在递归范围之内
fn dir_in(scope: &[Dirty], rel: &str) -> bool {
    scope
        .iter()
        .any(|d| under(&d.rel, rel) || (d.recursive && under(rel, &d.rel)))
}

/// 监听到变化的路径对应的局部对账范围。`rel` 为 `path` 相对根目录的路径，不为空；`modified` 表示内容或属性的
/// 修改（不是新建、删除、改名）。新建或改名得到的目录含整个子树（移进来的目录只报告目录本身）；目录本身的修改不必处理，
/// 其中的变化另有事件；文件看其所在目录的直接子项；已不存在的路径（删除或改名前的旧路径，不知道原来是文件还是目录）
/// 按子树处理。只读属性（`GetFileAttributesW`），不打开文件
pub fn dirty_of(path: &Path, rel: &str, modified: bool) -> Option<Dirty> {
    let dirty = |rel: &str, recursive| {
        Some(Dirty {
            rel: rel.to_string(),
            recursive,
        })
    };
    match platform::file_attributes(path) {
        Ok(a) if a & FILE_ATTRIBUTE_DIRECTORY == 0 => dirty(parent_of(rel), false),
        Ok(_) if modified => None,
        _ => dirty(rel, true),
    }
}

/// 根目录可以访问（据此判断离线）；只列目录，不读文件
pub fn root_reachable(root: &Path) -> bool {
    fs::read_dir(root).is_ok()
}

/// 一个根目录遍历完、写入了普通的新增与变化之后，等待配对的部分
struct Walk {
    root_id: i64,
    /// 库中的根目录路径
    root: String,
    stats: ScanStats,
    /// 消失的记录：(id, 可以配对：有片段且不是占位文件)
    gone: Vec<(i64, bool)>,
    /// 新出现、可能是改名或移动来的文件（不是占位文件），配对不上的再插入
    deferred: Vec<Rec>,
}

/// 扫描一个根目录并与库中记录对账。`progress` 在每处理 1000 个文件后以当前统计调用一次
pub fn scan_root(
    db: &Mutex<Db>,
    root_id: i64,
    limits: ScanLimits,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&ScanStats),
) -> Result<ScanStats, ScanError> {
    scan_with(db, root_id, limits, cancel, progress, &|_| 0)
}

/// 局部对账（监听）：只看 `dirs` 中各根目录的这些目录，改名与移动可跨根目录配对。返回各根目录的统计；
/// 已移除的根目录跳过
pub fn scan_dirs(
    db: &Mutex<Db>,
    dirs: &BTreeMap<i64, Vec<Dirty>>,
    limits: ScanLimits,
    cancel: &AtomicBool,
) -> Result<Vec<(i64, ScanStats)>, ScanError> {
    let mut walks = Vec::new();
    for (id, scope) in dirs {
        match walk(db, *id, Some(scope), limits, cancel, &mut |_| {}, &|_| 0) {
            Ok(w) => walks.push(w),
            Err(ScanError::NoRoot) => {}
            Err(err) => return Err(err),
        }
    }
    pair::settle(db, &mut walks, cancel)?;
    Ok(walks.into_iter().map(|w| (w.root_id, w.stats)).collect())
}

/// `extra_attrs` 按相对路径追加文件属性，测试用它模拟网盘占位文件，不必真的造云文件
fn scan_with(
    db: &Mutex<Db>,
    root_id: i64,
    limits: ScanLimits,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&ScanStats),
    extra_attrs: &dyn Fn(&str) -> u32,
) -> Result<ScanStats, ScanError> {
    let mut walks = vec![walk(
        db,
        root_id,
        None,
        limits,
        cancel,
        progress,
        extra_attrs,
    )?];
    pair::settle(db, &mut walks, cancel)?;
    Ok(walks[0].stats)
}

fn old_row(row: &rusqlite::Row) -> rusqlite::Result<(String, Old)> {
    let status: String = row.get(5)?;
    Ok((
        row.get(0)?,
        Old {
            id: row.get(1)?,
            kind: row.get(6)?,
            size: row.get(2)?,
            mtime: row.get(3)?,
            cloud: row.get(4)?,
            too_large: status == "too_large",
            status,
            has_job: row.get(7)?,
            // 不是整数（被改坏）时当作没有记下
            duration_ms: row.get::<_, Option<i64>>(8).unwrap_or(None),
            segs: row.get(9)?,
            hashed: row.get(10)?,
            pixel_limit: row.get::<_, Option<i64>>(11).unwrap_or(None),
        },
    ))
}

/// 库中的记录：整个根目录，或局部对账范围内的
fn known_files(
    conn: &Connection,
    root_id: i64,
    scope: Option<&[Dirty]>,
) -> rusqlite::Result<HashMap<String, Old>> {
    const SELECT: &str = "SELECT rel_path, id, size, mtime, cloud, status, kind,
           EXISTS (SELECT 1 FROM jobs j WHERE j.file_id = files.id),
           json_extract(meta_json, '$.duration_ms'),
           EXISTS (SELECT 1 FROM segments s WHERE s.file_id = files.id), quick_hash IS NOT NULL,
           json_extract(meta_json, '$.pixel_limit')
         FROM files WHERE root_id = ?1";
    let Some(scope) = scope else {
        return conn
            .prepare(SELECT)?
            .query_map([root_id], old_row)?
            .collect();
    };
    let mut known = HashMap::new();
    for d in scope {
        // 子树按路径范围取，用上 (root_id, rel_path) 的索引：分隔符的下一个字符作上界。只要直接子项时在 SQL 中排除更深的，
        // 免得根目录下一个文件变化就为整个根目录查一遍作业与片段
        let prefix = format!("{}{MAIN_SEPARATOR}", d.rel);
        let end = format!("{}{}", d.rel, char::from(MAIN_SEPARATOR as u8 + 1));
        let mut stmt;
        let rows = match (d.rel.is_empty(), d.recursive) {
            (true, true) => {
                stmt = conn.prepare_cached(SELECT)?;
                stmt.query_map(params![root_id], old_row)?
            }
            (true, false) => {
                stmt = conn.prepare_cached(&format!("{SELECT} AND instr(rel_path, ?2) = 0"))?;
                stmt.query_map(params![root_id, MAIN_SEPARATOR_STR], old_row)?
            }
            (false, true) => {
                stmt = conn.prepare_cached(&format!(
                    "{SELECT} AND (rel_path = ?4 OR (rel_path >= ?2 AND rel_path < ?3))"
                ))?;
                stmt.query_map(params![root_id, prefix, end, d.rel], old_row)?
            }
            (false, false) => {
                stmt = conn.prepare_cached(&format!(
                    "{SELECT} AND rel_path >= ?2 AND rel_path < ?3 AND instr(substr(rel_path, ?4), ?5) = 0"
                ))?;
                // substr 按字符计数，从 1 开始
                let start = prefix.chars().count() as i64 + 1;
                stmt.query_map(
                    params![root_id, prefix, end, start, MAIN_SEPARATOR_STR],
                    old_row,
                )?
            }
        };
        for row in rows {
            let (rel, old) = row?;
            if file_in(scope, &rel) {
                known.insert(rel, old);
            }
        }
    }
    Ok(known)
}

/// 遍历根目录（`scope` 为局部对账的范围），写入普通的新增与变化，返回消失的记录和等待配对的新文件
fn walk(
    db: &Mutex<Db>,
    root_id: i64,
    scope: Option<&[Dirty]>,
    limits: ScanLimits,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&ScanStats),
    extra_attrs: &dyn Fn(&str) -> u32,
) -> Result<Walk, ScanError> {
    let (root, settings, offline, data_dir, mut known) = {
        let db = lock(db);
        let row: Option<(String, String, bool)> = db
            .conn
            .query_row(
                "SELECT path, settings_json, offline FROM roots WHERE id = ?1",
                [root_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|err| db.err(err))?;
        let Some((root, settings, offline)) = row else {
            return Err(ScanError::NoRoot);
        };
        let data_dir = db
            .dir
            .canonicalize()
            .map_or_else(|_| fsx::shown(&db.dir), |d| fsx::shown(&d))
            .to_lowercase();
        let known = known_files(&db.conn, root_id, scope).map_err(|err| db.err(err))?;
        (root, settings, offline, data_dir, known)
    };
    let mut out = Walk {
        root_id,
        root: root.clone(),
        stats: ScanStats::default(),
        gone: Vec::new(),
        deferred: Vec::new(),
    };
    let set_offline = |value: bool| -> Result<(), ScanError> {
        let db = lock(db);
        db.conn
            .execute(
                "UPDATE roots SET offline = ?2 WHERE id = ?1",
                params![root_id, value],
            )
            .map_err(|err| db.err(err))?;
        Ok(())
    };

    let root_path = PathBuf::from(&root);
    // 移动硬盘断开、盘符变化等：标为离线，保留全部记录
    if !root_reachable(&root_path) {
        log::warn!("根目录 {root} 不可访问，标为离线。");
        if !offline {
            set_offline(true)?;
        }
        return Ok(out);
    }
    if offline {
        set_offline(false)?;
    }

    let globs = user_globs(&settings);
    let doc_limit = limits.max_doc_mb.saturating_mul(1024 * 1024);
    let image_limit = limits.max_image_mb.saturating_mul(1024 * 1024);
    let audio_limit_ms = limits.max_audio_min.saturating_mul(60_000);
    let pixel_limit = limits.max_image_mp.saturating_mul(1_000_000);
    // 整个根目录对账时，与有向量的记录大小相同的新文件才可能是改名或移动来的，留到最后配对；局部对账的新文件都留下
    let move_sizes: HashSet<i64> = known
        .values()
        .filter(|o| o.segs && !o.cloud)
        .map(|o| o.size)
        .collect();
    let stats = &mut out.stats;
    let mut batch: Vec<(Option<Old>, Rec)> = Vec::new();
    // 读不出来的目录或条目：其下已有的记录不能当作已删除
    let mut keep: Vec<String> = Vec::new();
    let mut seen = 0usize;
    let mut stack = vec![(root_path.clone(), String::new())];
    while let Some((dir, rel_dir)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return Err(ScanError::Cancelled);
        }
        let entries: Vec<DirEntry> = match fs::read_dir(&dir) {
            Ok(rd) => {
                let mut ok = Vec::new();
                for entry in rd {
                    match entry {
                        Ok(e) => ok.push(e),
                        Err(err) => {
                            log::warn!("读取目录 {} 时出错：{err}", dir.display());
                            keep.push(rel_dir.clone());
                        }
                    }
                }
                ok
            }
            Err(err) => {
                log::warn!("无法读取目录 {}：{err}", dir.display());
                keep.push(rel_dir.clone());
                continue;
            }
        };
        let has_project = entries.iter().any(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            PROJECT_FILES.iter().any(|p| p.eq_ignore_ascii_case(&name))
                || Path::new(&name)
                    .extension()
                    .is_some_and(|x| PROJECT_EXTS.iter().any(|p| x == *p))
        });
        for entry in entries {
            if cancel.load(Ordering::Relaxed) {
                return Err(ScanError::Cancelled);
            }
            let Ok(name) = entry.file_name().into_string() else {
                log::warn!(
                    "跳过名称无法转为 Unicode 的条目：{}",
                    entry.path().display()
                );
                stats.excluded += 1;
                continue;
            };
            let rel = if rel_dir.is_empty() {
                name.clone()
            } else {
                format!("{rel_dir}{MAIN_SEPARATOR_STR}{name}")
            };
            // 目录枚举的数据，不打开文件
            let (ft, meta) = match (entry.file_type(), entry.metadata()) {
                (Ok(ft), Ok(meta)) => (ft, meta),
                (Err(err), _) | (_, Err(err)) => {
                    log::warn!("无法读取 {} 的属性：{err}", entry.path().display());
                    keep.push(rel);
                    continue;
                }
            };
            if let Some(scope) = scope
                && !(if ft.is_dir() {
                    dir_in(scope, &rel)
                } else {
                    file_in(scope, &rel)
                })
            {
                continue;
            }
            if ft.is_symlink() {
                stats.excluded += 1;
                continue;
            }
            let attrs = platform::attrs(&meta) | extra_attrs(&rel);
            let glob_path = if MAIN_SEPARATOR == '\\' {
                rel.replace('\\', "/")
            } else {
                rel.clone()
            };
            if ft.is_dir() {
                let path = entry.path();
                // 按需下载的目录不进入，但它下面已有的记录（以前下载过时扫到的）不算删除
                if attrs & FILE_ATTRIBUTE_RECALL_ON_OPEN != 0 {
                    keep.push(rel);
                    stats.excluded += 1;
                    continue;
                }
                let skip = EXCLUDED_DIRS.iter().any(|d| d.eq_ignore_ascii_case(&name))
                    || (has_project && BUILD_DIRS.iter().any(|d| d.eq_ignore_ascii_case(&name)))
                    || fsx::shown(&path).to_lowercase() == data_dir
                    || globs.is_match(&name, &glob_path);
                if skip {
                    stats.excluded += 1;
                } else {
                    stack.push((path, rel));
                }
                continue;
            }
            let lower = name.to_lowercase();
            if attrs & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
                || name.starts_with("~$")
                || TEMP_SUFFIXES.iter().any(|s| lower.ends_with(s))
                || meta.len() == 0
                || globs.is_match(&name, &glob_path)
            {
                stats.excluded += 1;
                continue;
            }

            let ext = Path::new(&lower)
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_default();
            let kind = kind_of(&ext);
            let cloud = fsx::is_placeholder_attrs(attrs);
            let limit = match kind {
                "document" => doc_limit,
                "image" => image_limit,
                _ => u64::MAX,
            };
            // 早于 1970 年的修改时间记为负数
            let mtime = match meta.modified() {
                Ok(t) => match t.duration_since(UNIX_EPOCH) {
                    Ok(d) => d.as_millis() as i64,
                    Err(err) => -(err.duration().as_millis() as i64),
                },
                Err(_) => 0,
            };
            let size = i64::try_from(meta.len()).unwrap_or(i64::MAX);
            let old = known.remove(&rel);
            let ts = is_ts(&ext);
            // 音频、视频按库中记下的时长判定，图片按记下的像素上限判定，内容变了的不算（见模块说明）；
            // `.ts` / `.mts` 只有判定为视频的记有时长
            let too_long = ((matches!(kind, "audio" | "video") || ts)
                && old.as_ref().is_some_and(|o| {
                    o.size == size
                        && o.mtime == mtime
                        && o.duration_ms.is_some_and(|d| d as u64 > audio_limit_ms)
                }))
                || (kind == "image"
                    && old.as_ref().is_some_and(|o| {
                        o.too_large
                            && o.size == size
                            && o.mtime == mtime
                            && o.pixel_limit.is_some_and(|p| p as u64 >= pixel_limit)
                    }));
            let too_large = !cloud && (meta.len() > limit || too_long);
            stats.cloud += u64::from(cloud);
            stats.too_large += u64::from(too_large);
            // 大小上限或开启的类型改了时，未完成索引的本地文件重新判定；`.ts` / `.mts` 是否为视频由索引流水线判定
            // （见 KINDS），文件没变时按库中的种类判断，已判定不是视频的不再排队
            let unchanged = old.as_ref().is_some_and(|o| {
                let judged = o.kind == "other" && o.hashed && !o.has_job;
                o.size == size
                    && o.mtime == mtime
                    && o.cloud == cloud
                    && (cloud
                        || o.status == "stored"
                        || (o.too_large == too_large
                            && o.has_job == (!too_large && !judged && limits.wants(&o.kind, &ext))))
            });
            if !unchanged {
                let wants_job = !cloud && !too_large && limits.wants(kind, &ext);
                if old.is_some() {
                    stats.updated += 1;
                } else {
                    stats.added += 1;
                }
                let rec = Rec {
                    rel,
                    name,
                    ext,
                    kind,
                    size,
                    mtime,
                    cloud,
                    too_large,
                    wants_job,
                };
                if old.is_none() && !cloud && (scope.is_some() || move_sizes.contains(&size)) {
                    out.deferred.push(rec);
                } else {
                    batch.push((old, rec));
                }
            }
            if batch.len() >= BATCH {
                flush(db, root_id, &root, &mut batch, cancel)?;
            }
            seen += 1;
            if seen.is_multiple_of(BATCH) {
                progress(stats);
            }
        }
    }
    flush(db, root_id, &root, &mut batch, cancel)?;

    out.gone = known
        .into_iter()
        .filter(|(rel, _)| !keep.iter().any(|k| under(rel, k)))
        .map(|(_, old)| (old.id, old.segs && !old.cloud))
        .collect();
    Ok(out)
}

#[cfg(test)]
mod tests;
