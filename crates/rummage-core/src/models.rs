//! 模型文件的下载、导入与校验。
//! - 下载源：Hugging Face（默认）、镜像 hf-mirror.com 或用户填写的兼容地址；地址钉定仓库提交
//!   （`resolve/<提交>/<文件名>`），仓库、提交与已知哈希表都在 `rummage_embed::space` 与 `npu`。每个文件带自己的仓库与提交
//!   （`Pinned`）：Android 的 LiteRT 模型与分词器在两个仓库中。
//! - 高通 NPU 的 QNN 运行库（`rummage_embed::npu`）从 Maven Central 的 AAR 中取：不下载整个包，按记下的位置用 HTTP Range
//!   先取条目的本地文件头（得到文件名与扩展字段的长度），再取压缩数据，解压（deflate）到 `.part`，核对大小与 SHA-256 后改名。
//!   不走 Hugging Face 的下载源，同样跟随代理设置；文件小（每个不超过约 5 MB），不续传。
//! - 下载前请求 `/api/models/<仓库>/tree/<提交>` 取文件大小与 `lfs.oid`（即 SHA-256），与已知表核对，不一致时拒绝
//!   （镜像可能不同步或内容被替换，向量空间也按已知哈希计算）；接口不可用（返回错误状态或内容无法解析）时直接用已知表。
//!   连不上下载源时直接报错，不再请求文件。
//! - 已有的文件先核对大小与 SHA-256，正确的跳过，全部正确时不联网（开发版数据目录中已有的模型不会被重新下载或覆盖）。
//! - 断点续传：写入同目录的 `<文件名>.part`；续传前先对已下载的部分计算哈希，再用 HTTP `Range` 请求剩下的部分，
//!   服务器不支持 Range（返回 200），或 206 回复的 Content-Range 起点与已下载的长度不符时，删掉 `.part` 从头下载。完成后核对大小与 SHA-256，通过才改名为正式文件（同一目录内改名，
//!   原子替换），不通过时删除 `.part` 并报错。取消或网络中断时保留 `.part`，下次续传。
//! - 导入：按大小找出对应的模型文件，边复制到 `<文件名>.import.part`（不碰下载用的 `.part`）边计算哈希，
//!   与已知哈希一致才改名；不认识的文件报错。
//! - 校验（`verify`）：对已安装的文件完整核对大小与 SHA-256，结果由调用方记下。
//! - 删除（`remove`）：删除模型文件与下载、导入留下的临时文件；LiteRT 为它生成的缓存另由 `remove_caches` 删除。调用方先让推理进程退出；Windows 上文件仍被其他程序
//!   打开时删不掉，提示稍后重试，不做强制删除。
//! - 给用户看的错误都是带处理建议的中文整句，英文的系统与网络错误只写进日志。
//! - 读取文件内容前都先检查网盘占位文件属性（`rummage_index::ensure_local`）。
//! - 网络错误转换成中文原因，能区分是不是代理服务器的问题。
//!
//! 任务在调用方的线程上运行（核心为每个任务开专用线程）；下载用一个单线程 tokio 运行时，读取与取消同时等待。
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use reqwest::header::{CONTENT_RANGE, RANGE};
use reqwest::{Client, StatusCode, Url};
use rummage_embed::npu::{self, QnnEntry};
use rummage_embed::space::{self, ModelFile};
use rummage_proto::{ModelSource, ModelTaskPhase};
use sha2::{Digest, Sha256};

use crate::proxy::Route;

pub const OFFICIAL: &str = "https://huggingface.co";
pub const MIRROR: &str = "https://hf-mirror.com";
const USER_AGENT: &str = concat!("rummage/", env!("CARGO_PKG_VERSION"));
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// 读取响应时超过这么久没有收到数据就判为超时
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// 进度回调的最小间隔
const REPORT_EVERY: Duration = Duration::from_millis(250);
/// 取消标志的检查间隔
const CANCEL_POLL: Duration = Duration::from_millis(200);
const BUF: usize = 1 << 20;

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("已取消。")]
    Cancelled,
    /// 给用户看的中文整句
    #[error("{0}")]
    Failed(String),
}

/// 钉定在某个仓库提交上的模型文件
#[derive(Debug, Clone, Copy)]
pub struct Pinned {
    pub file: &'static ModelFile,
    pub repo: &'static str,
    pub commit: &'static str,
}

impl Pinned {
    /// 电脑的 GGUF 模型（`space::REPO`）
    pub const fn gguf(file: &'static ModelFile) -> Self {
        Pinned {
            file,
            repo: space::REPO,
            commit: space::COMMIT,
        }
    }

    /// LiteRT 的通用版与 NPU 版模型（`space::LITERT_REPO`）
    pub const fn litert(file: &'static ModelFile) -> Self {
        Pinned {
            file,
            repo: space::LITERT_REPO,
            commit: space::LITERT_COMMIT,
        }
    }

    /// LiteRT 数切片长度用的分词器（`space::TOKENIZER_REPO`）
    pub const TOKENIZER: Pinned = Pinned {
        file: &space::TOKENIZER,
        repo: space::TOKENIZER_REPO,
        commit: space::TOKENIZER_COMMIT,
    };

    /// 钉定版本的文件页面
    pub fn page(&self, base: &str) -> String {
        format!(
            "{base}/{}/blob/{}/{}",
            self.repo, self.commit, self.file.name
        )
    }
}

/// 当前文件的进度
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub phase: ModelTaskPhase,
    pub file: &'static str,
    /// 第几个文件（从 1 开始）与共几个
    pub index: u32,
    pub count: u32,
    pub done: u64,
    pub total: u64,
    /// 下载速度（字节每秒）
    pub speed: Option<u64>,
}

/// 安装进程默认的 ring 加密实现；已安装时什么也不做
pub fn install_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// 下载源的基地址（不带末尾的 `/`）。自定义地址可以带路径前缀，但必须是 http 或 https，不能带查询参数
pub fn base_url(source: &ModelSource) -> Result<String, String> {
    let url = match source {
        ModelSource::Official => return Ok(OFFICIAL.into()),
        ModelSource::Mirror => return Ok(MIRROR.into()),
        ModelSource::Custom { url } => url.trim().trim_end_matches('/'),
    };
    const HINT: &str = "例如 https://hf-mirror.com";
    if url.is_empty() {
        return Err(format!("请填写下载源地址，{HINT}。"));
    }
    let parsed = Url::parse(url).map_err(|_| format!("下载源地址格式不正确，{HINT}。"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(format!("下载源地址需以 http:// 或 https:// 开头，{HINT}。"));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(format!("下载源地址不能带查询参数，{HINT}。"));
    }
    Ok(url.to_string())
}

/// 节流后交给回调的进度
struct Reporter<'a> {
    cb: &'a mut dyn FnMut(&Progress),
    p: Progress,
    last: Instant,
    /// 计算速度的起点：时刻与当时已处理的字节数
    since: (Instant, u64),
}

impl<'a> Reporter<'a> {
    fn new(cb: &'a mut dyn FnMut(&Progress), count: usize) -> Self {
        let now = Instant::now();
        Reporter {
            cb,
            p: Progress {
                phase: ModelTaskPhase::Preparing,
                file: "",
                index: 0,
                count: count as u32,
                done: 0,
                total: 0,
                speed: None,
            },
            last: now,
            since: (now, 0),
        }
    }

    /// 进入新的一步，立即回调；`index` 从 0 开始
    fn start(&mut self, phase: ModelTaskPhase, file: &ModelFile, index: usize, done: u64) {
        let now = Instant::now();
        self.p = Progress {
            phase,
            file: file.name,
            index: index as u32 + 1,
            count: self.p.count,
            done,
            total: file.size,
            speed: None,
        };
        self.since = (now, done);
        self.last = now;
        (self.cb)(&self.p);
    }

    fn advance(&mut self, done: u64) {
        self.p.done = done;
        let now = Instant::now();
        if now.duration_since(self.last) < REPORT_EVERY {
            return;
        }
        self.last = now;
        if self.p.phase == ModelTaskPhase::Downloading {
            let secs = now.duration_since(self.since.0).as_secs_f64();
            self.p.speed = Some(((done - self.since.1) as f64 / secs.max(0.001)) as u64);
        }
        (self.cb)(&self.p);
    }
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn read_error(path: &Path, err: io::Error) -> Error {
    log::warn!("无法读取 {}：{err}", path.display());
    Error::Failed(format!(
        "无法读取 \"{}\"，请确认文件存在、没有被其他程序占用后重试。",
        path.display()
    ))
}

fn write_error(err: io::Error) -> Error {
    log::warn!("无法写入模型文件：{err}");
    if err.kind() == ErrorKind::StorageFull {
        return Error::Failed(
            "磁盘空间不足，无法保存模型文件，请清理数据目录所在的磁盘后重试。".into(),
        );
    }
    Error::Failed("无法保存模型文件，请检查数据目录所在磁盘的剩余空间与权限后重试。".into())
}

/// 改名为正式文件失败。Windows 上目标被占用（ERROR_ACCESS_DENIED、ERROR_SHARING_VIOLATION，例如推理进程正加载着
/// 旧文件）时提示退出后重试
fn rename_error(err: io::Error) -> Error {
    if cfg!(windows) && matches!(err.raw_os_error(), Some(5 | 32)) {
        log::warn!("无法替换模型文件，文件正被使用：{err}");
        return Error::Failed("模型文件正被使用，请从托盘退出 Rummage 后重试。".into());
    }
    write_error(err)
}

/// 计算整个文件的 SHA-256，返回哈希状态（续传时接着用）与读到的字节数；读取前检查占位文件属性
fn hash_file(path: &Path, cancel: &AtomicBool, rep: &mut Reporter) -> Result<(Sha256, u64), Error> {
    rummage_index::ensure_local(path).map_err(|err| Error::Failed(err.to_string()))?;
    let mut f = File::open(path).map_err(|err| read_error(path, err))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; BUF];
    let mut n = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let len = f.read(&mut buf).map_err(|err| read_error(path, err))?;
        if len == 0 {
            return Ok((hasher, n));
        }
        hasher.update(&buf[..len]);
        n += len as u64;
        rep.advance(n);
    }
}

/// `path` 是完整、正确的模型文件；不存在或大小不对时不读内容
fn valid(
    path: &Path,
    file: &ModelFile,
    cancel: &AtomicBool,
    rep: &mut Reporter,
) -> Result<bool, Error> {
    match fs::metadata(path) {
        Ok(m) if m.is_file() && m.len() == file.size => {}
        Ok(_) => return Ok(false),
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(read_error(path, err)),
    }
    let (hasher, _) = hash_file(path, cancel, rep)?;
    Ok(hex(&hasher.finalize()) == file.sha256)
}

/// 等到取消标志被设置
async fn cancelled(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(CANCEL_POLL).await;
    }
}

/// 下载 `files` 到 `dir`（`<数据目录>\models\<子目录>`），再从 QNN 包中取出 `qnn` 到 `qnn_dir`（见模块说明），
/// 已正确的文件跳过。`base` 为 `base_url` 的结果
#[allow(clippy::too_many_arguments)]
pub fn download(
    base: &str,
    files: &[Pinned],
    dir: &Path,
    qnn: &[&'static QnnEntry],
    qnn_dir: &Path,
    route: &Route,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<(), Error> {
    let mut rep = Reporter::new(progress, files.len() + qnn.len());
    let mut todo = Vec::new();
    for (i, f) in files.iter().enumerate() {
        rep.start(ModelTaskPhase::Checking, f.file, i, 0);
        if !valid(&dir.join(f.file.name), f.file, cancel, &mut rep)? {
            todo.push((i, *f));
        }
    }
    let mut qnn_todo = Vec::new();
    for (i, e) in qnn.iter().enumerate() {
        rep.start(ModelTaskPhase::Checking, &e.file, files.len() + i, 0);
        if !valid(&qnn_dir.join(e.file.name), &e.file, cancel, &mut rep)? {
            qnn_todo.push((files.len() + i, *e));
        }
    }
    if todo.is_empty() && qnn_todo.is_empty() {
        return Ok(());
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            log::error!("无法创建下载用的运行时：{err}");
            Error::Failed("无法开始下载，请重试；仍然失败时请重新启动 Rummage。".into())
        })?;
    rt.block_on(async {
        let builder = Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT);
        let client = route
            .apply(builder)
            .and_then(|b| b.build())
            .map_err(|err| {
                log::error!("无法创建下载用的网络客户端：{err}");
                Error::Failed("无法创建网络连接，请检查代理设置后重试。".into())
            })?;
        if !todo.is_empty() {
            fs::create_dir_all(dir).map_err(write_error)?;
        }
        // 每个仓库只读一次文件列表
        let mut lists: Vec<(&str, Option<Vec<Remote>>)> = Vec::new();
        for (i, f) in todo {
            let known = lists.iter().position(|(repo, _)| *repo == f.repo);
            let list = match known {
                Some(k) => &lists[k].1,
                None => {
                    rep.start(ModelTaskPhase::Preparing, f.file, i, 0);
                    let info = remote_info(&client, route, base, &f, cancel).await?;
                    lists.push((f.repo, info));
                    &lists[lists.len() - 1].1
                }
            };
            if let Some(list) = list {
                check_remote(list, f.file)?;
            }
            let url = format!("{base}/{}/resolve/{}/{}", f.repo, f.commit, f.file.name);
            let url = Url::parse(&url)
                .map_err(|_| Error::Failed("下载源地址格式不正确，请检查后重试。".into()))?;
            let part = dir.join(format!("{}.part", f.file.name));
            fetch(&client, route, &url, f.file, &part, i, cancel, &mut rep).await?;
            fs::rename(&part, dir.join(f.file.name)).map_err(rename_error)?;
        }
        if !qnn_todo.is_empty() {
            fs::create_dir_all(qnn_dir).map_err(write_error)?;
        }
        for (i, e) in qnn_todo {
            let part = qnn_dir.join(format!("{}.part", e.file.name));
            fetch_qnn(&client, route, e, &part, i, cancel, &mut rep).await?;
            fs::rename(&part, qnn_dir.join(e.file.name)).map_err(rename_error)?;
        }
        Ok(())
    })
}

/// 用 HTTP Range 取 `[start, start + len)`；须回 206 且起点相符
async fn range(
    client: &Client,
    url: &Url,
    proxied: bool,
    start: u64,
    len: u64,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, Error> {
    let req = client
        .get(url.clone())
        .header(RANGE, format!("bytes={start}-{}", start + len - 1));
    let resp = tokio::select! {
        r = req.send() => r.map_err(|err| net_error(&err, proxied))?,
        () = cancelled(cancel) => return Err(Error::Cancelled),
    };
    let status = resp.status();
    if !status.is_success() {
        log::warn!("下载 NPU 运行库时 Maven Central 返回 {status}");
        return Err(status_error(status));
    }
    if status != StatusCode::PARTIAL_CONTENT || range_start(&resp) != Some(start) {
        log::warn!("下载 NPU 运行库时 Maven Central 没有按请求返回部分内容（{status}）。");
        return Err(Error::Failed(
            "下载 NPU 运行库时服务器返回的数据不对，请稍后重试。".into(),
        ));
    }
    let body = tokio::select! {
        b = resp.bytes() => b.map_err(|err| net_error(&err, proxied))?,
        () = cancelled(cancel) => return Err(Error::Cancelled),
    };
    if body.len() as u64 != len {
        return Err(Error::Failed("下载 NPU 运行库时连接中断，请重试。".into()));
    }
    Ok(body.to_vec())
}

/// 从 QNN 包中取出一个条目，解压到 `part` 并核对（见模块说明）
async fn fetch_qnn(
    client: &Client,
    route: &Route,
    entry: &'static QnnEntry,
    part: &Path,
    index: usize,
    cancel: &AtomicBool,
    rep: &mut Reporter<'_>,
) -> Result<(), Error> {
    let url = Url::parse(npu::QNN_URL).expect("内置的地址格式正确");
    let proxied = route.proxies(&url);
    rep.start(ModelTaskPhase::Downloading, &entry.file, index, 0);
    // 本地文件头 30 字节：签名、……、文件名长度（26）、扩展字段长度（28）
    let head = range(client, &url, proxied, entry.offset, 30, cancel).await?;
    let le16 = |i: usize| u64::from(u16::from_le_bytes([head[i], head[i + 1]]));
    if head[..4] != *b"PK\x03\x04" {
        log::warn!("QNN 包中 {} 的位置不是本地文件头。", entry.file.name);
        return Err(Error::Failed(
            "下载的 NPU 运行库与预期的不同，请稍后重试。".into(),
        ));
    }
    let data = entry.offset + 30 + le16(26) + le16(28);
    let packed = range(client, &url, proxied, data, entry.compressed, cancel).await?;
    rep.advance(entry.file.size / 2);
    let mut raw = Vec::with_capacity(entry.file.size as usize);
    flate2::read::DeflateDecoder::new(&packed[..])
        .take(entry.file.size + 1)
        .read_to_end(&mut raw)
        .map_err(|err| {
            log::warn!("无法解压 QNN 包中的 {}：{err}", entry.file.name);
            Error::Failed("下载的 NPU 运行库已损坏，请重试。".into())
        })?;
    if raw.len() as u64 != entry.file.size || hex(&Sha256::digest(&raw)) != entry.file.sha256 {
        return Err(Error::Failed(format!(
            "下载的 \"{}\" 不完整或已损坏，请重试。",
            entry.file.name
        )));
    }
    let mut out = File::create(part).map_err(write_error)?;
    out.write_all(&raw).map_err(write_error)?;
    out.sync_all().map_err(write_error)?;
    rep.advance(entry.file.size);
    Ok(())
}

/// 文件列表接口中的一项：(路径, 大小, SHA-256)
type Remote = (String, u64, String);

/// 读取钉定提交的文件列表。`Ok(None)`：接口不可用（错误状态、内容无法解析），改用已知表；连不上下载源时返回错误
async fn remote_info(
    client: &Client,
    route: &Route,
    base: &str,
    pinned: &Pinned,
    cancel: &AtomicBool,
) -> Result<Option<Vec<Remote>>, Error> {
    let url = format!("{base}/api/models/{}/tree/{}", pinned.repo, pinned.commit);
    let Ok(url) = Url::parse(&url) else {
        return Err(Error::Failed("下载源地址格式不正确，请检查后重试。".into()));
    };
    let send = async {
        let resp = client.get(url.clone()).send().await?;
        if !resp.status().is_success() {
            log::warn!(
                "下载源的文件列表接口返回 {}，改用内置的文件信息。",
                resp.status()
            );
            return Ok(None);
        }
        resp.bytes().await.map(Some)
    };
    let body = tokio::select! {
        r = send => r.map_err(|err| net_error(&err, route.proxies(&url)))?,
        () = cancelled(cancel) => return Err(Error::Cancelled),
    };
    let Some(body) = body else {
        return Ok(None);
    };
    let list = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            let list = v.as_array()?.iter().filter_map(|e| {
                // 非 LFS 的小文件没有 lfs 字段，跳过
                let lfs = e.get("lfs")?;
                Some((
                    e.get("path")?.as_str()?.to_string(),
                    lfs.get("size")?.as_u64()?,
                    lfs.get("oid")?.as_str()?.to_lowercase(),
                ))
            });
            Some(list.collect::<Vec<_>>())
        });
    if list.is_none() {
        log::warn!("无法解析下载源的文件列表，改用内置的文件信息。");
    }
    Ok(list)
}

/// 下载源列出的文件信息须与已知表一致
fn check_remote(list: &[Remote], file: &ModelFile) -> Result<(), Error> {
    match list.iter().find(|(path, _, _)| path == file.name) {
        Some((_, size, sha)) if *size != file.size || sha != file.sha256 => {
            log::warn!(
                "下载源列出的 {} 为 {size} 字节、SHA-256 {sha}，与内置记录不一致。",
                file.name
            );
            Err(Error::Failed(format!(
                "下载源提供的 \"{}\" 与 Rummage 需要的版本不一致，请换一个下载源。",
                file.name
            )))
        }
        Some(_) => Ok(()),
        None => {
            log::warn!(
                "下载源的文件列表中没有 {}，按内置的文件信息校验。",
                file.name
            );
            Ok(())
        }
    }
}

/// 206 回复中 Content-Range 的起点
fn range_start(resp: &reqwest::Response) -> Option<u64> {
    let v = resp.headers().get(CONTENT_RANGE)?.to_str().ok()?;
    v.strip_prefix("bytes ")?.split('-').next()?.parse().ok()
}

/// 下载一个文件到 `part`，续传已有的部分；完成并校验通过后返回，`.part` 由调用方改名
#[allow(clippy::too_many_arguments)]
async fn fetch(
    client: &Client,
    route: &Route,
    url: &Url,
    file: &'static ModelFile,
    part: &Path,
    index: usize,
    cancel: &AtomicBool,
    rep: &mut Reporter<'_>,
) -> Result<(), Error> {
    let proxied = route.proxies(url);
    let mut have = fs::metadata(part).map_or(0, |m| m.len());
    if have > file.size {
        fs::remove_file(part).map_err(write_error)?;
        have = 0;
    }
    let mut hasher = Sha256::new();
    if have > 0 {
        rep.start(ModelTaskPhase::Checking, file, index, 0);
        (hasher, have) = hash_file(part, cancel, rep)?;
    }
    if have < file.size {
        // 续传的回复不对时删掉 `.part` 再从头请求一次；从头请求时不带 Range，最多再循环一次
        let mut resp = loop {
            let mut req = client.get(url.clone());
            if have > 0 {
                req = req.header(RANGE, format!("bytes={have}-"));
            }
            let resp = tokio::select! {
                r = req.send() => r.map_err(|err| net_error(&err, proxied))?,
                () = cancelled(cancel) => return Err(Error::Cancelled),
            };
            let status = resp.status();
            // 已下载的部分无效（文件在服务器上变了）：删掉，下次从头下载
            if status == StatusCode::RANGE_NOT_SATISFIABLE {
                fs::remove_file(part).map_err(write_error)?;
                return Err(Error::Failed(
                    "下载源不接受续传，已删除下载了一部分的文件，请重试。".into(),
                ));
            }
            if !status.is_success() {
                log::warn!("下载 {} 时下载源返回 {status}", file.name);
                return Err(status_error(status));
            }
            if status == StatusCode::PARTIAL_CONTENT && range_start(&resp) != Some(have) {
                if have == 0 {
                    log::warn!("下载 {} 时下载源返回了没有请求过的部分内容。", file.name);
                    return Err(Error::Failed(
                        "下载源返回的数据不完整，请重试或换一个下载源。".into(),
                    ));
                }
                log::warn!(
                    "下载源返回的续传位置与已下载的 {have} 字节不符，从头下载 {}。",
                    file.name
                );
                drop(resp);
                fs::remove_file(part).map_err(write_error)?;
                have = 0;
                hasher = Sha256::new();
                continue;
            }
            break resp;
        };
        let resume = have > 0 && resp.status() == StatusCode::PARTIAL_CONTENT;
        let mut out = if resume {
            OpenOptions::new().append(true).open(part)
        } else {
            have = 0;
            hasher = Sha256::new();
            File::create(part)
        }
        .map_err(write_error)?;
        rep.start(ModelTaskPhase::Downloading, file, index, have);
        loop {
            let chunk = tokio::select! {
                c = resp.chunk() => c.map_err(|err| net_error(&err, proxied))?,
                () = cancelled(cancel) => return Err(Error::Cancelled),
            };
            let Some(chunk) = chunk else {
                break;
            };
            if have + chunk.len() as u64 > file.size {
                drop(out);
                fs::remove_file(part).map_err(write_error)?;
                return Err(Error::Failed(format!(
                    "下载源返回的 \"{}\" 比预期的大，可能不是所需的版本，请换一个下载源。",
                    file.name
                )));
            }
            out.write_all(&chunk).map_err(write_error)?;
            hasher.update(&chunk);
            have += chunk.len() as u64;
            rep.advance(have);
        }
        out.sync_all().map_err(write_error)?;
    }
    if have != file.size {
        return Err(Error::Failed(format!(
            "下载中断（\"{}\" 只收到 {have} / {} 字节），请重试，会从中断处继续。",
            file.name, file.size
        )));
    }
    if hex(&hasher.finalize()) != file.sha256 {
        fs::remove_file(part).map_err(write_error)?;
        return Err(Error::Failed(format!(
            "下载的 \"{}\" 不完整或已损坏，已删除。请重试，或换一个下载源。",
            file.name
        )));
    }
    Ok(())
}

fn status_error(status: StatusCode) -> Error {
    let code = status.as_u16();
    Error::Failed(match code {
        404 => "下载源上找不到模型文件，请换一个下载源。".into(),
        401 | 403 => format!("下载源拒绝了访问（HTTP {code}），请换一个下载源。"),
        407 => "代理服务器要求身份验证，请检查代理地址中的用户名和密码。".into(),
        429 => "下载源限制了请求频率，请稍后再试或换一个下载源。".into(),
        500..=599 => format!("下载源暂时不可用（HTTP {code}），请稍后再试或换一个下载源。"),
        _ => format!("下载源返回了错误（HTTP {code}），请换一个下载源。"),
    })
}

/// 网络错误的中文原因。`proxied` 为这次请求经过自定义代理；
/// 跟随系统时不知道是否经过代理，只能从 reqwest 的隧道错误判断
fn net_error(err: &reqwest::Error, proxied: bool) -> Error {
    let mut chain = err.to_string();
    let mut kind = None;
    let mut code = None;
    let mut source = std::error::Error::source(err);
    while let Some(e) = source {
        chain.push_str(": ");
        chain.push_str(&e.to_string());
        if let Some(io) = e.downcast_ref::<io::Error>() {
            kind = kind.or(Some(io.kind()));
            code = code.or(io.raw_os_error());
        }
        source = e.source();
    }
    log::warn!("下载模型时网络出错：{chain}");
    let lower = chain.to_lowercase();
    let tls = ["certificate", "tls", "handshake", "corrupt message"]
        .iter()
        .any(|s| lower.contains(s));
    let retry = "请检查网络，或换用镜像、设置代理";
    let reason = if lower.contains("proxy authorization required") {
        "代理服务器要求身份验证，请检查代理地址中的用户名和密码".to_string()
    } else if lower.contains("tunnel error: unsuccessful") {
        "代理服务器未能连接到下载源，请检查代理软件的设置，或换用镜像".to_string()
    } else if lower.contains("tunnel error: failed to create underlying connection")
        || (proxied && err.is_connect() && !tls)
    {
        "无法连接代理服务器，请检查代理地址和端口是否正确、代理软件是否已启动".to_string()
    } else if err.is_timeout() || kind == Some(ErrorKind::TimedOut) {
        format!("连接下载源超时，{retry}")
    } else if lower.contains("dns error") {
        format!("无法解析下载源的域名，{retry}")
    } else if kind == Some(ErrorKind::ConnectionRefused) {
        "连接被拒绝，请检查下载源地址是否正确".to_string()
    } else if matches!(
        kind,
        Some(ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted)
    ) {
        format!("连接被重置，{retry}")
    } else if lower.contains("connection closed before message completed") {
        "下载源未返回数据就关闭了连接，请重试".to_string()
    } else if tls {
        "TLS 握手失败，无法验证下载源的证书，请检查网络环境或代理设置".to_string()
    } else if err.is_redirect() {
        "下载源重定向次数过多，请换一个下载源".to_string()
    } else {
        format!("网络连接出错，{retry}")
    };
    let note = code.map_or_else(String::new, |c| format!("（错误码 {c}）"));
    Error::Failed(format!("{reason}{note}。"))
}

/// 导入用户选择的模型文件：按大小在 `known` 中找到对应的文件，复制到 `dir` 并校验 SHA-256。
/// 返回导入（或已存在且正确）的文件；有不认识的文件时一个也不导入
pub fn import(
    paths: &[PathBuf],
    known: &[&'static ModelFile],
    dir: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<Vec<&'static ModelFile>, Error> {
    let mut picked = Vec::new();
    for path in paths {
        let name = path
            .file_name()
            .map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy());
        rummage_index::ensure_local(path).map_err(|err| Error::Failed(err.to_string()))?;
        let len = fs::metadata(path)
            .map_err(|err| read_error(path, err))?
            .len();
        let Some(file) = known.iter().find(|f| f.size == len) else {
            let names: Vec<&str> = known.iter().map(|f| f.name).collect();
            return Err(Error::Failed(format!(
                "\"{name}\" 不是 Rummage 使用的模型文件，请选择 {}。",
                names
                    .iter()
                    .map(|n| format!("\"{n}\""))
                    .collect::<Vec<_>>()
                    .join(" 或 ")
            )));
        };
        picked.push((path, *file));
    }
    let mut rep = Reporter::new(progress, picked.len());
    fs::create_dir_all(dir).map_err(write_error)?;
    let mut done = Vec::new();
    for (i, (src, file)) in picked.into_iter().enumerate() {
        let target = dir.join(file.name);
        rep.start(ModelTaskPhase::Checking, file, i, 0);
        if !valid(&target, file, cancel, &mut rep)? {
            rep.start(ModelTaskPhase::Copying, file, i, 0);
            let part = dir.join(format!("{}.import.part", file.name));
            let sha = copy_hashed(src, &part, cancel, &mut rep)?;
            if sha != file.sha256 {
                fs::remove_file(&part).map_err(write_error)?;
                let name = src.file_name().unwrap_or_default().to_string_lossy();
                return Err(Error::Failed(format!(
                    "\"{name}\" 的内容与 Rummage 需要的模型不一致，可能已损坏或不是所需的版本。"
                )));
            }
            fs::rename(&part, &target).map_err(rename_error)?;
        }
        if !done.iter().any(|f: &&ModelFile| f.name == file.name) {
            done.push(file);
        }
    }
    Ok(done)
}

/// 完整校验 `path` 处已安装的模型文件；文件不存在或大小不对时为 false
pub fn verify(
    path: &Path,
    file: &'static ModelFile,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<bool, Error> {
    let mut rep = Reporter::new(progress, 1);
    rep.start(ModelTaskPhase::Checking, file, 0, 0);
    valid(path, file, cancel, &mut rep)
}

/// 删除 `dir` 中的模型文件及其 `.part`、`.import.part`；不存在的跳过。`label` 是给用户看的模型名称。
/// 返回模型文件本身是否存在（否则只删了下载或导入了一部分的文件）
pub fn remove(dir: &Path, file: &ModelFile, label: &str) -> Result<bool, Error> {
    let installed = dir.join(file.name).is_file();
    let names = [
        file.name.to_string(),
        format!("{}.part", file.name),
        format!("{}.import.part", file.name),
    ];
    for name in names {
        match fs::remove_file(dir.join(&name)) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            // ERROR_ACCESS_DENIED、ERROR_SHARING_VIOLATION：文件仍被打开（推理进程没有及时退出，或被杀毒软件等扫描）
            Err(err) if cfg!(windows) && matches!(err.raw_os_error(), Some(5 | 32)) => {
                log::warn!("无法删除 {name}，文件正被使用：{err}");
                return Err(Error::Failed(format!(
                    "{label}正被使用，暂时无法删除，请稍后再试。"
                )));
            }
            Err(err) => {
                log::warn!("无法删除 {name}：{err}");
                return Err(Error::Failed(format!(
                    "无法删除{label}，请检查数据目录的权限后重试。"
                )));
            }
        }
    }
    Ok(installed)
}

/// 删除 LiteRT 在 `dir` 中为模型文件 `name` 生成的权重与程序缓存（`<name>.…`、`<name>_…`）。缓存名带模型文件的修改时间
/// 与大小，重新下载的模型不会再用旧缓存；删不掉的只记录警告
pub fn remove_caches(dir: &Path, name: &str) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let file = entry.file_name();
        let is_cache = file
            .to_string_lossy()
            .strip_prefix(name)
            .is_some_and(|rest| rest.starts_with(['.', '_']));
        if is_cache && let Err(err) = fs::remove_file(entry.path()) {
            log::warn!("无法删除模型缓存 {}：{err}", entry.path().display());
        }
    }
}

/// 复制文件并计算 SHA-256；取消或出错时删除写了一半的目标
fn copy_hashed(
    src: &Path,
    dst: &Path,
    cancel: &AtomicBool,
    rep: &mut Reporter,
) -> Result<String, Error> {
    let mut input = File::open(src).map_err(|err| read_error(src, err))?;
    let mut out = File::create(dst).map_err(write_error)?;
    let res = (|| {
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; BUF];
        let mut n = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let len = input.read(&mut buf).map_err(|err| read_error(src, err))?;
            if len == 0 {
                break;
            }
            out.write_all(&buf[..len]).map_err(write_error)?;
            hasher.update(&buf[..len]);
            n += len as u64;
            rep.advance(n);
        }
        out.sync_all().map_err(write_error)?;
        Ok(hex(&hasher.finalize()))
    })();
    if res.is_err() {
        drop(out);
        let _ = fs::remove_file(dst);
    }
    res
}

#[cfg(test)]
mod tests;
