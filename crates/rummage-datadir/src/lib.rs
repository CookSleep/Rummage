//! 数据目录的位置与更改。
//! 界面进程与核心进程共用这份逻辑，因此不依赖 Tauri。
//! 默认为安装目录下的 `data\`（开发版为 `src-tauri/dev-data/`），可在设置中更改。更改后的位置写在安装目录的
//! `data-dir` 指向文件中：一行路径，UTF-8；安装在可移动设备上且与安装目录同盘时省略盘符（以 `\` 开头），
//! U 盘换了盘符也能找到；其他情况写完整路径。
//! 更改流程：设置页用 `check` 校验新目录，确认后由 `request_change` 写入 `data-dir.pending` 并重启；
//! 下次启动时 `prepare` 在初始化日志、存储、窗口和核心进程之前完成迁移（见 `migrate`）。
//! WebView2 的数据目录只能在创建时指定，运行中 SQLite 也锁着文件，所以只在启动时迁移。
//! 指向的目录无法使用时 `prepare` 返回 `Missing` / `Invalid`，由调用方询问用户（界面进程用 `choose` 弹出对话框），
//! 再按用户的选择重试或带上 `Fallback` 重新调用；本 crate 不主动弹窗或退出进程
mod migrate;
#[cfg(not(windows))]
#[path = "other.rs"]
mod platform;
#[cfg(windows)]
#[path = "windows.rs"]
mod platform;

use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::{Component, Path, PathBuf, Prefix};

const POINTER: &str = "data-dir";
/// 已确认、待下次启动迁移：原目录、新目录、发起更改的进程号、迁移完成后要写入的指向文件内容
/// （新目录是默认位置时为空行，表示删除指向文件），各占一行
const PENDING: &str = "data-dir.pending";
/// 迁移开始前由 PENDING 改名而来，迁移结束后删除；启动时仍存在说明上次迁移中途程序意外退出
const RUNNING: &str = "data-dir.migrating";
/// 迁移期间独占打开，防止同时启动的第二个实例也去迁移
const LOCK: &str = "data-dir.lock";

const NETWORK: &str = "不支持网络位置。";
const ROOT: &str = "不能使用磁盘根目录。";
const PROTECTED: &str = "不能使用用户文件夹或 AppData。";
const KNOWN: &str = "不能直接使用下载、图片、文档等系统文件夹，可以在里面新建一个。";
const SYSTEM: &str = "不能使用 Windows、Program Files 等系统位置。";
const CLOUD: &str = "不能使用 OneDrive 等云同步文件夹。";
const IN_INSTALL: &str = "不能使用安装目录，卸载时可能被删除。";
const SAME: &str = "与当前数据目录相同。";
const NESTED: &str = "不能与当前数据目录互相包含。";
const NOT_EMPTY: &str = "请选择一个空文件夹。";
const NOT_DIR: &str = "请选择一个文件夹。";
const INDEXED: &str = "不能选在已被索引的文件夹之内。";

pub use migrate::dir_stats;
pub use platform::{can_choose, choose};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("无法确定程序所在目录：{0}")]
    Base(#[source] io::Error),
    /// 指向的目录不存在或无法访问，例如存储设备未连接
    #[error("找不到数据目录 \"{}\"，存储设备可能未连接。", shown(.0))]
    Missing(PathBuf),
    /// 指向文件损坏，或指向的位置不能用作数据目录；label、path 说明涉及的文件或目录
    #[error("{reason}（{label}：{}）", shown(path))]
    Invalid {
        reason: String,
        label: &'static str,
        path: PathBuf,
    },
    /// 所选位置不能作为新数据目录，内容为可直接展示给用户的原因
    #[error("{0}")]
    Rejected(String),
    #[error("无法开始更改数据目录，请确认 Rummage 的安装目录可以写入后重试。（{0}）")]
    Pending(#[source] io::Error),
    /// 同时启动的另一个实例正在迁移数据目录，本实例应直接退出
    #[error("另一个 Rummage 正在更改数据目录，请等它完成。")]
    Busy,
    /// 有待完成或中断的数据目录更改（`locate`），要由界面在启动时完成
    #[error("数据目录正在更改，请先打开 Rummage 完成更改。")]
    Changing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Success,
    Warning,
}

/// 启动时确定数据目录的结果，供前端加载后提示一次。
/// message 是一句话结论，paths 为 [标签, 路径]，details 为原因或补充说明，每项一段
#[derive(Debug, Clone)]
pub struct Notice {
    pub kind: NoticeKind,
    pub title: String,
    pub message: String,
    pub paths: Vec<(String, String)>,
    pub details: Vec<String>,
}

fn notice(
    kind: NoticeKind,
    title: &str,
    message: &str,
    paths: &[(&str, &Path)],
    details: Vec<String>,
) -> Notice {
    Notice {
        kind,
        title: title.into(),
        message: message.into(),
        paths: paths
            .iter()
            .map(|(label, path)| (label.to_string(), shown(path)))
            .collect(),
        details,
    }
}

/// 指向的目录无法使用（`Missing` / `Invalid`）时，调用方询问用户后选择的处理方式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// 删除指向文件，改用默认目录
    UseDefault,
    /// 无法询问用户时：本次使用默认目录，指向文件保持不动
    Skip,
}

pub struct Startup {
    /// 本次运行使用的数据目录；无法确定时为原因
    pub dir: Result<PathBuf, Error>,
    /// 迁移结果与上次未完成的迁移，没有时为空
    pub notices: Vec<Notice>,
}

/// 发布版为 exe 所在目录；debug 构建（含 tauri dev）为仓库的 `src-tauri/`
pub fn base_dir() -> io::Result<PathBuf> {
    if cfg!(debug_assertions) {
        // 本 crate 位于 <仓库>/crates/rummage-datadir/
        return Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .map(|repo| repo.join("src-tauri"))
            .ok_or_else(|| io::Error::other("无法确定仓库目录"));
    }
    let exe = std::env::current_exe()?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other("无法确定程序所在目录"))
}

fn default_dir(base: &Path) -> PathBuf {
    base.join(if cfg!(debug_assertions) {
        "dev-data"
    } else {
        "data"
    })
}

/// 默认数据目录，供“改用默认目录”的确认框显示
pub fn default_location() -> io::Result<PathBuf> {
    base_dir().map(|base| default_dir(&base))
}

/// 目录可能尚不存在：规范化最近的已存在上级，再接上其余部分
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            return rest.iter().rev().fold(real, |acc, name| acc.join(name));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// 规范化后忽略大小写比较完整路径
pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    let full = |path: &Path| normalize(path).to_string_lossy().to_lowercase();
    full(a) == full(b)
}

/// 两个已规范化的目录相同或一个位于另一个之中
fn is_nested(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// 提示中去掉规范化带来的 \\?\ 前缀，便于用户查找
pub(crate) fn shown(path: &Path) -> String {
    let raw = path.display().to_string();
    match raw.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!(r"\\{rest}"),
        None => raw.strip_prefix(r"\\?\").unwrap_or(&raw).to_string(),
    }
}

fn disk(path: &Path) -> Option<u8> {
    match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                Some(letter.to_ascii_uppercase())
            }
            _ => None,
        },
        _ => None,
    }
}

/// UNC 路径、映射的网络驱动器等网络位置：SQLite 在网络文件系统上容易损坏
fn is_network(path: &Path) -> bool {
    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return false;
    };
    match prefix.kind() {
        Prefix::UNC(..) | Prefix::VerbatimUNC(..) | Prefix::DeviceNS(_) => true,
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => platform::is_remote_drive(letter),
        Prefix::Verbatim(_) => false,
    }
}

/// 安装目录是否在可移动设备上
fn removable_base(base: &Path) -> bool {
    disk(base).is_some_and(platform::is_removable_drive)
}

/// 指向文件中记录的文本：新目录是默认位置时为 None（删除指向文件）。
/// 只有安装在可移动设备上、且新目录与安装目录同盘时才省略盘符，U 盘换了盘符也能找到；
/// 普通安装写完整路径，整个安装文件夹挪到别的盘后数据目录仍然有效
fn pointer_text(base: &Path, to: &Path, removable: bool) -> Option<String> {
    if normalize(to) == normalize(&default_dir(base)) {
        return None;
    }
    let text = to.display().to_string();
    match (disk(base), disk(to)) {
        (Some(a), Some(b)) if removable && a == b && text.as_bytes().get(1) == Some(&b':') => {
            Some(text[2..].to_string())
        }
        _ => Some(text),
    }
}

/// 省略了盘符的路径补上安装目录的盘符
fn parse_pointer(base: &Path, line: &str) -> PathBuf {
    if line.starts_with('\\')
        && !line.starts_with("\\\\")
        && let Some(Component::Prefix(prefix)) = base.components().next()
    {
        let mut path = prefix.as_os_str().to_owned();
        path.push(line);
        return PathBuf::from(path);
    }
    PathBuf::from(line)
}

/// 指向文件的第一行；不存在时为 None
fn read_pointer(base: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(base.join(POINTER)) {
        Ok(text) => Ok(Some(
            text.trim_start_matches('\u{feff}')
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string(),
        )),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// 写入并落盘后再改名，避免断电留下损坏或空的文件
fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let mut file = fs::File::create(&tmp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)
}

/// 写入指向文件；text 为 None 时删除（新目录是默认位置）
fn write_pointer(base: &Path, text: Option<&str>) -> io::Result<()> {
    let pointer = base.join(POINTER);
    match text {
        Some(text) => write_atomic(&pointer, text),
        None => match fs::remove_file(&pointer) {
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            other => other,
        },
    }
}

/// 已规范化的系统目录：protected（用户主目录、AppData 下的 Roaming 和 Local）及其上级、
/// known（下载、图片等已知文件夹）本身及其上级不能使用；system（Windows、Program Files、ProgramData、
/// 临时文件夹）和 cloud（OneDrive）及其中的位置都不能使用
#[derive(Default)]
struct SystemDirs {
    protected: Vec<PathBuf>,
    known: Vec<PathBuf>,
    system: Vec<PathBuf>,
    cloud: Vec<PathBuf>,
}

fn env_paths(names: &[&str]) -> Vec<PathBuf> {
    names
        .iter()
        .filter_map(std::env::var_os)
        .filter(|value| !value.is_empty())
        .map(|value| normalize(Path::new(&value)))
        .collect()
}

fn system_dirs() -> SystemDirs {
    let mut system = env_paths(&[
        "SystemRoot",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "ProgramData",
    ]);
    system.push(normalize(&std::env::temp_dir()));
    SystemDirs {
        protected: env_paths(&["USERPROFILE", "APPDATA", "LOCALAPPDATA"]),
        known: platform::known_dirs()
            .iter()
            .map(|dir| normalize(dir))
            .collect(),
        system,
        cloud: env_paths(&["OneDrive", "OneDriveConsumer", "OneDriveCommercial"]),
    }
}

/// 位置本身不能作为数据目录的原因，参数均已规范化；不检查是否为空，也不涉及当前数据目录
fn location_block(dir: &Path, base: &Path, dirs: &SystemDirs) -> Option<&'static str> {
    if is_network(dir) {
        return Some(NETWORK);
    }
    if dir.parent().is_none() {
        return Some(ROOT);
    }
    if dirs.protected.iter().any(|d| d.starts_with(dir)) {
        return Some(PROTECTED);
    }
    if dirs.known.iter().any(|d| d.starts_with(dir)) {
        return Some(KNOWN);
    }
    if dirs.system.iter().any(|d| is_nested(d, dir)) {
        return Some(SYSTEM);
    }
    if dirs.cloud.iter().any(|d| is_nested(d, dir)) {
        return Some(CLOUD);
    }
    if is_nested(dir, base) && dir != default_dir(base) {
        return Some(IN_INSTALL);
    }
    None
}

/// 不能作为新数据目录的原因，参数均已规范化
fn target_block(to: &Path, current: &Path, base: &Path, dirs: &SystemDirs) -> Option<&'static str> {
    if to == current {
        return Some(SAME);
    }
    // 当前数据目录可能在安装目录下（默认位置），这时优先说明包含关系
    match location_block(to, base, dirs) {
        Some(IN_INSTALL) | None if is_nested(to, current) => Some(NESTED),
        other => other,
    }
}

/// 新数据目录必须是绝对路径、为空或不存在，不在被索引的文件夹之内，且不违反 target_block 中的规则
fn check_target(
    to: &Path,
    current: &Path,
    base: &Path,
    dirs: &SystemDirs,
    indexed: &[PathBuf],
) -> Result<(), String> {
    let real = normalize(to);
    if !to.is_absolute() || real.components().any(|c| c == Component::ParentDir) {
        return Err(NOT_DIR.into());
    }
    if is_network(to) {
        return Err(NETWORK.into());
    }
    let base = normalize(base);
    if let Some(reason) = target_block(&real, &normalize(current), &base, dirs) {
        return Err(reason.into());
    }
    if indexed.iter().any(|dir| real.starts_with(normalize(dir))) {
        return Err(INDEXED.into());
    }
    if to.exists() && !to.is_dir() {
        return Err(NOT_DIR.into());
    }
    match fs::read_dir(to).map(|mut entries| entries.next().is_none()) {
        Ok(true) => Ok(()),
        Ok(false) => Err(NOT_EMPTY.into()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("无法读取所选文件夹：{err}")),
    }
}

/// 读取 data-dir 确定数据目录；不存在时为默认位置
fn resolve(base: &Path, dirs: &SystemDirs) -> Result<PathBuf, Error> {
    let pointer = base.join(POINTER);
    let invalid = |reason: String, label, path: &Path| Error::Invalid {
        reason,
        label,
        path: path.to_path_buf(),
    };
    let line = match read_pointer(base) {
        Ok(None) => return Ok(default_dir(base)),
        Ok(Some(line)) => line,
        Err(err) => {
            return Err(invalid(
                format!("无法读取数据目录设置：{err}"),
                "设置文件",
                &pointer,
            ));
        }
    };
    let dir = parse_pointer(base, &line);
    if !dir.is_absolute() {
        return Err(invalid(
            "数据目录设置中记录的不是有效的文件夹路径。".into(),
            "设置文件",
            &pointer,
        ));
    }
    if is_network(&dir) {
        return Err(invalid(
            "数据目录位于网络位置，不支持。".into(),
            "数据目录",
            &dir,
        ));
    }
    if !dir.is_dir() {
        return Err(Error::Missing(dir));
    }
    if let Some(reason) = location_block(&normalize(&dir), &normalize(base), dirs) {
        return Err(invalid(
            format!("数据目录位于不支持的位置。{reason}"),
            "数据目录",
            &dir,
        ));
    }
    Ok(dir)
}

/// 确定数据目录，有待迁移的更改时先完成迁移。指向的目录无法使用时：没有 fallback 则返回错误，
/// 不做任何改动；有则按其改用默认目录
fn startup(
    base: &Path,
    dirs: &SystemDirs,
    fallback: Option<Fallback>,
) -> (Result<PathBuf, Error>, Vec<Notice>) {
    // 改用默认目录会删除指向文件，先记下原始内容供判断上次中断的迁移
    let pointer = read_pointer(base);
    let default = default_dir(base);
    let mut notices = Vec::new();
    // 原来指向的目录，改用默认目录后写进提示，方便用户找回
    let recorded = match &pointer {
        Ok(Some(line)) => parse_pointer(base, line),
        _ => PathBuf::new(),
    };
    let rows: Vec<(&str, &Path)> = [
        ("原数据目录", recorded.as_path()),
        ("默认目录", default.as_path()),
    ]
    .into_iter()
    .filter(|(_, path)| !path.as_os_str().is_empty())
    .collect();
    let mut dir = match (resolve(base, dirs), fallback) {
        (Ok(dir), None | Some(Fallback::Skip)) => dir,
        (Err(err), None) => return (Err(err), Vec::new()),
        // 用户已确认改用默认目录：其间设备即使重新连接也按用户的选择处理
        (_, Some(Fallback::UseDefault)) => {
            let _ = fs::remove_file(base.join(POINTER));
            notices.push(notice(
                NoticeKind::Warning,
                "已改用默认数据目录",
                "原目录中的索引和模型不会转移，需要重新设置文件夹并下载模型。",
                &rows,
                vec![],
            ));
            default.clone()
        }
        (Err(err), Some(Fallback::Skip)) => {
            notices.push(notice(
                NoticeKind::Warning,
                "本次使用默认数据目录",
                match err {
                    Error::Missing(_) => {
                        "找不到原数据目录，本次使用默认目录。\n\n重新连接存储设备后，重新启动 Rummage 即可恢复。"
                    }
                    _ => "原数据目录设置无效，本次使用默认目录。",
                },
                &rows,
                vec![],
            ));
            default.clone()
        }
    };
    let (pending, running) = (base.join(PENDING), base.join(RUNNING));
    if !pending.exists() && !running.exists() {
        return (Ok(dir), notices);
    }
    match platform::lock(&base.join(LOCK)) {
        // 同时启动的另一个实例正在迁移并显示进度，由调用方退出本实例
        Ok(None) => return (Err(Error::Busy), notices),
        Err(err) => {
            let _ = fs::remove_file(&pending);
            notices.push(notice(
                NoticeKind::Warning,
                "更改数据目录失败",
                "数据仍在原目录。",
                &[("原目录", &dir)],
                vec![format!("原因：无法写入 Rummage 的安装目录（{err}）")],
            ));
        }
        Ok(Some(_lock)) => {
            if let Ok(text) = fs::read_to_string(&running) {
                notices.push(migrate::interrupted(base, &dir, &pointer, &text));
                let _ = fs::remove_file(&running);
            }
            if pending.exists() {
                let (next, result) = migrate::run_pending(base, &dir);
                dir = next;
                notices.push(result);
            }
        }
    }
    (Ok(dir), notices)
}

/// 命令行用：只确定数据目录，不迁移、不改动任何文件；有待完成或中断的更改时返回 `Changing`，
/// 指向的目录无法使用时返回 `Missing` / `Invalid`，都不回退到默认目录
pub fn locate() -> Result<PathBuf, Error> {
    let base = base_dir().map_err(Error::Base)?;
    locate_in(&base, &system_dirs())
}

/// 只读出当前指向的数据目录：不检查待完成的更改、不迁移，也不回退到默认目录（`rummage-core --shutdown` 用，
/// 迁移前也要能请正在运行的核心退出）
pub fn current() -> Result<PathBuf, Error> {
    let base = base_dir().map_err(Error::Base)?;
    resolve(&base, &system_dirs())
}

fn locate_in(base: &Path, dirs: &SystemDirs) -> Result<PathBuf, Error> {
    if base.join(PENDING).exists() || base.join(RUNNING).exists() {
        return Err(Error::Changing);
    }
    resolve(base, dirs)
}

/// 启动时最先调用：确定数据目录，有待迁移的更改时先完成迁移（超过半秒未完成时显示进度框）。
/// 先不带 fallback 调用；返回 `Missing` / `Invalid` 时询问用户，再重试或带上用户选择的 fallback 调用
pub fn prepare(fallback: Option<Fallback>) -> Startup {
    let (dir, notices) = match base_dir() {
        Ok(base) => startup(&base, &system_dirs(), fallback),
        Err(err) => (Err(Error::Base(err)), Vec::new()),
    };
    Startup { dir, notices }
}

/// 校验新数据目录，不能使用时返回可直接展示给用户的原因。current 为当前数据目录，indexed 为已有的被索引文件夹
pub fn check(to: &Path, current: &Path, indexed: &[PathBuf]) -> Result<(), Error> {
    let base = base_dir().map_err(Error::Base)?;
    check_target(to, current, &base, &system_dirs(), indexed).map_err(Error::Rejected)
}

/// 待迁移标记的内容，第 4 行为确认时算好的指向文件内容，保证迁移后写入的与确认时一致
fn pending_text(base: &Path, from: &Path, to: &Path) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        from.display(),
        to.display(),
        std::process::id(),
        pointer_text(base, to, removable_base(base)).unwrap_or_default()
    )
}

/// 校验后记下待迁移的新目录，由下次启动完成迁移；调用方随后重启应用
pub fn request_change(to: &Path, current: &Path, indexed: &[PathBuf]) -> Result<(), Error> {
    check(to, current, indexed)?;
    let base = base_dir().map_err(Error::Base)?;
    write_atomic(&base.join(PENDING), &pending_text(&base, current, to)).map_err(Error::Pending)
}

#[cfg(test)]
pub(crate) mod tests;
