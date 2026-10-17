//! 界面进程、命令行、MCP 与核心进程之间的 IPC 消息。
//! 传输层是换行分隔的 JSON，一行一条消息，由 `encode` / `decode` 编解码。
//! 流程：连接后客户端先发 `Hello`（协议版本与令牌），核心回 `Ok(Welcome)` 或 `Err(ApiError)`；
//! 握手成功后客户端发 `ClientMsg::Request`，核心以同一 `id` 回 `CoreMsg::Response`，并随时推送 `CoreMsg::Event`。
//! 两端由同一版本的本 crate 编译，协议版本不一致时在握手阶段以 `protocol_mismatch` 拒绝。
pub mod config;
pub mod params;
pub mod settings;
pub mod setup;

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub use params::{Group, ParamDef, ParamKind, ParamValue};
pub use settings::{Appearance, FontMode, Notice};
pub use setup::{AUTO_DEVICE, CPU_DEVICE};
pub use setup::{
    BlockedGpu, DeviceResult, DeviceSpeed, DeviceStatus, Estimate, GpuState, KindTotal,
    ModelFileStatus, ModelSource, ModelTask, ModelTaskKind, ModelTaskPhase, ModelTaskState,
    ProxyMode, ProxySetting, RootEstimate, SelftestStatus, SetupStatus,
};

/// 每次修改消息类型都要加 1。
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("消息格式不正确：{0}")]
    Decode(#[from] serde_json::Error),
    /// 参数不存在或取值不合法，内容是给用户看的中文整句
    #[error("{0}")]
    InvalidParam(String),
}

/// 握手：客户端连接后发出的第一条消息
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub protocol: u32,
    pub token: String,
}

/// 握手成功时核心的回复；失败时回 `ApiError`，整体为 `Result<Welcome, ApiError>`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Welcome {
    pub protocol: u32,
    pub core_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub code: String,
    /// 给用户看的中文整句
    pub message: String,
}

impl ApiError {
    pub const UNAUTHORIZED: &str = "unauthorized";
    pub const PROTOCOL_MISMATCH: &str = "protocol_mismatch";
    pub const BAD_REQUEST: &str = "bad_request";
    pub const INTERNAL: &str = "internal";
    pub const UNAVAILABLE: &str = "unavailable";
}

/// 客户端发出的消息
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum ClientMsg {
    Request { id: u64, body: Request },
}

/// 核心发出的消息
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum CoreMsg {
    Response {
        id: u64,
        result: Result<Reply, ApiError>,
    },
    Event {
        body: Event,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum Request {
    Ping,
    GetParams,
    /// 保存参数，回复 `Done`；影响索引的参数已保存、但没能安排重建时回 `DoneWithWarning`
    SetParam {
        key: String,
        value: ParamValue,
    },
    /// 让核心回复 `Done` 后正常退出（开发时重新构建前、界面发现核心版本不一致时）
    Shutdown,
    ListRoots,
    /// 添加被索引的文件夹，成功后立即在后台扫描它，回复 `Reply::Root`（新建的文件夹）。`excludes` 为它的用户排除规则
    /// （同 `SetRootExcludes`），有写法不正确的规则时不添加，回 `bad_request`
    AddRoot {
        path: String,
        excludes: Vec<String>,
    },
    /// 移除文件夹及其全部索引
    RemoveRoot {
        id: i64,
    },
    /// 索引进度，回复 `Reply::Index`
    IndexStatus,
    /// 失败的文件，最近失败的在前，回复 `Reply::FailedFiles`。
    /// `after` 为上一页最后一行的游标，为空时从头取
    FailedFiles {
        after: Option<FailedCursor>,
        limit: u32,
    },
    /// 重试一个失败的文件：移出失败记录、清零失败次数后重新排队，回复 `Done`；
    /// 文件已不是失败状态或是加密文档（不能重试）时什么也不做
    RetryFailed {
        file_id: i64,
    },
    /// 重试全部能重试的失败文件（加密文档除外），回复 `Done`
    RetryAllFailed,
    /// 忽略一个失败的文件：在它所属文件夹的排除规则中加入它的精确路径，并从索引中移除，之后不再跟进它的变化，
    /// 回复 `Done`；文件已不是失败状态时什么也不做
    IgnoreFailed {
        file_id: i64,
    },
    /// 忽略全部失败的文件（含加密文档），回复 `Done`
    IgnoreAllFailed,
    /// 手动暂停建索引，重启核心后仍然有效。`until` 为 Unix 毫秒，到期后自动恢复；
    /// 为空时一直暂停到 `ResumeIndexing`。“到明天”等选项由界面换算成 `until`
    PauseIndexing {
        until: Option<i64>,
    },
    /// 取消手动暂停；自动暂停的条件（全屏、电池、磁盘空间）仍然生效
    ResumeIndexing,
    /// Android：前台服务今日的后台运行额度是否已用完。用完时以 `PauseReason::Quota` 暂停领取作业，
    /// 应用回到前台后由界面解除。不持久化，核心重启后清除
    BackgroundQuota {
        exhausted: bool,
    },
    /// 搜索，回复 `Reply::Results`。`similar` 不为空时“找相似的”（用已存的向量，忽略 `image_path`；`text` 不为空时
    /// 只限图片，重新读取该图片与文字合成一条查询）；`image_path` 不为空时以图搜图（不走文件名通道，`text` 为附加的
    /// 文字描述，与图片合成一条查询），路径只用于计算查询向量；
    /// 否则按 `text` 做语义与文件名检索。
    /// `kinds`、`root_ids` 为空表示不限，`since`、`until` 为修改时间（Unix 毫秒，含两端），
    /// `limit` 为空时用参数 `result_limit`；`snippets` 为真时给文档结果附正文片段（`SearchHit::snippet`，命令行用）
    Search {
        text: Option<String>,
        image_path: Option<String>,
        similar: Option<SimilarTo>,
        kinds: Vec<String>,
        root_ids: Vec<i64>,
        since: Option<i64>,
        until: Option<i64>,
        limit: Option<u32>,
        snippets: bool,
    },
    /// 按索引中的文件 ID 取路径等信息（打开、在资源管理器中显示、预览），回复 `Reply::File`；
    /// 界面只能用索引中的 ID，不能传任意路径
    FileInfo {
        file_id: i64,
    },
    /// 取文件的缩略图，回复 `Reply::Thumbnail`：已缓存时直接返回，否则生成后存入 `<数据目录>\thumbs\`。
    /// 网盘占位文件、设备未连接时回 `bad_request`
    Thumbnail {
        file_id: i64,
    },
    /// 片段的正文与前后文（文档预览），回复 `Reply::Segment`
    SegmentText {
        segment_id: i64,
    },
    /// 视频在 `ms` 毫秒处的一帧画面，回复 `Reply::Thumbnail`：已缓存时直接返回，否则用 ffmpeg 现取后存入
    /// 缩略图缓存。非视频、网盘占位文件、设备未连接时回 `bad_request`
    VideoFrame {
        file_id: i64,
        ms: u64,
    },
    /// 长图切片片段在原图中的区域的裁剪图，回复 `Reply::Thumbnail`：区域从片段的 locator 中读出，
    /// 界面不能指定任意区域。不是长图切片、网盘占位文件、设备未连接时回 `bad_request`
    RegionThumb {
        segment_id: i64,
    },
    /// 引导状态（`setup`），回复 `Reply::Setup`
    SetupStatus,
    /// 完成引导，之后不再显示，并清除记下的引导步骤；唤醒索引流水线
    FinishSetup,
    /// 记下引导进行到第几步，重启（包括更改数据目录后的重启）后从这一步继续
    SetOnboardingStep {
        step: u32,
    },
    /// 在后台下载模型文件：主模型，`mmproj` 为真时再加 mmproj；已正确的文件跳过。
    /// 进度以 `ModelTask` 事件推送；已有任务在进行时回 `bad_request`
    DownloadModels {
        source: ModelSource,
        mmproj: bool,
    },
    /// 在后台导入用户选择的模型文件：按大小与 SHA-256 识别主模型和 mmproj，复制到模型目录。
    /// 路径由界面进程的系统文件对话框给出，前端不能提交路径。进度同 `DownloadModels`
    ImportModels {
        paths: Vec<String>,
    },
    /// 取消进行中的下载、导入或校验；已下载的部分保留，下次续传（设置中的“暂停”）
    CancelModelTask,
    /// 在后台完整校验一个已安装的模型文件：`mmproj` 为真时校验 mmproj，否则校验主模型。进度同 `DownloadModels`，
    /// 结果记下来，之后在 `ModelFileStatus::verified` 中返回
    VerifyModel {
        mmproj: bool,
    },
    /// 引导进入下载模型一步时，在后台依次完整校验已安装、没有校验记录的模型文件：主模型，`mmproj` 为真时再加 mmproj；
    /// 记为未通过的不重校，没有要校验的文件时不开始任务。进度与结果同 `VerifyModel`
    VerifyModels {
        mmproj: bool,
    },
    /// 在后台删除一个模型文件（或它下载了一部分的文件）：`mmproj` 为真时删除 mmproj，否则删除主模型。先让推理进程释放文件；
    /// 已建立的索引保留。删除主模型后不能语义检索、也不再识别新内容，索引状态为 `no_model`，重新下载或导入后恢复。进度同 `DownloadModels`
    DeleteModel {
        mmproj: bool,
    },
    /// 保存代理设置，回复 `Reply::Proxy`（地址已规范化）；地址不合法时回 `bad_request`
    SetProxy {
        setting: ProxySetting,
    },
    /// 在后台运行设备自检，进度以 `Selftest` 事件推送；完成后索引流水线改用新结果，不需要重启
    RunSelftest {
        /// 引导中发起：用户此前选了只用 CPU 时改回自动选择（引导中先跳过、后又检测，表示要用显卡）；
        /// 设置页发起时保留用户的选择
        onboarding: bool,
    },
    /// 选择使用的设备：`auto` 按自检记录选择，`cpu` 只用 CPU，或某块通过检测的显卡的 `DeviceResult::id`；
    /// 显卡不能用时回 `bad_request`。选 `cpu` 后再运行自检会改回 `auto`
    SetPreferredDevice {
        device: String,
    },
    /// 解除屏蔽：删除这块显卡（`BlockedGpu::id`）的全部屏蔽记录并清掉它本次运行的停用，随即开始只测它的检测
    /// （其余设备照缓存显示；按手动检测对待，进度以 `Selftest` 事件推送）。不当场重测时（不在系统中、缓存结果没测过本次需要的
    /// 图片与音视频、检测没能开始）回复 `DoneWithWarning` 说明原因。检测进行中或没有屏蔽记录时回 `bad_request`
    UnblockGpu {
        device: String,
    },
    /// 预估索引规模，回复 `Reply::Estimate`
    Estimate,
    /// 保存根目录的用户排除规则（glob），成功后重新扫描该根目录；有写法不正确的规则时回 `bad_request`
    SetRootExcludes {
        id: i64,
        excludes: Vec<String>,
    },
    /// 外观设置，回复 `Reply::Appearance`；没有保存过时为默认值
    GetAppearance,
    /// 保存外观设置；字体名过长时回 `bad_request`
    SetAppearance {
        appearance: Appearance,
    },
    /// 需要在设置页提示的通知，回复 `Reply::Notices`
    GetNotices,
    /// 用户确认后清除一条通知
    DismissNotice {
        notice: Notice,
    },
    /// 索引数据库能否打开，回复 `Reply::DbStatus`
    DbStatus,
    /// 清空索引：删除全部文件、片段、向量与作业记录，保留文件夹、排除规则和设置，随后重新扫描全部文件夹
    ClearIndex,
    /// 最近的搜索，新的在前，回复 `Reply::History`；参数 `save_history` 关闭时为空
    GetHistory,
    /// 记下一次文字搜索；参数 `save_history` 关闭时不记录
    AddHistory {
        query: String,
    },
    RemoveHistory {
        query: String,
    },
    ClearHistory,
}

impl Request {
    /// 不改变应用状态的请求（生成缩略图等缓存不算）。核心对同一连接上的只读请求并发处理，其余按收到的顺序依次处理；
    /// MCP 据此决定连接断开后能否重发。新增请求时在这里归类
    pub fn is_readonly(&self) -> bool {
        match self {
            Request::Ping
            | Request::GetParams
            | Request::ListRoots
            | Request::IndexStatus
            | Request::FailedFiles { .. }
            | Request::Search { .. }
            | Request::FileInfo { .. }
            | Request::Thumbnail { .. }
            | Request::SegmentText { .. }
            | Request::VideoFrame { .. }
            | Request::RegionThumb { .. }
            | Request::SetupStatus
            | Request::Estimate
            | Request::GetAppearance
            | Request::GetNotices
            | Request::DbStatus
            | Request::GetHistory => true,
            Request::SetParam { .. }
            | Request::Shutdown
            | Request::AddRoot { .. }
            | Request::RemoveRoot { .. }
            | Request::RetryFailed { .. }
            | Request::RetryAllFailed
            | Request::IgnoreFailed { .. }
            | Request::IgnoreAllFailed
            | Request::PauseIndexing { .. }
            | Request::ResumeIndexing
            | Request::BackgroundQuota { .. }
            | Request::FinishSetup
            | Request::SetOnboardingStep { .. }
            | Request::DownloadModels { .. }
            | Request::ImportModels { .. }
            | Request::CancelModelTask
            | Request::VerifyModel { .. }
            | Request::VerifyModels { .. }
            | Request::DeleteModel { .. }
            | Request::SetProxy { .. }
            | Request::RunSelftest { .. }
            | Request::SetPreferredDevice { .. }
            | Request::UnblockGpu { .. }
            | Request::SetRootExcludes { .. }
            | Request::SetAppearance { .. }
            | Request::DismissNotice { .. }
            | Request::ClearIndex
            | Request::AddHistory { .. }
            | Request::RemoveHistory { .. }
            | Request::ClearHistory => false,
        }
    }
}

/// “找相似的”的查询：索引中某个文件的片段（视频、长图为命中的那一段，文档为命中的片段），用它已存的向量检索，
/// 不重新推理；只比较同一向量空间、同一模态，结果中不含该文件。`segment_id` 为空时用该文件第一个有向量的片段
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilarTo {
    pub file_id: i64,
    pub segment_id: Option<i64>,
}

/// 对应 `rummage_index::SegmentHit`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentHit {
    pub segment_id: i64,
    /// 页码、章节、时间区间、长图区域等；长图切片为 `{"region":{"x","y","width","height"}}`（原图像素坐标），
    /// 音频段为 `{"start_ms","end_ms"}`（从文件开头算起的毫秒），视频为相邻命中窗口合并后的时间段（写法同音频，
    /// `segment_id` 与 `cosine` 取其中 z 最高的窗口，各段按 z 从高到低），
    /// 整图片段为 `{}`。`SearchHit::segments` 的第一项即最佳命中的片段
    pub locator_json: String,
    pub cosine: f32,
}

/// 文档预览中的一个片段：命中片段的正文，以及前后相邻片段中不与它重叠的一段（各取一个段落，过长时截断）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentPreview {
    pub file_id: i64,
    /// 写法同 `SegmentHit::locator_json`
    pub locator_json: String,
    /// 没有正文（图片、音视频）时为空
    pub text: Option<String>,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// 对应 `rummage_index::Hit`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub file_id: i64,
    pub path: String,
    pub name: String,
    pub kind: String,
    pub size: i64,
    /// Unix 毫秒
    pub mtime: i64,
    /// 网盘占位文件
    pub cloud: bool,
    /// 所在文件夹的设备未连接
    pub offline: bool,
    /// RRF 融合分数
    pub score: f64,
    /// 最高余弦；没有语义命中时为空
    pub cosine: Option<f32>,
    /// 校准后的最高分数（z 分数），界面据此换算匹配度；没有语义命中或还没有校准时为空
    pub z: Option<f32>,
    /// 匹配度百分比（0–100），由 `z` 换算；`z` 为空时为空
    pub match_pct: Option<u8>,
    /// 匹配度较低（低于参数 `min_relevance`，关键词也没有命中），排在其余结果之后，界面默认折叠
    pub low_relevance: bool,
    /// 命中的片段，按校准后的分数（没有时按余弦）从高到低
    pub segments: Vec<SegmentHit>,
    pub name_hit: bool,
    /// 正文关键词命中的片段 id，最匹配的在前；为空表示正文没有命中
    pub text_segments: Vec<i64>,
    /// 图片的宽高（像素，已按 EXIF 方向转正）；未记录时为空，瀑布流据此排版
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// 文档的正文片段：请求带 `snippets` 时才有，没有带文字的命中片段时为空
    pub snippet: Option<Snippet>,
}

/// 搜索结果中文档的正文片段：取自第一个带文字的命中片段（语义命中在前，其次正文关键词命中），空白合并、过长时截断
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snippet {
    /// 片段 ID，即命令行的“命中 ID”，`SegmentText` 据此取前后文
    pub segment_id: i64,
    pub text: String,
}

/// 语义检索（含以图搜图）不可用的原因
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticUnavailable {
    /// 文字模型（主模型）未下载
    NoModel,
    /// 以图搜图，但图片识别未开启（参数 `index_images`）
    ImagesOff,
    /// 以图搜图，但缺少多模态模型（mmproj）
    NoMmproj,
    /// 引导未完成、设备检测或模型下载进行中，推理引擎暂停
    Paused,
    /// 其他原因（引擎无法启动、推理出错）
    Engine,
    /// 搜索内容（文字或以图搜图的文字描述）超过模型的长度上限
    QueryTooLong,
}

/// 对应 `rummage_index::FileRow` 与 `IndexInfo`，只含界面需要的字段（详情栏）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    pub file_id: i64,
    pub path: String,
    pub name: String,
    /// 小写、不含点
    pub ext: String,
    pub kind: String,
    pub cloud: bool,
    pub offline: bool,
    pub size: i64,
    /// Unix 毫秒
    pub mtime: i64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// `files.status`：`pending`、`stored`、`cloud`、`too_large`
    pub status: String,
    /// 作业；为空表示这个文件不做语义索引（类型不支持、仅在云端、过大）
    pub job: Option<JobInfo>,
    /// 已写入的片段数
    pub segments: u32,
    /// 建好索引的时间（最近一次写入片段，Unix 毫秒）；没有片段时为空
    pub indexed_at: Option<i64>,
    /// 向量所在空间的预处理版本与后端
    pub spaces: Vec<VectorSpaceInfo>,
}

/// 对应 `rummage_index::JobRow`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobInfo {
    /// `pending`、`preprocessing`、`embedding`、`stored`、`failed`
    pub stage: String,
    pub attempts: u32,
    /// 给用户看的中文整句
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorSpaceInfo {
    /// 预处理参数版本，如 `text-v1`、`image-v1-t140`
    pub preprocess: String,
    /// 计算向量时的后端，如 `cpu`
    pub backend: String,
}

/// 对应 `rummage_index::FailedFile`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedFile {
    pub file_id: i64,
    pub name: String,
    pub path: String,
    pub kind: String,
    /// 给用户看的中文整句
    pub reason: Option<String>,
    /// 失败的时间（Unix 毫秒）
    pub failed_at: i64,
    /// 能否重试：加密文档不能
    pub retryable: bool,
}

/// 失败文件列表的分页游标：上一页最后一行的失败时间与文件 ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedCursor {
    pub failed_at: i64,
    pub file_id: i64,
}

/// 对应 `rummage_index::Root`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootInfo {
    pub id: i64,
    pub path: String,
    /// 所在设备未连接，索引保留
    pub offline: bool,
    /// 已记录的文件数
    pub files: i64,
    /// 用户排除规则（glob）
    pub excludes: Vec<String>,
    /// 还在统计文件数（添加后、启动对账或清空索引后首次扫完之前），`files` 不完整
    pub scanning: bool,
}

/// 对应 `rummage_index::ScanStats`
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanStats {
    pub added: u64,
    pub updated: u64,
    pub removed: u64,
    /// 本次遇到的网盘占位文件数
    pub cloud: u64,
    /// 本次遇到的超过大小上限的文件数
    pub too_large: u64,
    /// 本次排除的目录与文件数
    pub excluded: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexState {
    Idle,
    Running,
    /// 主模型未下载
    NoModel,
    /// 有图片、音频或视频作业待处理，但多模态模型文件（mmproj）缺失
    NoMmproj,
    /// 推理后端创建失败，原因见 `IndexStatus::error`
    Error,
}

/// 对应 `rummage_index::JobCounts`：按作业表统计，`pending` 含正在处理的
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexCounts {
    pub total: u64,
    pub done: u64,
    pub failed: u64,
    pub pending: u64,
}

/// 建索引暂停的原因。多个原因同时成立时只报一个，
/// 优先级：手动 > 睡眠 > 磁盘 > 额度 > 过热 > 电池 > 亮屏 > 全屏
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum PauseReason {
    /// 用户手动暂停；`until` 为 Unix 毫秒，为空时直到手动恢复
    Manual { until: Option<i64> },
    /// 系统即将睡眠，或刚唤醒、正在验证推理引擎
    Sleeping,
    /// 数据目录所在磁盘的剩余空间低于 `min_free_disk_gb`
    DiskLow { free_gb: f64 },
    /// Android 前台服务今日的后台运行额度已用完（`BackgroundQuota`），应用回到前台后解除
    Quota,
    /// Android 手机过热：热状态达到 `SEVERE` 或以上，或预测的温度余量达到 1.0
    Hot,
    /// 使用电池供电（未充电），且开启了 `pause_on_battery`
    Battery,
    /// Android 屏幕亮着，且开启了 `pause_when_screen_on`
    ScreenOn,
    /// 有全屏程序，且开启了 `pause_on_fullscreen`
    Fullscreen,
}

/// 索引进度，总是全量计数
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    pub state: IndexState,
    /// 暂停领取新作业的原因；为空表示没有暂停。暂停时处理中的作业照常完成
    pub paused: Option<PauseReason>,
    pub total: u64,
    pub done: u64,
    pub failed: u64,
    pub pending: u64,
    /// 键为文件种类（`image`、`document` 等）
    pub by_kind: BTreeMap<String, IndexCounts>,
    /// 推理使用的设备名称；未启动时为空
    pub backend: Option<String>,
    /// `state` 为 `error` 时的原因，中文整句
    pub error: Option<String>,
    /// 按最近的处理速度估算的剩余秒数；数据不足、暂停、出错或空闲时为空
    pub eta_s: Option<u64>,
    /// 按新设置重建的进度；没有要重建的文件时为空。要重建的文件在上面的计数中算作已完成（旧索引照常可搜）
    pub rebuild: Option<RebuildStatus>,
}

/// 按新设置重建的进度：`total` 为这一批的文件数，新的设置修改加入的文件计入同一批，全部完成后清零
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RebuildStatus {
    pub done: u64,
    pub total: u64,
    /// 还要重建的文件数，键为文件种类
    pub by_kind: BTreeMap<String, u64>,
}

/// 参数定义不在这里传输，各进程直接读本地的 `params::PARAMS`（握手已保证版本一致）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum Reply {
    Pong,
    Params {
        values: BTreeMap<String, ParamValue>,
    },
    Done,
    /// 已完成，但有需要告诉用户的情况（参数已保存、却没能安排重建索引），内容是中文整句
    DoneWithWarning {
        message: String,
    },
    Roots {
        roots: Vec<RootInfo>,
    },
    /// `AddRoot` 新建的文件夹；路径是规范化后的
    Root {
        root: RootInfo,
    },
    Index(IndexStatus),
    /// `total` 为失败文件的总数，`retryable` 为其中能重试的，`files` 为请求的这一页
    FailedFiles {
        files: Vec<FailedFile>,
        total: u64,
        retryable: u64,
    },
    Results {
        hits: Vec<SearchHit>,
        took_ms: u64,
        /// 索引尚未完成，结果可能不完整
        partial: bool,
        /// 推理引擎不可用（没有模型或出错），只有文件名结果
        semantic_unavailable: bool,
        /// `semantic_unavailable` 时的原因，界面据此说明；可用时为空
        unavailable: Option<SemanticUnavailable>,
    },
    File(FileInfo),
    /// 缩略图文件（WebP）的完整路径，位于数据目录的 `thumbs\` 下
    Thumbnail {
        path: String,
    },
    /// 片段已不存在时为空
    Segment {
        segment: Option<SegmentPreview>,
    },
    /// 装箱：引导状态比其他回复大得多，不让每条消息都按它分配
    Setup(Box<SetupStatus>),
    Proxy(ProxySetting),
    Estimate(Estimate),
    Appearance(Appearance),
    Notices {
        notices: Vec<Notice>,
    },
    /// 数据库可用时 `error` 为空；`rebuildable` 为清除后重建能解决的问题（损坏、从迁移备份恢复失败），
    /// 磁盘未连接、不可写、版本过新、被占用、升级失败等清除也无济于事的为否
    DbStatus {
        error: Option<String>,
        rebuildable: bool,
    },
    History {
        queries: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum Event {
    ParamsChanged {
        values: BTreeMap<String, ParamValue>,
    },
    /// 扫描进行中每处理 1000 个文件推送一次（`done` 为 false），结束（含离线、出错、取消）时推送一次 `done` 为 true
    ScanProgress {
        root_id: i64,
        done: bool,
        stats: ScanStats,
    },
    /// 索引进度，最多每 500 毫秒一次；带全量计数，漏掉几条不影响结果
    IndexProgress(IndexStatus),
    /// 模型下载或导入的进度，下载中最多每 250 毫秒一次，状态变化时立即推送
    ModelTask(ModelTask),
    /// 设备自检的进度与结果
    Selftest(SelftestStatus),
    /// 文件夹被添加、移除或改了排除规则（不论来自界面、命令行还是 MCP），已打开的设置页据此刷新
    RootsChanged,
}

/// 编码为一行 JSON，不含换行（JSON 字符串中的换行会被转义）
pub fn encode<T: Serialize>(msg: &T) -> String {
    serde_json::to_string(msg).expect("消息类型只含字符串键的映射和可序列化的值，序列化不会失败")
}

pub fn decode<T: DeserializeOwned>(line: &str) -> Result<T, Error> {
    Ok(serde_json::from_str(line)?)
}

#[cfg(test)]
mod tests;
