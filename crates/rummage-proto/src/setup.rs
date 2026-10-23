//! 首次使用引导用到的消息内容：引导状态、模型下载与导入、网络代理、设备自检和索引预估。
//! 请求、回复与事件的枚举项在 `lib.rs` 中，这里只放它们携带的类型。前端的对应类型在 `src/types.ts`。
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ParamValue;

/// 模型下载源
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ModelSource {
    /// huggingface.co
    Official,
    /// 魔搭 modelscope.cn
    #[serde(rename = "modelscope")]
    ModelScope,
    /// 用户填写的地址，接口与 Hugging Face 兼容
    Custom { url: String },
}

/// 网络代理模式
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    #[default]
    System,
    None,
    Custom,
}

/// 保存的代理设置；切换到其他模式时仍保留自定义地址，切回时不必重填
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxySetting {
    pub mode: ProxyMode,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTaskKind {
    Download,
    Import,
    /// 完整校验一个已安装的模型文件（大小与 SHA-256）
    Verify,
    /// 删除一个模型文件（主模型或 mmproj）
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTaskState {
    Running,
    Done,
    Failed,
    Cancelled,
}

/// 当前文件处于哪一步
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTaskPhase {
    /// 连接下载源、读取文件信息
    Preparing,
    /// 核对已有文件或续传前已下载的部分
    Checking,
    Downloading,
    /// 导入时边复制边计算哈希
    Copying,
}

/// 模型下载、导入、校验或删除任务的进度（同一时间只有一个任务）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelTask {
    pub kind: ModelTaskKind,
    pub state: ModelTaskState,
    pub phase: ModelTaskPhase,
    /// 当前处理的文件名；尚未开始时为空
    pub file: Option<String>,
    /// 当前文件是第几个（从 1 开始）与共几个
    pub index: u32,
    pub count: u32,
    /// 当前文件已处理与总共的字节数
    pub done: u64,
    pub total: u64,
    /// 下载速度（字节每秒）；不在下载时为空
    pub speed: Option<u64>,
    /// 失败原因或完成说明，中文整句
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceStatus {
    /// 等待测试
    Pending,
    Testing,
    Passed,
    Failed,
    /// 此前出现过驱动错误，驱动或运行库更新前不再测试
    Blacklisted,
    /// 未检测：另一块显卡出现驱动错误、本次运行中停用、解除屏蔽后尚未重新检测，或解除屏蔽后的重测中照缓存显示的其余设备没有结果
    Skipped,
}

/// 首选设备（`SelftestStatus::preferred`）：按检测结果自动选择
pub const AUTO_DEVICE: &str = "auto";
/// 首选设备：只用 CPU；也是 CPU 的设备标识（`DeviceResult::id`）
pub const CPU_DEVICE: &str = "cpu";

/// 一个候选设备的自检结果，对应 `rummage_embed::selftest::CandidateReport`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceResult {
    /// 设备标识（`rummage_embed::selftest::device_id`）：CPU 为 `cpu`，显卡由描述与 PCI 号组成，同型号的两块卡也不同
    pub id: String,
    pub gpu: bool,
    /// 显卡的 PCI 号，同型号的显卡据此区分；CPU 与不报 PCI 号的核显为空
    pub pci_id: Option<String>,
    /// ggml 设备描述，CPU 为处理器名称
    pub name: String,
    pub status: DeviceStatus,
    /// 未通过或被跳过的原因，中文整句
    pub reason: Option<String>,
    /// 黄金样本单条耗时的中位数（毫秒）
    pub text_ms: Option<f64>,
    pub image_ms: Option<f64>,
    pub audio_ms: Option<f64>,
    pub video_ms: Option<f64>,
    /// 按测速时的参数处理单个文件（单段）的耗时；没有测速结果（旧的检测结果）时为空
    pub speed: Option<DeviceSpeed>,
    /// 显卡现在的情况；CPU 与检测中为空
    pub state: Option<GpuState>,
}

/// 按当时的参数测得的耗时（毫秒），对应 `rummage_embed::selftest::CandidateReport::speed`：
/// 一段 `chunk_tokens` 长的文本、一张图片、一段音频、一个视频片段；没测的为空
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSpeed {
    pub text_ms: Option<f64>,
    pub image_ms: Option<f64>,
    pub audio_ms: Option<f64>,
    pub video_ms: Option<f64>,
    /// 测速时影响耗时的参数值；之后改了其中任何一项，界面提示这些耗时是修改前的
    pub params: BTreeMap<String, ParamValue>,
}

/// 显卡现在的情况。已屏蔽的显卡一般只在 `SelftestStatus::blocked` 中列出；本次测过且未通过时例外，标为 `Blocked`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuState {
    /// 没有下面几种情况
    Usable,
    /// 本次运行中停用（出过错，或所有显卡都停用），重新启动后照常检测与使用
    Disabled,
    /// 最近一次检测时在、现在不在系统中（如外接显卡已拔出）
    Absent,
    /// 已屏蔽（只在本次测过它且未通过时出现在检测结果中，同时列在 `SelftestStatus.blocked`）
    Blocked,
}

/// 被屏蔽的显卡，对应 `rummage_embed::selftest::BlockedGpu`：同一块显卡的多条屏蔽记录合为一项
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedGpu {
    /// 设备标识，与 `DeviceResult::id` 相同；解除屏蔽时传回
    pub id: String,
    /// 显卡的描述
    pub name: String,
    pub pci_id: Option<String>,
    /// 核心记下的屏蔽原因，中文整句；没有时为空
    pub reason: Option<String>,
}

/// 设备自检的状态
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelftestStatus {
    pub running: bool,
    /// 用户选择的设备：`auto`（按检测结果）、`cpu` 或某块显卡的 `DeviceResult::id`
    pub preferred: String,
    /// 选择的显卡目前不能使用（没有通过最近一次检测、已被屏蔽、本次运行中停用或已拔出），已按自动选择时的原因，中文整句
    pub preferred_error: Option<String>,
    /// 最近一次检测的结果；检测中为本次的进度。没有检测过时为空；不含已屏蔽的显卡（见 `blocked`）
    pub devices: Vec<DeviceResult>,
    /// 实际使用的设备在 `devices` 中的位置（已按用户的选择）
    pub selected: Option<usize>,
    /// 检测本身无法进行的原因（模型缺失、黄金样本无效等），中文整句
    pub error: Option<String>,
    /// 结果已过期（显卡有增减或更换、运行库或模型变化、需要图片与音视频模型而上次没测），核心会在后台自动重新检测
    pub stale: bool,
    /// 最近一次检测是核心在结果过期时自动发起的（检测中时界面据此说明正在用 CPU 建立索引）
    pub auto: bool,
    /// 本次运行中最近出了问题而停用、已改用别的设备时，出问题的设备的标识（`DeviceResult::id`）；它已屏蔽时
    /// 只在 `blocked` 中，否则在 `devices` 中。`selected` 为改用的设备
    pub failed: Option<String>,
    /// 本次运行中显卡出现可能与驱动有关的问题，所有显卡都停用、只用 CPU（重新启动后再按检测结果选）
    pub gpus_off: bool,
    /// 被屏蔽的显卡，可以在设备页解除屏蔽（`Request::UnblockGpu`）
    pub blocked: Vec<BlockedGpu>,
}

/// 一个模型文件：钉定的文件名与大小（`rummage_embed::space`），以及本地文件是否可用（核对大小）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFileStatus {
    /// 主文件名；与它一起下载的文件（分词器、QNN 运行库）不单独列出
    pub name: String,
    /// 全部文件的大小
    pub bytes: u64,
    /// 全部文件都可用
    pub ready: bool,
    /// 已下载了一部分的 `.part` 文件的大小，没有时为 0
    pub partial_bytes: u64,
    /// 上次完整校验（下载、导入或校验）的结果；没有记录、记录与文件现在的大小或修改时间不符，或文件不可用时为空
    pub verified: Option<bool>,
    /// 钉定版本的主文件页面，用户自行下载时打开
    pub url: String,
}

/// 引导需要的全部状态
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupStatus {
    /// 已完成引导，不再显示
    pub onboarded: bool,
    /// 引导进行到第几步（从 0 起），重启后从这一步继续；没有记录时为 0
    pub step: u32,
    /// 主模型
    pub model: ModelFileStatus,
    /// 多模态模型（mmproj）；Android 上与 `model` 相同（LiteRT 通用版含全部模态）
    pub mmproj: ModelFileStatus,
    /// Android：本机芯片的 NPU 模型；芯片没有 NPU 版或不是 Android 时为空
    pub npu: Option<ModelFileStatus>,
    /// Android：NPU 模型对应的芯片，如“高通 SM8550”
    pub npu_chip: Option<String>,
    /// Android：分词器 `tokenizer.json` 的下载页（与模型不在同一个仓库），手动导入时也要它；电脑上为空
    pub tokenizer_url: Option<String>,
    /// 最近一次模型任务；本次运行没有任务时为空
    pub task: Option<ModelTask>,
    pub selftest: SelftestStatus,
    pub proxy: ProxySetting,
}

/// 某类文件的数量与总大小
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KindTotal {
    pub files: u64,
    pub bytes: u64,
    /// 图片：长图切片的总段数，由核心按抽样推算；其他种类为 0
    #[serde(default)]
    pub slices: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootEstimate {
    pub id: i64,
    pub path: String,
    /// 已记录的全部文件数（都能按文件名搜索）
    pub files: u64,
    /// 能识别内容的文件（不含网盘占位文件与超过大小上限的），键为 `document`、`image`、`audio`、`video`
    pub kinds: BTreeMap<String, KindTotal>,
    /// 还在统计文件数，`files` 与 `kinds` 不完整（同 `RootInfo::scanning`）
    pub scanning: bool,
}

/// 索引预估的原始数据，时间与空间由界面按自检速度换算
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Estimate {
    /// 还有文件夹在扫描，数量会继续增加
    pub scanning: bool,
    pub roots: Vec<RootEstimate>,
    /// 数据目录所在磁盘对当前用户可用的空间（GB），界面据此提示空间可能不够；查询失败时为空
    pub free_gb: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode, encode};

    fn round_trip<T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(
        msg: T,
    ) {
        assert_eq!(decode::<T>(&encode(&msg)).unwrap(), msg);
    }

    #[test]
    fn setup_round_trip() {
        round_trip(SetupStatus {
            onboarded: false,
            step: 3,
            model: ModelFileStatus {
                name: "a.gguf".into(),
                bytes: 3,
                ready: true,
                partial_bytes: 0,
                verified: Some(true),
                url: "https://huggingface.co/a/blob/b/a.gguf".into(),
            },
            mmproj: ModelFileStatus {
                name: "b.gguf".into(),
                bytes: 4,
                ready: false,
                partial_bytes: 2,
                verified: None,
                url: "https://huggingface.co/a/blob/b/b.gguf".into(),
            },
            npu: Some(ModelFileStatus {
                name: "c.litertlm".into(),
                bytes: 5,
                ready: false,
                partial_bytes: 0,
                verified: None,
                url: "https://huggingface.co/c/blob/d/c.litertlm".into(),
            }),
            npu_chip: Some("高通 SM8550".into()),
            tokenizer_url: Some("https://huggingface.co/e/blob/f/tokenizer.json".into()),
            task: Some(ModelTask {
                kind: ModelTaskKind::Download,
                state: ModelTaskState::Running,
                phase: ModelTaskPhase::Downloading,
                file: Some("a.gguf".into()),
                index: 1,
                count: 2,
                done: 10,
                total: 20,
                speed: Some(5),
                message: None,
            }),
            selftest: SelftestStatus {
                running: true,
                preferred: AUTO_DEVICE.into(),
                preferred_error: None,
                devices: vec![DeviceResult {
                    id: "gpu:RTX|0000:01:00.0".into(),
                    gpu: true,
                    pci_id: Some("0000:01:00.0".into()),
                    name: "RTX".into(),
                    status: DeviceStatus::Testing,
                    reason: None,
                    text_ms: Some(7.0),
                    image_ms: None,
                    audio_ms: Some(48.0),
                    video_ms: None,
                    speed: Some(DeviceSpeed {
                        text_ms: Some(30.0),
                        image_ms: Some(1500.5),
                        audio_ms: None,
                        video_ms: None,
                        params: BTreeMap::from([
                            ("chunk_tokens".to_string(), ParamValue::Int(384)),
                            ("selftest_min_cosine".to_string(), ParamValue::Float(0.99)),
                        ]),
                    }),
                    state: Some(GpuState::Absent),
                }],
                selected: None,
                error: None,
                stale: true,
                auto: true,
                failed: Some("gpu:RTX|0000:01:00.0".into()),
                gpus_off: true,
                blocked: vec![BlockedGpu {
                    id: "gpu:RTX|0000:01:00.0".into(),
                    name: "RTX".into(),
                    pci_id: Some("0000:01:00.0".into()),
                    reason: None,
                }],
            },
            proxy: ProxySetting {
                mode: ProxyMode::Custom,
                url: "http://127.0.0.1:7890".into(),
            },
        });
        round_trip(Estimate {
            scanning: true,
            roots: vec![RootEstimate {
                id: 1,
                path: r"D:\资料".into(),
                files: 3,
                kinds: BTreeMap::from([(
                    "image".to_string(),
                    KindTotal {
                        files: 2,
                        bytes: 9,
                        slices: 3,
                    },
                )]),
                scanning: true,
            }],
            free_gb: Some(12.5),
        });
        for s in [
            ModelSource::Official,
            ModelSource::ModelScope,
            ModelSource::Custom {
                url: "https://x".into(),
            },
        ] {
            round_trip(s);
        }
    }

    #[test]
    fn wire_format() {
        assert_eq!(encode(&ModelSource::ModelScope), r#"{"type":"modelscope"}"#);
        assert_eq!(
            encode(&ModelSource::Custom { url: "u".into() }),
            r#"{"type":"custom","url":"u"}"#
        );
        assert_eq!(encode(&ProxyMode::None), r#""none""#);
        assert_eq!(encode(&DeviceStatus::Blacklisted), r#""blacklisted""#);
        assert_eq!(encode(&GpuState::Absent), r#""absent""#);
        assert_eq!(encode(&ModelTaskKind::Verify), r#""verify""#);
        let line = encode(&KindTotal {
            files: 1,
            bytes: 2,
            slices: 0,
        });
        assert_eq!(line, r#"{"files":1,"bytes":2,"slices":0}"#);
    }
}
