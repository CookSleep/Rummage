//! 任务前缀，方案 A（主模型卡写法）。前缀只加在文本上，拼接前去掉输入首尾空白。
//! 前缀方案属于向量空间配置（`space::PREFIX_SCHEME`），改动这里的写法必须同时更换方案名。

pub fn query(q: &str) -> String {
    format!("task: search result | query: {}", q.trim())
}

/// 标题最多保留的字符数：防止很长的文件名挤占切片预算（切片上限 512 含前缀）。
/// 它改变文档向量，计入文本向量的参数版本（`space::text_version`），改动后旧向量会被识别为需要重建
pub const MAX_TITLE_CHARS: usize = 64;

/// 去掉首尾空白后按 Unicode 字符截断。
fn short_title(title: &str) -> String {
    title.trim().chars().take(MAX_TITLE_CHARS).collect()
}

/// 没有标题或标题只有空白时写 `none`。
pub fn document(title: Option<&str>, text: &str) -> String {
    let title = title.map(short_title).filter(|t| !t.is_empty());
    format!(
        "title: {} | text: {}",
        title.as_deref().unwrap_or("none"),
        text.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与黄金样本清单中各文本的 `input` 字段逐条核对。
    #[test]
    fn matches_golden_manifest() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/golden/manifest.json"
        );
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let texts = manifest["texts"].as_array().unwrap();
        assert!(!texts.is_empty());
        for t in texts {
            let text = t["text"].as_str().unwrap();
            let got = match t["kind"].as_str().unwrap() {
                "query" => query(text),
                "document" => document(t["title"].as_str(), text),
                kind => panic!("未知的文本类型 {kind}"),
            };
            assert_eq!(got, t["input"].as_str().unwrap(), "{}", t["id"]);
        }
    }

    #[test]
    fn trims_and_formats() {
        assert_eq!(query("  猫 \n"), "task: search result | query: 猫");
        assert_eq!(document(None, " a "), "title: none | text: a");
        assert_eq!(document(Some(" 报告 "), "b"), "title: 报告 | text: b");
        assert_eq!(document(Some(""), "b"), "title: none | text: b");
        assert_eq!(document(Some(" \n "), "b"), "title: none | text: b");
    }

    #[test]
    fn long_title_truncated() {
        let long = "长".repeat(100);
        let want = format!("title: {} | text: x", "长".repeat(64));
        assert_eq!(document(Some(&format!(" {long} ")), "x"), want);
        let exact = "a".repeat(64);
        assert_eq!(
            document(Some(&exact), "x"),
            format!("title: {exact} | text: x")
        );
    }
}
