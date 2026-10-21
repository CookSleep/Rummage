//! 手机 NPU 用的文件：按芯片编译的 LiteRT 模型、厂商的分发库，以及高通 NPU 需要的 QNN 运行库。
//!
//! - 芯片按系统属性 `ro.soc.model` 识别（`for_soc`），只比较字母与数字、不分大小写，如 `SM8550`、`MT6991`、`Tensor G5`。
//!   模型在 `space::LITERT_REPO` 的同一提交下，文件名为 `embeddinggemma-2-740m_<厂商>_<芯片>.litertlm`。
//! - 分发库（`libLiteRtDispatch_*.so`，Apache-2.0）取自 Google Maven 的 `litert-npu-runtime-*:2.3.0`，随 APK 分发。
//!   联发科与 Google Tensor 的厂商库在系统中；高通的 QNN 运行库是高通的专有许可，不随 APK 分发，
//!   用到时从 Maven Central 的 `qnn-runtime` 包中按需取出（`QNN_*`）。版本须与分发库编译时用的 QAIRT 相同（2.50.0）。
//! - QNN 包（AAR，即 zip）很大，只取需要的几个条目：`QnnEntry` 记下条目在包中的位置与压缩后大小，下载时用 HTTP Range
//!   只取这一段并解压（deflate），解压后按 `file` 核对大小与 SHA-256。

use crate::space::ModelFile;

/// NPU 的厂商
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    /// 带 HTP 版本（如 `V73`），决定要取哪一对 QNN 的 Stub 与 Skel
    Qualcomm(&'static str),
    MediaTek,
    GoogleTensor,
}

impl Vendor {
    /// 随 APK 分发的分发库文件名
    pub fn dispatch_lib(self) -> &'static str {
        match self {
            Vendor::Qualcomm(_) => "libLiteRtDispatch_Qualcomm.so",
            Vendor::MediaTek => "libLiteRtDispatch_MediaTek.so",
            Vendor::GoogleTensor => "libLiteRtDispatch_GoogleTensor.so",
        }
    }
}

/// 一颗芯片的 NPU 模型
#[derive(Debug)]
pub struct NpuModel {
    /// 与 `ro.soc.model` 比较的型号
    pub soc: &'static str,
    pub vendor: Vendor,
    pub file: ModelFile,
}

pub const MODELS: [NpuModel; 8] = [
    NpuModel {
        soc: "SM8550",
        vendor: Vendor::Qualcomm("V73"),
        file: ModelFile {
            name: "embeddinggemma-2-740m_Qualcomm_SM8550.litertlm",
            sha256: "802686a865fd7350f2ce4b6fa17ce02a411f56b5b508ec2bd8cb7310b2db82e6",
            size: 562_905_088,
        },
    },
    NpuModel {
        soc: "SM8650",
        vendor: Vendor::Qualcomm("V75"),
        file: ModelFile {
            name: "embeddinggemma-2-740m_Qualcomm_SM8650.litertlm",
            sha256: "3b8a18c6d043b8132c4eda45611296e1ae455ada82cda8b591ec0ca141e041c5",
            size: 562_659_328,
        },
    },
    NpuModel {
        soc: "SM8750",
        vendor: Vendor::Qualcomm("V79"),
        file: ModelFile {
            name: "embeddinggemma-2-740m_Qualcomm_SM8750.litertlm",
            sha256: "ac1973c0195dc6e28a621651f2f5e04d35fdc7a7b11cdbbe94f29f5dccb57996",
            size: 543_522_816,
        },
    },
    NpuModel {
        soc: "SM8850",
        vendor: Vendor::Qualcomm("V81"),
        file: ModelFile {
            name: "embeddinggemma-2-740m_Qualcomm_SM8850.litertlm",
            sha256: "19f6e8db6aa74b116d1c558c862ee131f60ce093576d3c8dd1898f656ca8080b",
            size: 559_448_064,
        },
    },
    NpuModel {
        soc: "MT6991",
        vendor: Vendor::MediaTek,
        file: ModelFile {
            name: "embeddinggemma-2-740m_MediaTek_MT6991.litertlm",
            sha256: "99e6a35b824b4f7ad4f7c7f20c59ba380aa90e0b4442aadfee3674230c6c7cca",
            size: 895_008_768,
        },
    },
    NpuModel {
        soc: "MT6993",
        vendor: Vendor::MediaTek,
        file: ModelFile {
            name: "embeddinggemma-2-740m_MediaTek_MT6993.litertlm",
            sha256: "ed2a2554bfb6d7abcafd0e9e36ee2f8d7de82888e7ed0fc52dd144e65b171742",
            size: 864_681_984,
        },
    },
    NpuModel {
        soc: "Tensor G5",
        vendor: Vendor::GoogleTensor,
        file: ModelFile {
            name: "embeddinggemma-2-740m_Google_Tensor_G5.litertlm",
            sha256: "5b53d76c2c7c66d2dc6230ff158ea0033ac5939ac7cfcf827c3a22cdfc742a5e",
            size: 657_656_500,
        },
    },
    NpuModel {
        soc: "Tensor G6",
        vendor: Vendor::GoogleTensor,
        file: ModelFile {
            name: "embeddinggemma-2-740m_Google_Tensor_G6.litertlm",
            sha256: "ed0e4f413e705946eda544078d8bf5acf10fe5aee2946451c81919a1f59665e7",
            size: 615_713_460,
        },
    },
];

fn normalize(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// 芯片型号（`ro.soc.model`）对应的 NPU 模型；没有时为空
pub fn for_soc(soc: &str) -> Option<&'static NpuModel> {
    let soc = normalize(soc);
    if soc.is_empty() {
        return None;
    }
    MODELS.iter().find(|m| normalize(m.soc) == soc)
}

/// QNN 运行库的版本，与分发库 2.3.0 编译时用的 QAIRT 相同
pub const QNN_VERSION: &str = "2.50.0";
/// QNN 包的下载地址
pub const QNN_URL: &str =
    "https://repo1.maven.org/maven2/com/qualcomm/qti/qnn-runtime/2.50.0/qnn-runtime-2.50.0.aar";
/// QNN 运行库与分发库放在 `<数据目录>/models/` 下的这个目录中，作为 NPU 的分发库目录
pub const QNN_DIR: &str = "qnn-2.50.0";

/// QNN 包中的一个条目
#[derive(Debug)]
pub struct QnnEntry {
    /// 本地文件头在包中的偏移
    pub offset: u64,
    /// 压缩后的字节数（deflate）
    pub compressed: u64,
    pub file: ModelFile,
}

const fn qnn(
    name: &'static str,
    offset: u64,
    compressed: u64,
    size: u64,
    sha256: &'static str,
) -> QnnEntry {
    QnnEntry {
        offset,
        compressed,
        file: ModelFile { name, sha256, size },
    }
}

/// 每颗高通芯片都要的两个库
pub const QNN_COMMON: [QnnEntry; 2] = [
    qnn(
        "libQnnHtp.so",
        4_917_230,
        1_543_106,
        3_979_248,
        "30e2981965e687d71626c91397867da81247c639e598f49fa5d139cbb21255d0",
    ),
    qnn(
        "libQnnSystem.so",
        69_617_128,
        1_497_797,
        4_068_296,
        "ca9e5429c77380e1c3f5a401f00288915ec2f6cc478979bafd57c665391d432f",
    ),
];

/// 按 HTP 版本的 Stub 与 Skel（Stub 在前）
pub const QNN_HTP: [(&str, [QnnEntry; 2]); 4] = [
    (
        "V73",
        [
            qnn(
                "libQnnHtpV73Stub.so",
                54_774_415,
                289_664,
                791_696,
                "33ff99698707e84ea6f4f1d5e588ff8d79354e962c0f6c9e3a14c42c08c69bd7",
            ),
            qnn(
                "libQnnHtpV73Skel.so",
                50_382_805,
                4_391_547,
                18_709_712,
                "024a0aea3d8d44fc5b59ffab20bde4348d07d05ad7d23f27c8bd06aa3d240d8a",
            ),
        ],
    ),
    (
        "V75",
        [
            qnn(
                "libQnnHtpV75Stub.so",
                59_472_731,
                289_659,
                791_696,
                "eca06fdf5bba9d9a291eae6d53ca46c7432e57be6c8e0565a628f2acc353b0a1",
            ),
            qnn(
                "libQnnHtpV75Skel.so",
                55_064_142,
                4_408_526,
                18_693_300,
                "3e9774b74769915b4f54364f8fc25887b3439561a970dca57c9f4dc9612b38af",
            ),
        ],
    ),
    (
        "V79",
        [
            qnn(
                "libQnnHtpV79Stub.so",
                64_201_974,
                289_664,
                791_696,
                "d9f5eca67297125c572995526fc97b1c04cf6b8eb14c81eb0cef5115f58aa766",
            ),
            qnn(
                "libQnnHtpV79Skel.so",
                59_762_453,
                4_439_458,
                18_513_604,
                "860c9d2e7c937c9fb8f8f18daa9a79cab6c566066a2d36f235f6c8708fdc75bd",
            ),
        ],
    ),
    (
        "V81",
        [
            qnn(
                "libQnnHtpV81Stub.so",
                69_317_678,
                299_387,
                816_456,
                "e672e6047088a4440e3027663805b6de7b0531f3e21c45ddb2e19e3f29583d6c",
            ),
            qnn(
                "libQnnHtpV81Skel.so",
                64_491_701,
                4_825_914,
                19_708_192,
                "02047c9fef8a22801c0eefaa79188e87b600372c9813dea3f621ba256d1ddce0",
            ),
        ],
    ),
];

/// 一颗高通芯片要取的 QNN 条目（`QNN_COMMON` 加该 HTP 版本的 Stub 与 Skel）；其他厂商为空
pub fn qnn_entries(vendor: Vendor) -> Vec<&'static QnnEntry> {
    let Vendor::Qualcomm(htp) = vendor else {
        return Vec::new();
    };
    let mut out: Vec<&QnnEntry> = QNN_COMMON.iter().collect();
    if let Some((_, pair)) = QNN_HTP.iter().find(|(v, _)| *v == htp) {
        out.extend(pair);
    }
    out
}

/// 本机芯片的 NPU 模型；没有对应的芯片版（或不是 Android）时为空
pub fn local() -> Option<&'static NpuModel> {
    for_soc(&crate::platform::soc_model())
}

/// 通用版或 NPU 版的模型文件名对应的已知文件
pub fn litert_model(name: &str) -> Option<&'static ModelFile> {
    if name == crate::space::LITERT_MODEL.name {
        return Some(&crate::space::LITERT_MODEL);
    }
    MODELS.iter().map(|m| &m.file).find(|f| f.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_soc() {
        assert_eq!(for_soc("SM8550").unwrap().vendor, Vendor::Qualcomm("V73"));
        assert_eq!(for_soc("sm8650").unwrap().soc, "SM8650");
        assert_eq!(for_soc("Tensor G5").unwrap().vendor, Vendor::GoogleTensor);
        assert_eq!(for_soc("tensor_g6").unwrap().soc, "Tensor G6");
        assert!(for_soc("SM8450").is_none());
        assert!(for_soc("").is_none());
    }

    #[test]
    fn qnn_for_every_qualcomm_chip() {
        for m in &MODELS {
            let n = qnn_entries(m.vendor).len();
            match m.vendor {
                Vendor::Qualcomm(_) => assert_eq!(n, 4, "{}", m.soc),
                _ => assert_eq!(n, 0, "{}", m.soc),
            }
            assert!(m.file.name.contains(&m.soc.replace(' ', "_")), "{}", m.soc);
        }
    }

    #[test]
    fn known_models() {
        assert!(litert_model(crate::space::LITERT_MODEL.name).is_some());
        assert_eq!(
            litert_model("embeddinggemma-2-740m_Qualcomm_SM8550.litertlm")
                .unwrap()
                .size,
            562_905_088
        );
        assert!(litert_model("x.litertlm").is_none());
    }
}
