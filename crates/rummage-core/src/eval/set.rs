//! 评测集的读取与校验（格式见 `docs/eval/FORMAT.md`）：`manifest.json` 与 `queries.jsonl`。
//! 只保留认识的模态（`image`、`document`、`audio`、`video`）的子集及其查询；评测集中的相对路径以 `/` 分隔，
//! 不以 `/` 开头、不含 `..`，按评测集目录拼成绝对路径。
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

fn field<'a>(v: &'a Value, key: &str, what: &str) -> Result<&'a Value, String> {
    v.get(key).ok_or_else(|| format!("{what}缺少字段 {key}。"))
}

fn text<'a>(v: &'a Value, key: &str, what: &str) -> Result<&'a str, String> {
    field(v, key, what)?
        .as_str()
        .ok_or_else(|| format!("{what}的 {key} 应为字符串。"))
}

/// 评测集中的相对路径：`/` 分隔，不以 `/` 开头，不含 `..`
fn rel_path(root: &Path, rel: &str) -> Result<PathBuf, String> {
    if rel.starts_with('/') || rel.split('/').any(|p| p == ".." || p.is_empty()) {
        return Err(format!("路径 \"{rel}\" 不合法。"));
    }
    Ok(rel
        .split('/')
        .fold(root.to_path_buf(), |p, part| p.join(part)))
}

pub(super) struct Subset {
    pub(super) name: String,
    /// `files/<子集>/`
    pub(super) dir: PathBuf,
}

pub(super) struct Query {
    pub(super) id: String,
    pub(super) subset: String,
    pub(super) text: String,
    pub(super) image: Option<PathBuf>,
    /// 相关文件的绝对路径
    pub(super) relevant: Vec<PathBuf>,
}

/// 读取并校验 `manifest.json` 与 `queries.jsonl`，只保留认识的模态的子集
pub(super) fn load(set: &Path) -> Result<(Vec<Subset>, Vec<Query>), String> {
    let read = |name: &str| {
        fs::read_to_string(set.join(name)).map_err(|err| format!("无法读取 \"{name}\"：{err}"))
    };
    let manifest: Value = serde_json::from_str(&read("manifest.json")?)
        .map_err(|err| format!("manifest.json：{err}"))?;
    if manifest.get("format_version").and_then(Value::as_i64) != Some(1) {
        return Err("manifest.json 的 format_version 不是 1，本程序不认识。".into());
    }
    let mut subsets = Vec::new();
    let list = field(&manifest, "subsets", "manifest.json ")?
        .as_array()
        .ok_or("manifest.json 的 subsets 应为数组。")?;
    for s in list {
        let what = "manifest.json 的子集";
        for key in [
            "name",
            "modality",
            "description",
            "source",
            "license",
            "query_lang",
            "query_origin",
        ] {
            text(s, key, what)?;
        }
        for key in ["count_files", "count_queries"] {
            if !field(s, key, what)?.is_i64() {
                return Err(format!("{what}的 {key} 应为整数。"));
            }
        }
        if matches!(
            text(s, "modality", what)?,
            "image" | "document" | "audio" | "video"
        ) {
            let name = text(s, "name", what)?.to_string();
            let dir = rel_path(set, &format!("files/{name}"))?;
            subsets.push(Subset { name, dir });
        }
    }
    let mut queries = Vec::new();
    for (n, line) in read("queries.jsonl")?.lines().enumerate() {
        let what = &format!("queries.jsonl 第 {} 行", n + 1);
        let q: Value = serde_json::from_str(line).map_err(|err| format!("{what}：{err}"))?;
        let subset = text(&q, "subset", what)?.to_string();
        let relevant = field(&q, "relevant", what)?
            .as_array()
            .filter(|r| !r.is_empty())
            .ok_or_else(|| format!("{what}的 relevant 应为非空数组。"))?
            .iter()
            .map(|r| rel_path(set, text(r, "file", what)?))
            .collect::<Result<Vec<_>, _>>()?;
        let image = match q.get("query_image") {
            Some(v) => Some(rel_path(
                set,
                v.as_str()
                    .ok_or_else(|| format!("{what}的 query_image 应为字符串。"))?,
            )?),
            None => None,
        };
        let q = Query {
            id: text(&q, "id", what)?.to_string(),
            text: text(&q, "query", what)?.to_string(),
            subset,
            image,
            relevant,
        };
        if subsets.iter().any(|s| s.name == q.subset) {
            queries.push(q);
        }
    }
    Ok((subsets, queries))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        let root = Path::new("E");
        assert_eq!(
            rel_path(root, "files/a/b.jpg").unwrap(),
            Path::new("E").join("files").join("a").join("b.jpg")
        );
        assert!(rel_path(root, "/files/a.jpg").is_err());
        assert!(rel_path(root, "files/../a.jpg").is_err());
        assert!(rel_path(root, "files//a.jpg").is_err());
    }

    #[test]
    fn load_checks_format() {
        let dir = tempfile::tempdir().unwrap();
        let subset = r#"{"name": "photo", "modality": "image", "description": "", "source": "", "license": "",
            "query_lang": "zh", "query_origin": "human", "count_files": 1, "count_queries": 1}"#;
        let audio = subset.replace("photo", "sound").replace("image", "audio");
        let video = subset.replace("photo", "clip").replace("image", "video");
        let other = subset.replace("photo", "misc").replace("image", "3d");
        fs::write(
            dir.path().join("manifest.json"),
            format!(r#"{{"format_version": 1, "subsets": [{subset}, {audio}, {video}, {other}]}}"#),
        )
        .unwrap();
        fs::write(
            dir.path().join("queries.jsonl"),
            concat!(
                r#"{"id": "photo-1", "subset": "photo", "query": "猫", "relevant": [{"file": "files/photo/1.jpg", "grade": 2}], "extra": 1}"#,
                "\n",
                r#"{"id": "sound-1", "subset": "sound", "query": "狗叫", "relevant": [{"file": "files/sound/1.wav", "start": 1, "end": 2}]}"#,
                "\n",
                r#"{"id": "clip-1", "subset": "clip", "query": "跳水", "relevant": [{"file": "files/clip/1.mp4"}]}"#,
                "\n"
            ),
        )
        .unwrap();
        let (subsets, queries) = load(dir.path()).unwrap();
        // 只保留认识的模态，不认识的字段忽略
        assert_eq!(subsets.len(), 3);
        assert_eq!(subsets[0].dir, dir.path().join("files").join("photo"));
        assert_eq!(subsets[1].name, "sound");
        assert_eq!(subsets[2].name, "clip");
        assert_eq!(queries.len(), 3);
        assert_eq!(
            queries[1].relevant,
            vec![dir.path().join("files").join("sound").join("1.wav")]
        );
        assert_eq!(queries[0].text, "猫");
        assert_eq!(
            queries[0].relevant,
            vec![dir.path().join("files").join("photo").join("1.jpg")]
        );

        // 缺少必填字段、版本不认识时报错
        fs::write(
            dir.path().join("queries.jsonl"),
            r#"{"id": "photo-1", "subset": "photo", "query": "猫", "relevant": []}"#,
        )
        .unwrap();
        assert!(load(dir.path()).is_err());
        fs::write(
            dir.path().join("manifest.json"),
            r#"{"format_version": 2, "subsets": []}"#,
        )
        .unwrap();
        assert!(load(dir.path()).is_err());
    }
}
