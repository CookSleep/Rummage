// 约定：界面进程在 Tauri 命令里拆开核心进程回复的 Result 后再返回，前端不直接接触 IPC 消息，所以这里不定义消息类型。

/** 参数值，对应 Rust `rummage_proto::ParamValue`（untagged） */
export type ParamValue = boolean | number

/** 参数当前值，对应 Rust `BTreeMap<String, ParamValue>` */
export type ParamValues = Record<string, ParamValue>

/** 参数分组，对应 Rust `rummage_proto::Group` */
export type ParamGroup = 'Search' | 'Index' | 'Resource' | 'Advanced'

/** 参数定义，对应 Rust `rummage_proto::ParamDef`；`kind` 对应 `ParamKind` */
export interface ParamDef {
  key: string
  group: ParamGroup
  /** 设置页中的小标题，同组同小标题的参数相邻；为空时不在参数列表中显示 */
  section: string
  kind:
    | 'Bool'
    | { Int: { min: number; max: number } }
    | { Float: { min: number; max: number } }
    /** 只能取列出的值之一，界面显示各值的名称（如视频预设）；`unit` 不为空时名称后写出数值，如“标准（140 Token）” */
    | { Named: { options: [number, string][]; unit: string } }
  /** 已按运行环境求得的默认值 */
  default: ParamValue
  affectsIndex: boolean
  /** `affectsIndex` 为真时受影响的文件种类（`image`、`document`、`audio`、`video`），否则为空 */
  indexKinds: string[]
  label: string
  help: string
  basis: string
  /** 只在此平台上显示（`android`、`windows`）；为空时各平台都显示 */
  platform: string
}

/** 核心进程的连接状态，对应 Rust `src-tauri/src/core.rs` 的 `Status` */
export type CoreStatus = 'starting' | 'running' | 'restarting' | 'failed'

/** 核心进程返回的错误，对应 Rust `rummage_proto::ApiError` */
export interface ApiError {
  code: string
  /** 给用户看的中文整句 */
  message: string
}

/** 对应 Rust `rummage_proto::SegmentHit` */
export interface SegmentHit {
  segmentId: number
  /** 页码、章节、行号等。文本片段为 `{ char_start, char_end, line_start, line_end, heading }`；经 Xberg 提取的文档没有行号，PDF 另有 `page_start` / `page_end`，演示文稿另有 `slide_start` / `slide_end`。图片的整图片段为 `{}`；长图切片为 `{ region: { x, y, width, height } }`，原图（已按 EXIF 方向转正）中的像素区域。音频段与视频的命中时间段为 `{ start_ms, end_ms }`，视频为相邻命中窗口合并后的一段 */
  locatorJson: string
  cosine: number
}

/** 对应 Rust `rummage_proto::SearchHit` */
export interface SearchHit {
  fileId: number
  path: string
  name: string
  kind: string
  size: number
  /** Unix 毫秒 */
  mtime: number
  /** 网盘占位文件 */
  cloud: boolean
  /** 所在文件夹不可访问（`missing` 或设备未连接） */
  offline: boolean
  /** 所在文件夹不存在，它的磁盘或存储已连接 */
  missing: boolean
  score: number
  /** 最高余弦；没有语义命中时为空 */
  cosine: number | null
  /** 校准后的最高分数（z 分数）；没有语义命中或还没有校准时为空 */
  z: number | null
  /** 匹配度百分比（0–100），由核心按 z 换算；z 为空时为空 */
  matchPct: number | null
  /** 匹配度较低，排在其余结果之后，默认折叠 */
  lowRelevance: boolean
  /** 按校准后的分数（没有时按余弦）从高到低 */
  segments: SegmentHit[]
  nameHit: boolean
  /** 正文关键词命中的片段 id，最匹配的在前；为空表示正文没有命中 */
  textSegments: number[]
  /** 图片、视频的宽高，未记录时为空 */
  width: number | null
  height: number | null
  /** 文档的正文片段，只在命令行的搜索中提供，界面中总为空 */
  snippet: Snippet | null
}

/** 对应 Rust `rummage_proto::Snippet` */
export interface Snippet {
  segmentId: number
  text: string
}

/** 文档预览中的一个片段，对应 Rust `rummage_proto::SegmentPreview`：命中片段的正文与前后各一段不重叠的上下文 */
export interface SegmentPreview {
  fileId: number
  /** 写法同 `SegmentHit.locatorJson` */
  locatorJson: string
  text: string | null
  before: string | null
  after: string | null
}

/** 打开到命中位置，对应 Rust `src-tauri/src/open_at.rs` 的 `OpenAt`：音视频为起点毫秒，PDF 为页码 */
export interface OpenAt {
  ms?: number
  page?: number
}

/** 对应 Rust `src-tauri/src/search.rs` 的 `Results` */
export interface SearchResults {
  hits: SearchHit[]
  tookMs: number
  /** 索引尚未完成，结果可能不完整 */
  partial: boolean
  /** 推理引擎不可用，只有文件名结果 */
  semanticUnavailable: boolean
  /** 不可用的原因 */
  unavailable: SemanticUnavailable | null
}

/** 语义检索不可用的原因，对应 Rust `rummage_proto::SemanticUnavailable` */
export type SemanticUnavailable = 'no_model' | 'images_off' | 'no_mmproj' | 'paused' | 'engine' | 'query_too_long'

/** 对应 Rust `rummage_proto::IndexState` */
export type IndexState = 'idle' | 'running' | 'no_model' | 'no_mmproj' | 'error'

/** 对应 Rust `rummage_proto::PauseReason` */
export type PauseReason =
  | { type: 'manual'; until: number | null }
  | { type: 'sleeping' }
  | { type: 'disk_low'; freeGb: number }
  | { type: 'quota' }
  | { type: 'hot' }
  | { type: 'battery' }
  | { type: 'screen_on' }
  | { type: 'fullscreen' }

/** 手动暂停建索引的时长，对应 Rust `src-tauri/src/pause.rs` 的 `PauseFor` */
export type PauseFor = 'hour' | 'tomorrow' | 'manual'

/** 对应 Rust `rummage_proto::IndexCounts` */
export interface IndexCounts {
  total: number
  done: number
  failed: number
  pending: number
}

/** 对应 Rust `rummage_proto::IndexStatus` */
export interface IndexStatus extends IndexCounts {
  state: IndexState
  paused: PauseReason | null
  byKind: Record<string, IndexCounts>
  backend: string | null
  /** `state` 为 `error` 时的原因；为 `no_model` 时是只有 NPU 模型、NPU 却用不了的说明，缺少模型时为空 */
  error: string | null
  /** 按最近的处理速度估算的剩余秒数；数据不足、暂停、出错或空闲时为空 */
  etaS: number | null
  /** 按新设置重建的进度；没有要重建的文件时为空。要重建的文件在上面的计数中算作已完成 */
  rebuild: RebuildStatus | null
}

/** 建立索引失败的文件，对应 Rust `rummage_proto::FailedFile` */
export interface FailedFile {
  fileId: number
  name: string
  path: string
  kind: string
  /** 给用户看的中文整句 */
  reason: string | null
  /** 失败的时间（Unix 毫秒），与 `fileId` 一起作为分页游标 */
  failedAt: number
  /** 能否重试：加密文档不能 */
  retryable: boolean
}

/** 失败文件列表的分页游标：上一页最后一行，对应 Rust `rummage_proto::FailedCursor` */
export interface FailedCursor {
  failedAt: number
  fileId: number
}

/** 失败文件的一页，对应 Rust `src-tauri/src/failures.rs` 的 `FailedPage` */
export interface FailedPage {
  files: FailedFile[]
  total: number
  /** 其中能重试的 */
  retryable: number
}

/** 对应 Rust `rummage_proto::RebuildStatus` */
export interface RebuildStatus {
  done: number
  total: number
  /** 还要重建的文件数，键为文件种类 */
  byKind: Record<string, number>
}

/** Tauri 事件 `core-event` 的内容，对应 Rust `rummage_proto::Event`（内部带 `type` 标签），只列出界面用到的 */
export type CoreEvent =
  | ({ type: 'IndexProgress' } & IndexStatus)
  | ({ type: 'ModelTask' } & ModelTask)
  | ({ type: 'Selftest' } & SelftestStatus)
  | { type: 'ScanProgress'; rootId: number; done: boolean }
  | { type: 'ParamsChanged'; values: ParamValues }
  /** 文件夹被添加、移除或改了排除规则，可能来自命令行或 MCP */
  | { type: 'RootsChanged' }

/** “找相似的”：用索引中这个片段已存的向量搜索，对应 Rust `rummage_proto::SimilarTo` */
export interface SimilarTo {
  fileId: number
  /** 为空时用该文件第一个有向量的片段 */
  segmentId: number | null
}

/** 搜索框中的“相似：<文件名>”标记（前端专用） */
export interface SimilarQuery extends SimilarTo {
  name: string
  /** 文件种类，决定找相似图标的样式 */
  kind: string
  /** 普通图片（不是长图切片）：可以加文字描述，由核心重新读取这张图片与文字合成一条查询 */
  describable: boolean
}

/** 筛选条件，对应 Rust `src-tauri/src/search.rs` 的 `SearchFilter`；为空的列表表示不限 */
export interface SearchFilter {
  kinds: string[]
  rootIds: number[]
  /** 修改时间的起止（Unix 毫秒，含两端） */
  since: number | null
  until: number | null
}

/** 以图搜图的查询图片，对应 Rust `src-tauri/src/image_query.rs` 的 `ImageQuery`：路径留在 Rust 侧，前端只拿令牌 */
export interface ImageQuery {
  token: number
  name: string
}

/** 对应 Rust `rummage_proto::JobInfo` */
export interface JobInfo {
  stage: 'pending' | 'preprocessing' | 'embedding' | 'stored' | 'failed'
  attempts: number
  /** 给用户看的中文整句 */
  lastError: string | null
}

/** 对应 Rust `rummage_proto::VectorSpaceInfo` */
export interface VectorSpaceInfo {
  preprocess: string
  backend: string
}

/** 详情栏的文件信息与索引情况，对应 Rust `rummage_proto::FileInfo` */
export interface FileInfo {
  fileId: number
  path: string
  name: string
  ext: string
  kind: string
  cloud: boolean
  /** 所在文件夹不可访问（`missing` 或设备未连接） */
  offline: boolean
  /** 所在文件夹不存在，它的磁盘或存储已连接 */
  missing: boolean
  size: number
  /** Unix 毫秒 */
  mtime: number
  width: number | null
  height: number | null
  /** `pending`、`stored`、`cloud`、`too_large` */
  status: string
  /** 为空表示不做语义索引 */
  job: JobInfo | null
  segments: number
  /** 建好索引的时间（Unix 毫秒），没有片段时为空 */
  indexedAt: number | null
  spaces: VectorSpaceInfo[]
}

/** 对应 Rust `rummage_proto::RootInfo` */
export interface RootInfo {
  id: number
  path: string
  /** 不可访问（`missing` 或设备未连接） */
  offline: boolean
  /** 文件夹不存在（被删除、改名或移走），它的磁盘或存储已连接 */
  missing: boolean
  files: number
  /** 用户排除规则（glob） */
  excludes: string[]
  /** 还在统计文件数，`files` 不完整 */
  scanning: boolean
}

/** 模型下载源，对应 Rust `rummage_proto::ModelSource` */
export type ModelSource = { type: 'official' } | { type: 'mirror' } | { type: 'custom'; url: string }

/** 对应 Rust `rummage_proto::ProxyMode` */
export type ProxyMode = 'system' | 'none' | 'custom'

/** 对应 Rust `rummage_proto::ProxySetting` */
export interface ProxySetting {
  mode: ProxyMode
  url: string
}

/** 模型下载、导入、校验或删除的进度，对应 Rust `rummage_proto::ModelTask` */
export interface ModelTask {
  kind: 'download' | 'import' | 'verify' | 'delete'
  state: 'running' | 'done' | 'failed' | 'cancelled'
  phase: 'preparing' | 'checking' | 'downloading' | 'copying'
  file: string | null
  /** 第几个文件（从 1 开始）与共几个 */
  index: number
  count: number
  /** 当前文件已处理与总共的字节数 */
  done: number
  total: number
  /** 字节每秒 */
  speed: number | null
  /** 失败原因或完成说明 */
  message: string | null
}

/** 对应 Rust `rummage_proto::DeviceStatus` */
export type DeviceStatus = 'pending' | 'testing' | 'passed' | 'failed' | 'blacklisted' | 'skipped'

/** 一个设备的自检结果，对应 Rust `rummage_proto::DeviceResult` */
export interface DeviceResult {
  /** 设备标识：CPU 为 `cpu`，显卡由描述与 PCI 号组成（同型号的两块卡也不同） */
  id: string
  gpu: boolean
  /** 显卡的 PCI 号，同型号的显卡据此区分；CPU 与不报 PCI 号的核显为空 */
  pciId: string | null
  name: string
  status: DeviceStatus
  reason: string | null
  /** 黄金样本单条耗时的中位数（毫秒） */
  textMs: number | null
  imageMs: number | null
  audioMs: number | null
  /** 一个视频片段（黄金样本的视频窗口，带声音与只看画面两种的中位数） */
  videoMs: number | null
  /** 按测速时的参数处理单个文件（单段）的耗时；没有测速结果（旧的检测结果）时为空 */
  speed: DeviceSpeed | null
  /** 显卡现在的情况；CPU 与检测中为空 */
  state: GpuState | null
}

/** 按当时的参数测得的耗时（毫秒）：一段文本、一张图片、一段音频、一个视频片段，没测的为空；对应 Rust `rummage_proto::DeviceSpeed` */
export interface DeviceSpeed {
  textMs: number | null
  imageMs: number | null
  audioMs: number | null
  videoMs: number | null
  /** 测速时影响耗时的参数值；之后改了其中任何一项，耗时就是修改前的 */
  params: ParamValues
}

/** 显卡现在的情况（可用、本次运行停用、不在系统中、已屏蔽），对应 Rust `rummage_proto::GpuState`；已屏蔽的显卡一般不在检测结果中（见 `SelftestStatus.blocked`），本次测过且未通过时例外 */
export type GpuState = 'usable' | 'disabled' | 'absent' | 'blocked'

/** 被屏蔽的显卡，对应 Rust `rummage_proto::BlockedGpu` */
export interface BlockedGpu {
  /** 设备标识，与 `DeviceResult.id` 相同 */
  id: string
  name: string
  pciId: string | null
  /** 后台服务记下的屏蔽原因；没有时为空 */
  reason: string | null
}

/** 对应 Rust `rummage_proto::SelftestStatus` */
export interface SelftestStatus {
  running: boolean
  /** 用户选择的设备：`auto`（按检测结果）、`cpu` 或某块显卡的 `DeviceResult.id` */
  preferred: string
  /** 选择的显卡目前不能使用、已按自动选择时的原因 */
  preferredError: string | null
  /** 最近一次检测的结果；不含已屏蔽的显卡 */
  devices: DeviceResult[]
  /** 实际使用的设备（已按用户的选择） */
  selected: number | null
  error: string | null
  /** 结果已过期（显卡有增减或更换、运行库或模型变化、所需模态没测过），后台服务会自动重新检测 */
  stale: boolean
  /** 最近一次检测是后台服务在结果过期时自动发起的 */
  auto: boolean
  /** 本次运行中最近出了问题而停用、已改用别的设备时，出问题的设备的标识；它已屏蔽时只在 `blocked` 中，否则在 `devices` 中。`selected` 为改用的设备 */
  failed: string | null
  /** 本次运行中显卡出现可能与驱动有关的问题，所有显卡都停用、只用 CPU */
  gpusOff: boolean
  /** 被屏蔽的显卡，可在设备页解除屏蔽 */
  blocked: BlockedGpu[]
}

/** 一个模型文件，对应 Rust `rummage_proto::ModelFileStatus` */
export interface ModelFileStatus {
  /** 主文件名；与它一起下载的文件（分词器、QNN 运行库）不单独列出 */
  name: string
  /** 全部文件的大小 */
  bytes: number
  /** 全部文件都可用 */
  ready: boolean
  /** 已下载了一部分的 .part 文件的大小，没有时为 0 */
  partialBytes: number
  /** 上次完整校验的结果；没有记录、记录与文件现在的大小或修改时间不符，或文件不可用时为空 */
  verified: boolean | null
  /** 钉定版本的主文件页面，用户自行下载时打开 */
  url: string
}

/** 引导状态，对应 Rust `rummage_proto::SetupStatus` */
export interface SetupStatus {
  onboarded: boolean
  /** 引导进行到第几步（从 0 起），重启后从这一步继续 */
  step: number
  /** 主模型 */
  model: ModelFileStatus
  /** 多模态模型；Android 上与 `model` 相同（LiteRT 通用模型含全部模态） */
  mmproj: ModelFileStatus
  /** Android：本机芯片的 NPU 模型；芯片没有 NPU 版或不是 Android 时为空 */
  npu: ModelFileStatus | null
  /** Android：NPU 模型对应的芯片，如“高通 SM8550” */
  npuChip: string | null
  /** Android：分词器 tokenizer.json 的下载页（与模型不在同一个仓库），手动导入时也要它；电脑上为空 */
  tokenizerUrl: string | null
  task: ModelTask | null
  selftest: SelftestStatus
  proxy: ProxySetting
}

/** 对应 Rust `rummage_proto::KindTotal` */
export interface KindTotal {
  files: number
  bytes: number
  /** 图片：长图切片的总段数，由核心按抽样推算；其他种类为 0 */
  slices: number
}

/** 刚添加的文件夹，对应 Rust `src-tauri` 的 `setup::AddedRoot` */
export interface AddedRoot {
  id: number
  path: string
  /** 整个磁盘或存储 */
  wholeDrive: boolean
}

/** 对应 Rust `rummage_proto::RootEstimate` */
export interface RootEstimate {
  id: number
  path: string
  files: number
  /** 键为 `document`、`image`、`audio`、`video` */
  kinds: Record<string, KindTotal>
  /** 还在统计文件数，`files` 与 `kinds` 不完整 */
  scanning: boolean
}

/** 对应 Rust `rummage_proto::Estimate` */
export interface Estimate {
  scanning: boolean
  roots: RootEstimate[]
  /** 数据目录所在磁盘的剩余空间（GB）；查询失败时为空 */
  freeGb: number | null
}

/** 字体来源，对应 Rust `rummage_proto::FontMode` */
export type FontMode = 'builtin' | 'system' | 'custom'

/** 外观设置，对应 Rust `rummage_proto::Appearance`；字体名只在 `custom` 时使用 */
export interface Appearance {
  uiFont: FontMode
  uiFontName: string
  monoFont: FontMode
  monoFontName: string
}

/** 设置页顶部的通知，对应 Rust `rummage_proto::Notice` */
export type Notice = 'db_restored'

/** 数据目录及各部分的占用（字节），对应 Rust `src-tauri/src/data.rs` 的 `StorageInfo` */
export interface StorageInfo {
  dataDir: string
  /** 索引数据库，含 WAL 与迁移备份 */
  db: number
  thumbs: number
  models: number
  logs: number
  other: number
}

/** “Agent”页的 MCP 客户端，对应 Rust `src-tauri/src/agent.rs` 的 `Client` */
export type AgentClient = 'claudeCode' | 'codex' | 'claudeDesktop' | 'cursor' | 'vscode' | 'opencode'

/** Skill 的安装位置，对应 Rust `SkillTarget`：`~/.claude/skills` 或 `~/.agents/skills` */
export type SkillTarget = 'claude' | 'agents'

/**
 * 对应 Rust `State`：没有检测到客户端、未添加、已添加、已添加但命令路径或 Skill 内容与当前不同、
 * 配置文件格式有误或同名文件夹不是 Rummage 的 Skill
 */
export type AgentState = 'notFound' | 'absent' | 'added' | 'outdated' | 'invalid'

/** “Agent”页的状态，对应 Rust `AgentInfo` */
export interface AgentInfo {
  /** MCP 服务的可执行文件 `rummage-core.exe` */
  coreExe: string
  /** 随安装包附带的 Skill 文件夹 */
  skillDir: string
  /** `path` 为该客户端的配置文件 */
  clients: { id: AgentClient; path: string; state: AgentState }[]
  /** `path` 为安装后的 Skill 文件夹 */
  skills: { id: SkillTarget; path: string; state: AgentState }[]
}

/** 选好的新数据目录，对应 Rust `src-tauri/src/data.rs` 的 `DataDirChange` */
export interface DataDirChange {
  from: string
  to: string
  /** 当前数据的文件数与大小 */
  files: number
  bytes: number
  /** 不能使用所选目录的原因 */
  blocked: string | null
}

/** 启动时确定数据目录的结果，对应 Rust `src-tauri/src/data.rs` 的 `StartupNotice` */
export interface StartupNotice {
  success: boolean
  title: string
  message: string
  /** [标签, 路径] */
  paths: [string, string][]
  details: string[]
}

/** 索引数据库能否打开，对应 Rust `src-tauri/src/data.rs` 的 `DbStatus` */
export interface DbStatus {
  error: string | null
  /** 清除后重建能解决 */
  rebuildable: boolean
}

/** 配置文件中的文件夹，对应 Rust `rummage_proto::config::ConfigFolder` */
export interface ConfigFolder {
  path: string
  excludes: string[]
}

/** 导出的配置文件，对应 Rust `rummage_proto::config::ConfigFile`；由 Rust 侧读写，字段名与文件中一致 */
export interface ConfigFile {
  format: string
  version: number
  platform: 'windows' | 'android'
  app_version: string
  /** Unix 毫秒 */
  exported_at: number
  folders: ConfigFolder[]
  params: ParamValues
}

/** 导入前的预览，对应 Rust `src-tauri/src/config.rs` 的 `Preview` */
export interface ConfigPreview {
  /** 会改变的参数 */
  params: ParamValues
}

/** 导入的结果，对应 Rust `src-tauri/src/config.rs` 的 `Outcome` */
export interface ConfigImport {
  added: number
  /** 不存在而丢弃的文件夹 */
  missing: number
  /** 无法访问而丢弃的文件夹 */
  inaccessible: number
  /** 后台服务拒绝添加的文件夹 */
  failed: number
  /** 丢弃的参数 */
  droppedParams: number
  /** 参数已保存、但需要告诉用户的情况 */
  warning: string | null
}
