//! 索引数据库 `<数据目录>\index.db`。
//! - 结构版本记在 `PRAGMA user_version`，`MIGRATIONS` 第 N 项把结构从 N 升到 N+1，每项一个事务。
//! - 打开顺序：在 `<数据目录>\tmp\` 实际写一个文件探测可写，并清掉上次崩溃留下的临时文件 → 打开连接，
//!   设置忙等待、WAL、`recursive_triggers` → 只读时按不可写处理 → 版本高于程序支持的版本时拒绝 → 迁移 → 开启外键
//!   → 建记录缩略图引用的临时触发器（`thumbs`）。
//! - 迁移：不是新库时先备份为 `index.db.v<旧版本>.bak`（先合并 WAL，复制到 `tmp\` 再改名）。迁移期间外键关闭，
//!   每项提交前用 `foreign_key_check` 检查；全部成功后删除备份，中途失败时用备份覆盖回去，数据库回到迁移前的状态。
//! - 迁移前已有同名备份（上次迁移中途退出或恢复失败）：当前库通过 `integrity_check` 就覆盖旧备份照常迁移；
//!   不通过或打不开时用备份替换后再迁移一次，成功后在 `meta` 写入 `notice.db_restored` 供设置页提示，
//!   仍失败时保留备份：磁盘满、无法备份、不可写这类用户能自行处理的错误原样返回，其余返回 `RestoreFailed`。
//! - `recursive_triggers` 开启后，`REPLACE` 冲突删除旧行时也会执行删除触发器和外键级联：`files` 的写入用
//!   `INSERT … ON CONFLICT DO UPDATE`，保留原行 id；用 `INSERT OR REPLACE` 会级联删掉该文件的片段和向量。
//! - 错误的 `Display` 是给用户看的中文整句，原始错误在构造时写日志。
//! - 写入只用这一个连接，由调用方放在专用线程或互斥锁后使用（`Db` 不是 `Sync`）。
//! - 时间一律为 Unix 毫秒。
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use half::f16;
use rusqlite::{Connection, ErrorCode, MAIN_DB, OptionalExtension, params};

use crate::thumbs;

const DB_FILE: &str = "index.db";
/// 自动从迁移备份恢复过时写入的 `meta` 键，值为恢复时间；设置页据此提示“上次更新索引数据库时出错，已从备份恢复。”
pub const NOTICE_DB_RESTORED: &str = "notice.db_restored";

/// 按版本顺序执行的迁移，第 N 项把结构从 N 升到 N+1。
/// **已发布的项不能修改，只能追加**；新增表或字段时同时考虑旧数据的升级路径。
const MIGRATIONS: &[&str] = &["
-- 只由 Rust 读写的内部状态
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
-- 用户调整过的参数，值为 JSON；未出现的参数用参数表的默认值
CREATE TABLE params (key TEXT PRIMARY KEY, value TEXT NOT NULL);
-- 向量空间：id 由其余各列算出；纯文本空间不经过 mmproj，mmproj_sha256 为空
CREATE TABLE spaces (
  id TEXT PRIMARY KEY,
  family TEXT NOT NULL,
  model_sha256 TEXT NOT NULL,
  mmproj_sha256 TEXT,
  runtime TEXT NOT NULL,
  backend TEXT NOT NULL,
  precision TEXT NOT NULL,
  prefix_scheme TEXT NOT NULL,
  preprocess TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
-- 索引的根目录；offline 为所在设备未连接，离线时保留索引
CREATE TABLE roots (
  id INTEGER PRIMARY KEY,
  path TEXT NOT NULL UNIQUE,
  settings_json TEXT NOT NULL,
  settings_version INTEGER NOT NULL,
  offline INTEGER NOT NULL DEFAULT 0,
  added_at INTEGER NOT NULL
);
-- cloud：网盘占位文件，只记元数据；volume_file_id：卷序列号 + NTFS 文件 ID，用于识别改名和移动
CREATE TABLE files (
  id INTEGER PRIMARY KEY,
  root_id INTEGER NOT NULL REFERENCES roots(id) ON DELETE CASCADE,
  rel_path TEXT NOT NULL,
  name TEXT NOT NULL,
  ext TEXT NOT NULL,
  kind TEXT NOT NULL,
  size INTEGER NOT NULL,
  mtime INTEGER NOT NULL,
  quick_hash BLOB,
  content_hash BLOB,
  volume_file_id TEXT,
  meta_json TEXT NOT NULL DEFAULT '{}',
  cloud INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  -- 图片的宽高（像素，已按 EXIF 方向转正），解码时记录；瀑布流据此直接算出卡片位置，没有宽高时按正方形排版
  width INTEGER,
  height INTEGER,
  UNIQUE (root_id, rel_path)
);
CREATE INDEX files_quick_hash ON files (quick_hash);
CREATE INDEX files_content_hash ON files (content_hash);
CREATE INDEX files_volume_file_id ON files (volume_file_id);
-- 首次建索引按修改时间从新到旧处理
CREATE INDEX files_mtime ON files (mtime);
-- locator_json：页码、章节、时间区间、长图区域等
CREATE TABLE segments (
  id INTEGER PRIMARY KEY,
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  modality TEXT NOT NULL,
  locator_json TEXT NOT NULL,
  text TEXT,
  created_at INTEGER NOT NULL
);
-- 删除文件时按 file_id 级联删除片段，没有索引会逐个文件全表扫描
CREATE INDEX segments_file ON segments (file_id);
-- vec：dim 维 f16 小端；coarse：粗排用的 256 维 int8 加 f32 小端缩放系数（`vindex::quantize`）
CREATE TABLE vectors (
  segment_id INTEGER NOT NULL REFERENCES segments(id) ON DELETE CASCADE,
  space_id TEXT NOT NULL REFERENCES spaces(id),
  dim INTEGER NOT NULL,
  vec BLOB NOT NULL,
  params_version TEXT NOT NULL,
  coarse BLOB NOT NULL,
  PRIMARY KEY (segment_id, space_id)
);
-- 按空间载入向量、清理旧空间
CREATE INDEX vectors_space ON vectors (space_id);
-- stage：pending / preprocessing / embedding / stored / failed
-- parts：文档要算的部分，1 为正文、2 为内嵌媒体，按位或；其他种类恒为 3
-- media_failed：已完成的文档中无法识别的内嵌媒体个数；大于 0 时文档出现在“失败文件”列表中，正文照常可搜
CREATE TABLE jobs (
  file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
  stage TEXT NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  priority INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  parts INTEGER NOT NULL DEFAULT 3,
  media_failed INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX jobs_stage ON jobs (stage, priority);
-- 按 quick_hash 连续失败 3 次的文件
CREATE TABLE blacklist (
  quick_hash BLOB PRIMARY KEY,
  reason TEXT NOT NULL,
  failures INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
-- 文件名搜索，内容取自 files，由下面的触发器同步
CREATE VIRTUAL TABLE fts_names USING fts5(
  name, rel_path, content = 'files', content_rowid = 'id', tokenize = 'trigram'
);
CREATE TRIGGER files_fts_insert AFTER INSERT ON files BEGIN
  INSERT INTO fts_names (rowid, name, rel_path) VALUES (new.id, new.name, new.rel_path);
END;
CREATE TRIGGER files_fts_delete AFTER DELETE ON files BEGIN
  INSERT INTO fts_names (fts_names, rowid, name, rel_path) VALUES ('delete', old.id, old.name, old.rel_path);
END;
CREATE TRIGGER files_fts_update AFTER UPDATE OF name, rel_path ON files BEGIN
  INSERT INTO fts_names (fts_names, rowid, name, rel_path) VALUES ('delete', old.id, old.name, old.rel_path);
  INSERT INTO fts_names (rowid, name, rel_path) VALUES (new.id, new.name, new.rel_path);
END;
-- 最近的文字搜索，只存本地；used_at 为最近一次搜索的时间，同一查询只留一行
CREATE TABLE search_history (query TEXT PRIMARY KEY, used_at INTEGER NOT NULL);
-- 正文关键词搜索，内容取自 segments.text，由下面的触发器同步。
-- 没有正文的片段（图片）也照常写入，删除时才不会和索引对不上
CREATE VIRTUAL TABLE fts_text USING fts5(
  text, content = 'segments', content_rowid = 'id', tokenize = 'trigram'
);
CREATE TRIGGER segments_fts_insert AFTER INSERT ON segments BEGIN
  INSERT INTO fts_text (rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER segments_fts_delete AFTER DELETE ON segments BEGIN
  INSERT INTO fts_text (fts_text, rowid, text) VALUES ('delete', old.id, old.text);
END;
CREATE TRIGGER segments_fts_update AFTER UPDATE OF text ON segments BEGIN
  INSERT INTO fts_text (fts_text, rowid, text) VALUES ('delete', old.id, old.text);
  INSERT INTO fts_text (rowid, text) VALUES (new.id, new.text);
END;
-- 分数校准的背景分布（`calib`）：kind 为查询类型（text / similar）；source 为算出时查询的来源
-- （文字查询为文本空间加查询表指纹，similar 为目标空间本身），vectors 为当时该“空间 × 模态”的向量数，据此判断何时重算
CREATE TABLE calib (
  kind TEXT NOT NULL,
  space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
  modality TEXT NOT NULL,
  source TEXT NOT NULL,
  mean REAL NOT NULL,
  std REAL NOT NULL,
  vectors INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (kind, space_id, modality)
);
-- 通用查询表在当前文本空间中的查询向量（768 维 f16），文本空间不变就不再算；只保留一个空间的
CREATE TABLE calib_queries (
  space_id TEXT NOT NULL,
  query TEXT NOT NULL,
  vec BLOB NOT NULL,
  PRIMARY KEY (space_id, query)
);
"];

/// 消息都是能直接展示给用户的中文整句；原始错误在构造时写日志
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("数据目录所在的磁盘（{0}）未连接，请连接后重新启动 Rummage。")]
    DriveMissing(String),
    #[error("数据目录（{0}）不可写，请检查目录权限和磁盘空间后重新启动 Rummage。")]
    Unwritable(String),
    #[error("索引数据库来自更新版本的 Rummage，请升级应用后再打开。")]
    TooNew,
    #[error("升级索引数据库前无法备份，请检查磁盘空间后重新启动 Rummage。")]
    Backup,
    #[error(
        "索引数据库从版本 {0} 升级失败，已恢复到升级前的状态。\n\n请更新到最新版本的 Rummage 后再打开，或查看日志了解原因。"
    )]
    Migrate(usize),
    #[error(
        "索引数据库升级失败，且无法从备份恢复，备份文件保存在 \"{0}\"。\n\n请关闭 Rummage，用该文件替换 index.db 后重新启动。"
    )]
    Restore(String),
    #[error(
        "更新索引数据库时出错，自动恢复也没有成功。\n\n可以清除全部索引后重新建立（不会删除你的文件），或关闭 Rummage，用数据目录中的 \"{}\" 替换 index.db 后再打开。",
        .backup.file_name().unwrap_or_default().to_string_lossy()
    )]
    RestoreFailed { backup: PathBuf },
    #[error("数据目录所在的磁盘空间不足，请清理后重试。")]
    DiskFull,
    #[error(
        "索引数据库已损坏，请在“索引数据库无法打开”窗口中选择“清除并重建”（不会删除你的文件）。"
    )]
    Corrupt,
    #[error("索引数据库被其他程序占用，请关闭后重试。")]
    Busy,
    #[error(
        "索引数据库无法打开。请重新启动 Rummage；仍然失败时，请附上数据目录中 logs 文件夹里的日志反馈问题。"
    )]
    Sqlite,
}

/// 向量空间的一行，各字段的含义见 `rummage_embed::SpaceConfig`
#[derive(Debug, Clone, PartialEq)]
pub struct Space {
    pub id: String,
    pub family: String,
    pub model_sha256: String,
    pub mmproj_sha256: Option<String>,
    pub runtime: String,
    pub backend: String,
    pub precision: String,
    pub prefix_scheme: String,
    pub preprocess: String,
}

pub struct Db {
    pub(crate) conn: Connection,
    /// 数据目录，用于错误消息和扫描时跳过数据目录
    pub(crate) dir: PathBuf,
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 目录不可写时区分是不是整个磁盘不在（移动硬盘未连接、盘符变了）
fn unwritable(dir: &Path, err: io::Error) -> DbError {
    log::error!("数据目录 {} 不可写：{err}", dir.display());
    let root: PathBuf = dir
        .components()
        .take_while(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
        .collect();
    if !root.as_os_str().is_empty() && !root.exists() {
        return DbError::DriveMissing(root.display().to_string());
    }
    DbError::Unwritable(dir.display().to_string())
}

/// 按错误码归为用户能自行处理的几类
fn sqlite(dir: &Path, err: rusqlite::Error) -> DbError {
    log::error!("索引数据库出错：{err}");
    match err.sqlite_error_code() {
        Some(ErrorCode::DiskFull) => DbError::DiskFull,
        Some(ErrorCode::ReadOnly) => DbError::Unwritable(dir.display().to_string()),
        Some(ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt) => DbError::Corrupt,
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => DbError::Busy,
        _ => DbError::Sqlite,
    }
}

/// 执行一项迁移；外键此时关闭，提交前检查没有违例，有违例时不提交（事务丢弃时回滚）
fn apply(conn: &mut Connection, version: usize, sql: &str) -> Result<(), String> {
    let tx = conn.transaction().map_err(|err| err.to_string())?;
    tx.execute_batch(sql).map_err(|err| err.to_string())?;
    let violated = tx
        .prepare("PRAGMA foreign_key_check")
        .and_then(|mut stmt| stmt.query([])?.next().map(|row| row.is_some()))
        .map_err(|err| err.to_string())?;
    if violated {
        return Err("迁移后存在违反外键约束的行".into());
    }
    tx.pragma_update(None, "user_version", (version + 1) as i64)
        .and_then(|()| tx.commit())
        .map_err(|err| err.to_string())
}

/// 合并 WAL 后复制主文件；先写到 `tmp\` 再改名，目标位置不会出现不完整的备份
fn backup(conn: &Connection, dir: &Path, path: &Path, target: &Path) -> Result<(), String> {
    // 返回（是否被挡住, WAL 帧数, 已合并帧数）：没被挡住且全部合并，主文件才是完整的数据库
    let (busy, log, done): (i64, i64, i64) = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(|err| err.to_string())?;
    if busy != 0 || log != done {
        return Err(format!("WAL 未能完全合并（{busy}, {log}, {done}）"));
    }
    let tmp = dir.join("tmp").join(DB_FILE);
    fs::copy(path, &tmp).map_err(|err| err.to_string())?;
    fs::rename(&tmp, target).map_err(|err| err.to_string())
}

/// 用备份覆盖数据库，备份文件保留，由调用方决定是否删除。连接已关闭；残留的 WAL 属于被替换的数据库，
/// 必须一起删掉，否则下次打开会把它重放到旧文件上
fn restore(path: &Path, backup: &Path) -> io::Result<()> {
    for suffix in ["-wal", "-shm"] {
        let mut side = path.as_os_str().to_owned();
        side.push(suffix);
        match fs::remove_file(PathBuf::from(side)) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
    }
    fs::copy(backup, path).map(|_| ())
}

fn backup_path(data_dir: &Path, version: usize) -> PathBuf {
    data_dir.join(format!("{DB_FILE}.v{version}.bak"))
}

/// 打开连接并读出结构版本；文件不是数据库或已损坏时返回 `Corrupt`
fn connect(data_dir: &Path, path: &Path) -> Result<(Connection, usize), DbError> {
    let sql = |err| sqlite(data_dir, err);
    let conn = Connection::open(path).map_err(sql)?;
    // 忙等待先于其他 PRAGMA 设置，切换 WAL 时遇到锁也会等待；外键在迁移完成后才开启
    conn.busy_timeout(Duration::from_secs(5)).map_err(sql)?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA recursive_triggers = ON;",
    )
    .map_err(sql)?;
    if conn.is_readonly(MAIN_DB).map_err(sql)? {
        log::error!("索引数据库 {} 只能以只读方式打开", path.display());
        return Err(DbError::Unwritable(data_dir.display().to_string()));
    }
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(sql)?;
    // user_version 为负说明不是本程序写的，按“更新的版本”处理，不去碰它
    let version = usize::try_from(version).map_err(|_| DbError::TooNew)?;
    Ok((conn, version))
}

/// `PRAGMA integrity_check` 只返回一行 `ok` 时库才完好；检查本身出错也按损坏处理
fn intact(conn: &Connection) -> bool {
    match conn.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)) {
        Ok(res) => res == "ok",
        Err(err) => {
            log::warn!("索引数据库完整性检查出错：{err}");
            false
        }
    }
}

/// 一次打开与迁移的结果
enum Opened {
    Ready(Connection),
    /// 当前库损坏，且有上次迁移留下的备份可以恢复
    Damaged(PathBuf),
}

impl Db {
    pub fn open(data_dir: &Path) -> Result<Self, DbError> {
        Self::open_with(data_dir, MIGRATIONS)
    }

    fn open_with(data_dir: &Path, migrations: &[&str]) -> Result<Self, DbError> {
        // 目录已存在时 create_dir_all 不会报错，用实际写文件探测权限
        let tmp = data_dir.join("tmp");
        fs::create_dir_all(&tmp).map_err(|err| unwritable(data_dir, err))?;
        fs::write(tmp.join(".write-test"), b"ok").map_err(|err| unwritable(data_dir, err))?;
        // 此时还没有写入在进行，tmp 中的都是上次崩溃留下的
        for entry in fs::read_dir(&tmp)
            .map_err(|err| unwritable(data_dir, err))?
            .flatten()
        {
            let _ = fs::remove_file(entry.path());
        }

        let path = data_dir.join(DB_FILE);
        let bak = match Self::attempt(data_dir, &path, migrations, false)? {
            Opened::Ready(conn) => {
                let db = Self {
                    conn,
                    dir: data_dir.to_path_buf(),
                };
                db.conn
                    .execute_batch(thumbs::TRACK)
                    .map_err(|err| db.err(err))?;
                return Ok(db);
            }
            Opened::Damaged(bak) => bak,
        };

        // 当前库损坏：用上次迁移前的备份替换后再迁移一次；仍失败时保留备份，让用户选择重建或手动替换
        log::warn!(
            "索引数据库已损坏，用迁移备份 {} 替换后重新升级",
            bak.display()
        );
        if let Err(err) = restore(&path, &bak) {
            log::error!("无法用备份 {} 替换索引数据库：{err}", bak.display());
            return Err(DbError::RestoreFailed { backup: bak });
        }
        let conn = match Self::attempt(data_dir, &path, migrations, true) {
            Ok(Opened::Ready(conn)) => conn,
            Err(err @ (DbError::DiskFull | DbError::Backup | DbError::Unwritable(_))) => {
                log::error!("从备份 {} 恢复后升级索引数据库失败：{err}", bak.display());
                return Err(err);
            }
            _ => {
                log::error!("从备份 {} 恢复后升级索引数据库仍然失败", bak.display());
                return Err(DbError::RestoreFailed { backup: bak });
            }
        };
        let db = Self {
            conn,
            dir: data_dir.to_path_buf(),
        };
        db.conn
            .execute_batch(thumbs::TRACK)
            .map_err(|err| db.err(err))?;
        db.meta_set(NOTICE_DB_RESTORED, &now_ms().to_string())?;
        Ok(db)
    }

    /// 打开并迁移一次。`retry` 为 true 表示刚从备份恢复：不再检查已有备份，失败时保留备份
    fn attempt(
        data_dir: &Path,
        path: &Path,
        migrations: &[&str],
        retry: bool,
    ) -> Result<Opened, DbError> {
        let (mut conn, version) = match connect(data_dir, path) {
            Ok(opened) => opened,
            // 打不开时找不到当前版本，按版本从新到旧找上次迁移留下的备份
            Err(DbError::Corrupt) if !retry => {
                return (1..migrations.len())
                    .rev()
                    .map(|v| backup_path(data_dir, v))
                    .find(|p| p.exists())
                    .map(Opened::Damaged)
                    .ok_or(DbError::Corrupt);
            }
            Err(err) => return Err(err),
        };
        if version > migrations.len() {
            return Err(DbError::TooNew);
        }

        let bak = if version > 0 && version < migrations.len() {
            let target = backup_path(data_dir, version);
            if !retry && target.exists() {
                if !intact(&conn) {
                    return Ok(Opened::Damaged(target));
                }
                // 上次只是中途退出、当前库完好，下面的备份直接覆盖旧备份
                log::warn!(
                    "发现上次升级留下的备份 {}，当前索引数据库完好，覆盖该备份后照常升级",
                    target.display()
                );
            }
            if let Err(err) = backup(&conn, data_dir, path, &target) {
                log::error!("无法备份索引数据库：{err}");
                return Err(DbError::Backup);
            }
            Some(target)
        } else {
            None
        };
        for (idx, sql) in migrations.iter().enumerate().skip(version) {
            let Err(err) = apply(&mut conn, idx, sql) else {
                continue;
            };
            log::error!("索引数据库从版本 {idx} 升级失败：{err}");
            drop(conn);
            let Some(bak) = bak else {
                return Err(DbError::Migrate(idx));
            };
            if let Err(err) = restore(path, &bak) {
                log::error!("无法从备份 {} 恢复索引数据库：{err}", bak.display());
                return Err(DbError::Restore(bak.display().to_string()));
            }
            if !retry && let Err(err) = fs::remove_file(&bak) {
                log::warn!("无法删除索引数据库的迁移备份 {}：{err}", bak.display());
            }
            return Err(DbError::Migrate(idx));
        }
        if let Some(bak) = bak
            && let Err(err) = fs::remove_file(&bak)
        {
            log::warn!("无法删除索引数据库的迁移备份 {}：{err}", bak.display());
        }
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(|err| sqlite(data_dir, err))?;
        Ok(Opened::Ready(conn))
    }

    pub(crate) fn err(&self, err: rusqlite::Error) -> DbError {
        sqlite(&self.dir, err)
    }

    /// 全部参数；值不是合法 JSON 的行跳过并记录警告
    pub fn params(&self) -> Result<BTreeMap<String, serde_json::Value>, DbError> {
        let rows: Vec<(String, String)> = self
            .conn
            .prepare("SELECT key, value FROM params")
            .and_then(|mut stmt| {
                stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect()
            })
            .map_err(|err| self.err(err))?;
        let mut out = BTreeMap::new();
        for (key, raw) in rows {
            match serde_json::from_str(&raw) {
                Ok(value) => {
                    out.insert(key, value);
                }
                Err(err) => log::warn!("参数 {key} 的值不是合法 JSON，已忽略：{err}"),
            }
        }
        Ok(out)
    }

    pub fn set_param(&self, key: &str, value: &serde_json::Value) -> Result<(), DbError> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO params (key, value) VALUES (?1, ?2)",
                params![key, value.to_string()],
            )
            .map_err(|err| self.err(err))?;
        Ok(())
    }

    pub fn meta_get(&self, key: &str) -> Result<Option<String>, DbError> {
        self.conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|err| self.err(err))
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<(), DbError> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .map_err(|err| self.err(err))?;
        Ok(())
    }

    /// 键不存在时什么也不做
    pub fn meta_delete(&self, key: &str) -> Result<(), DbError> {
        self.conn
            .execute("DELETE FROM meta WHERE key = ?1", [key])
            .map_err(|err| self.err(err))?;
        Ok(())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 插入向量空间；同一 id 已存在时不改动，返回 false
    pub fn insert_space(&self, space: &Space) -> Result<bool, DbError> {
        let n = insert_space(&self.conn, space).map_err(|err| self.err(err))?;
        Ok(n == 1)
    }
}

/// 同 `Db::insert_space`，可在事务中使用；返回插入的行数
pub(crate) fn insert_space(conn: &Connection, space: &Space) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT OR IGNORE INTO spaces (id, family, model_sha256, mmproj_sha256, runtime, backend,
           precision, prefix_scheme, preprocess, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            space.id,
            space.family,
            space.model_sha256,
            space.mmproj_sha256,
            space.runtime,
            space.backend,
            space.precision,
            space.prefix_scheme,
            space.preprocess,
            now_ms(),
        ],
    )
}

/// f32 向量转为 f16 小端字节，舍入到最近偶数
pub fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter()
        .flat_map(|&x| f16::from_f32(x).to_le_bytes())
        .collect()
}

/// 长度不是偶数时返回 `None`
pub fn blob_to_vec(blob: &[u8]) -> Option<Vec<f32>> {
    let (chunks, rest) = blob.as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    Some(
        chunks
            .iter()
            .map(|&b| f16::from_le_bytes(b).to_f32())
            .collect(),
    )
}

#[cfg(test)]
mod tests;
