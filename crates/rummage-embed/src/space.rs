//! 向量空间 ID：哪些向量可以直接比较。
//!
//! 输入为模型家族、所用模型文件的 SHA-256、运行库、后端、精度、前缀方案与该模态的预处理参数版本，
//! 按固定顺序拼成“键=值”逐行的规范字符串，取 SHA-256 的前 16 位十六进制。
//! - 文本向量只经过主模型，只取主模型的哈希；图像向量取主模型加 mmproj 的哈希，所以只换 mmproj 时文本空间不变。
//! - 模型文件的哈希用钉定版本的已知值，启动时只核对文件大小，不重新计算 800 MB 的哈希；
//!   完整哈希由模型下载与导入保证（下载、导入时逐个校验 SHA-256）。
//! - 修改预处理逻辑时提升对应模态的预处理参数版本，使旧向量被识别为需要重建。

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Error;

pub const FAMILY: &str = "embeddinggemma-2";
/// 官方预编译包，未打补丁。
pub const RUNTIME: &str = "llama.cpp b11461 官方预编译";
/// 记的是 CPU 参考配置（与黄金向量相同）。GPU 打开 Flash Attention，偏差与 Vulkan 默认精度同属后端差异，
/// 由自检兜底、按兼容空间处理，不计入空间 ID；改这个字符串会让已有索引全部重建。
pub const PRECISION: &str = "q8_0, flash_attn=off";
pub const PREFIX_SCHEME: &str = "A";
pub const TEXT_PREPROCESS: &str = "text-v1";

/// 文本向量的参数版本（`vectors.params_version`）：文本空间的预处理版本加上标题截断长度（`prefix::MAX_TITLE_CHARS`）、
/// 分段长度、重叠比例与最短分段长度，如 `text-v1-t64-c384-o15-m64`。
/// 这些参数只决定文档向量的输入（标题保留多少、每个向量覆盖哪段文字），不改变向量与查询的可比性；文本查询只与
/// 预处理版本相同的文本空间比较，所以它们不计入空间（与音频的切段参数不同），只用来判断“设置已改、索引未重建”
pub fn text_version(chunk_tokens: usize, overlap_pct: usize, min_tokens: usize) -> String {
    format!(
        "{TEXT_PREPROCESS}-t{}-c{chunk_tokens}-o{overlap_pct}-m{min_tokens}",
        crate::prefix::MAX_TITLE_CHARS
    )
}
/// 图像预处理参数版本，空间 ID 中再加上档位（`image-v1-t140`）。
/// 修改 `rummage_media::prepare` 的行为（解码、合成白底、方向、尺寸计算、缩放）时必须提升它。
pub const IMAGE_PREPROCESS: &str = "image-v1";

/// 图像向量的预处理参数版本，含视觉 token 档位，如 `image-v1-t140`；也写入 `vectors.params_version`。
pub fn image_preprocess(tier: u32) -> String {
    format!("{IMAGE_PREPROCESS}-t{tier}")
}

/// 长图切片的版本，只出现在切片向量的 `vectors.params_version` 中（`image_slice_preprocess`）。
/// 切片按切片档位（参数 `long_image_tokens`）缩放、编码，与同档位的普通图片完全相同，所以放在该档位的图像空间；
/// 普通比例的图片与长图的整图向量都不受切片影响，不必重算。修改 `rummage_media::slices` / `crop` 时必须提升它。
pub const IMAGE_SLICE: &str = "slice-v1";

/// 长图切片向量的预处理参数版本：切片档位的图像版本加切分参数，如 `image-v1-t560-slice-v1-r2.5-n12`
pub fn image_slice_preprocess(tier: u32, max_ratio: f64, max_slices: u32) -> String {
    format!(
        "{}-{IMAGE_SLICE}-r{max_ratio}-n{max_slices}",
        image_preprocess(tier)
    )
}

/// 音频预处理参数版本。修改 `rummage_media::audio` 的解码（重采样、混音）或切段、静音判定的规则时必须提升它
pub const AUDIO_PREPROCESS: &str = "audio-v2";

/// 音频向量的预处理参数版本，含切段长度、实际使用的重叠（秒）与静音阈值（dB），如 `audio-v2-s10-o2-q-60`；
/// 也写入 `vectors.params_version`。切段参数改变的是每个向量覆盖哪一段声音，所以与图像的档位一样计入空间，
/// 文本查询照样能比较新旧两个音频空间（`rummage_index::comparable`），重建期间旧向量仍可搜到
pub fn audio_preprocess(segment_s: u32, overlap_s: u32, silence_db: i64) -> String {
    format!("{AUDIO_PREPROCESS}-s{segment_s}-o{overlap_s}-q{silence_db}")
}

/// 视频预处理参数版本。修改 `rummage_media::video` 的窗口、抽帧、缩放或组装规则时必须提升它
pub const VIDEO_PREPROCESS: &str = "video-v2";

/// 视频窗口向量的预处理参数版本，含窗口秒数、实际使用的重叠、每窗口帧数、每帧视觉 token 档位（参数 `video_frame_tokens`），
/// 以及带音频（`av`）还是纯画面（`v`），如 `video-v2-w2-o0-f2-t140-v`；也写入 `vectors.params_version`。
/// 带音频与纯画面是两个子空间（`video_av` / `video_v`），分数分布不同，分开检索
pub fn video_preprocess(
    window_s: u32,
    overlap_s: u32,
    frames: u32,
    frame_tokens: u32,
    audio: bool,
) -> String {
    format!(
        "{VIDEO_PREPROCESS}-w{window_s}-o{overlap_s}-f{frames}-t{frame_tokens}-{}",
        if audio { "av" } else { "v" }
    )
}

/// 建索引时带音频的视频窗口的版本：再加上片段长度上限（参数 `video_max_tokens`），如 `video-v2-w6-o0-f6-t140-av-m1152`。
/// 上限决定哪些视频整段改为只看画面，改了它这些视频要重建；参考向量不受上限约束，用 `video_preprocess`
pub fn video_av_preprocess(
    window_s: u32,
    overlap_s: u32,
    frames: u32,
    frame_tokens: u32,
    max_tokens: usize,
) -> String {
    format!(
        "{}-m{max_tokens}",
        video_preprocess(window_s, overlap_s, frames, frame_tokens, true)
    )
}

/// 模型仓库与钉定的提交。下载地址为 `resolve/<提交>/<文件名>`，不跟随 main。
pub const REPO: &str = "ggml-org/embeddinggemma-2-GGUF";
pub const COMMIT: &str = "bfcd298762cc34d0357ece5ebdd31791a3a374d8";
/// 模型在 `<数据目录>\models\` 下的子目录
pub const MODEL_DIR: &str = "embeddinggemma-2-gguf";

/// 钉定的模型文件；也是下载接口不可用、手动导入时使用的已知哈希表。
#[derive(Debug)]
pub struct ModelFile {
    /// 仓库中与本地 `MODEL_DIR` 下的文件名
    pub name: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

pub const MODEL: ModelFile = ModelFile {
    name: "embeddinggemma-2-Q8_0.gguf",
    sha256: "2188ac1deca4b77dffefd603c2776a9d76d9d74ec01841392982ebb840b09135",
    size: 309_855_456,
};

pub const MMPROJ: ModelFile = ModelFile {
    name: "mmproj-embeddinggemma-2-Q8_0.gguf",
    sha256: "c4a8a52691ecef40618438928bdf9e68379b854e24166f292592353db0aab64f",
    size: 554_821_024,
};

impl ModelFile {
    /// 只核对大小；大小相同而内容不同的文件由下载与导入时的完整哈希校验拦住。
    pub fn check(&self, path: &Path) -> Result<(), Error> {
        let meta = std::fs::metadata(path).map_err(|source| Error::ModelIo {
            path: path.to_path_buf(),
            source,
        })?;
        if meta.len() != self.size {
            return Err(Error::ModelSize {
                path: path.to_path_buf(),
                actual: meta.len(),
                expected: self.size,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Cpu,
    Vulkan,
}

impl Backend {
    /// 写入 `spaces.backend` 与空间 ID 的取值。
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Cpu => "cpu",
            Backend::Vulkan => "vulkan",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpaceId(pub String);

impl fmt::Display for SpaceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 计算空间 ID 的全部输入（预处理参数版本除外，按模态给出）。
#[derive(Debug, Clone)]
pub struct SpaceConfig {
    pub family: String,
    pub model_sha256: String,
    pub mmproj_sha256: String,
    pub runtime: String,
    pub backend: Backend,
    pub precision: String,
    pub prefix_scheme: String,
}

impl SpaceConfig {
    /// 当前程序使用的配置。
    pub fn current(backend: Backend) -> Self {
        SpaceConfig {
            family: FAMILY.to_string(),
            model_sha256: MODEL.sha256.to_string(),
            mmproj_sha256: MMPROJ.sha256.to_string(),
            runtime: RUNTIME.to_string(),
            backend,
            precision: PRECISION.to_string(),
            prefix_scheme: PREFIX_SCHEME.to_string(),
        }
    }

    pub fn text(&self) -> SpaceId {
        self.id(&self.model_sha256, TEXT_PREPROCESS)
    }

    /// `tier` 为视觉 token 档位（参数 `image_tokens`，长图切片为 `long_image_tokens`），档位决定向量。
    pub fn image(&self, tier: u32) -> SpaceId {
        self.id(
            &format!("{}+{}", self.model_sha256, self.mmproj_sha256),
            &image_preprocess(tier),
        )
    }

    /// 音频与视频的空间：`preprocess` 为 `audio_preprocess` / `video_preprocess` 给出的预处理参数版本；
    /// 两者都经过 mmproj，取主模型加 mmproj 的哈希
    pub fn media(&self, preprocess: &str) -> SpaceId {
        self.id(
            &format!("{}+{}", self.model_sha256, self.mmproj_sha256),
            preprocess,
        )
    }

    fn id(&self, models: &str, preprocess: &str) -> SpaceId {
        let canonical = format!(
            "family={}\nmodels={models}\nruntime={}\nbackend={}\nprecision={}\nprefix={}\npreprocess={preprocess}\n",
            self.family,
            self.runtime,
            self.backend.as_str(),
            self.precision,
            self.prefix_scheme,
        );
        let hash = Sha256::digest(canonical.as_bytes());
        let hex: String = hash[..8].iter().map(|b| format!("{b:02x}")).collect();
        SpaceId(hex)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_ids() {
        let cfg = SpaceConfig::current(Backend::Cpu);
        let text = cfg.text();
        assert_eq!(text.0.len(), 16);
        assert!(text.0.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(text, SpaceConfig::current(Backend::Cpu).text());
        assert_eq!(text_version(384, 15, 64), "text-v1-t64-c384-o15-m64");
        assert_eq!(cfg.image(140), cfg.image(140));
        assert_ne!(text, cfg.image(140));
        assert_ne!(cfg.image(140), cfg.image(280));
        assert_eq!(
            image_slice_preprocess(560, 2.5, 12),
            "image-v1-t560-slice-v1-r2.5-n12"
        );
        assert_eq!(
            image_slice_preprocess(280, 2.0, 8),
            "image-v1-t280-slice-v1-r2-n8"
        );
        let audio = audio_preprocess(10, 2, -60);
        assert_eq!(audio, "audio-v2-s10-o2-q-60");
        assert_eq!(cfg.media(&audio), cfg.media(&audio));
        assert_ne!(cfg.media(&audio), cfg.media(&audio_preprocess(5, 1, -60)));
        assert_ne!(cfg.media(&audio), cfg.image(140));
        assert_ne!(cfg.media(&audio), text);
        let (av, v) = (
            video_preprocess(6, 0, 6, 70, true),
            video_preprocess(6, 0, 6, 70, false),
        );
        assert_eq!(v, "video-v2-w6-o0-f6-t70-v");
        assert_eq!(av, "video-v2-w6-o0-f6-t70-av");
        assert_ne!(cfg.media(&av), cfg.media(&v));
        assert_ne!(
            cfg.media(&v),
            cfg.media(&video_preprocess(2, 0, 2, 70, false))
        );
        assert_ne!(
            cfg.media(&v),
            cfg.media(&video_preprocess(6, 0, 6, 140, false))
        );
        assert_eq!(
            video_av_preprocess(6, 0, 6, 140, 1152),
            "video-v2-w6-o0-f6-t140-av-m1152"
        );
    }

    #[test]
    fn every_field_changes_id() {
        let base = SpaceConfig::current(Backend::Cpu);
        let variants: Vec<(&str, SpaceConfig)> = vec![
            (
                "family",
                SpaceConfig {
                    family: "x".into(),
                    ..base.clone()
                },
            ),
            (
                "model",
                SpaceConfig {
                    model_sha256: "x".into(),
                    ..base.clone()
                },
            ),
            (
                "runtime",
                SpaceConfig {
                    runtime: "x".into(),
                    ..base.clone()
                },
            ),
            (
                "backend",
                SpaceConfig {
                    backend: Backend::Vulkan,
                    ..base.clone()
                },
            ),
            (
                "precision",
                SpaceConfig {
                    precision: "x".into(),
                    ..base.clone()
                },
            ),
            (
                "prefix",
                SpaceConfig {
                    prefix_scheme: "B".into(),
                    ..base.clone()
                },
            ),
        ];
        for (name, v) in &variants {
            assert_ne!(v.text(), base.text(), "{name} 变化后文本空间应改变");
            assert_ne!(v.image(140), base.image(140), "{name} 变化后图像空间应改变");
        }
    }

    #[test]
    fn text_ignores_mmproj() {
        let base = SpaceConfig::current(Backend::Cpu);
        let other = SpaceConfig {
            mmproj_sha256: "x".into(),
            ..base.clone()
        };
        assert_eq!(other.text(), base.text());
        assert_ne!(other.image(140), base.image(140));
        assert_ne!(other.media("a"), base.media("a"));
    }
}
