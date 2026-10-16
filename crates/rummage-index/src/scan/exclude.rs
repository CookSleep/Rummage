//! 遍历时的排除规则：默认排除的目录与文件，只在同级有项目文件时排除的构建输出目录，
//! 以及根目录设置中的用户规则。数据目录本身、隐藏与系统文件、符号链接的排除在遍历（`super::walk`）中判断。
//! - 用户规则类似 `.gitignore`，不区分大小写：不含 `/` 的规则匹配任意深度的文件名或目录名（如 `*.log`、`缓存`）；
//!   含 `/` 的规则相对根目录匹配整个路径，`*` 不跨层，需要跨层时写 `**`（如 `照片/raw`、`项目/**/*.bak`）；
//!   以 `/` 开头的规则同样只匹配相对根目录的路径（如 `/a.txt` 只排除根目录下的 a.txt，与 `.gitignore` 相同）；
//!   匹配到目录时整个目录都排除。
//! - 转义与 `.gitignore` 相同用 `\`：`\` 后跟 `ESCAPED` 中的字符时是转义（如 `/报告\[1\].txt`），其他 `\` 是路径分隔符，
//!   保存时换成 `/`（`slashes`）；编译时开启 `backslash_escape`（globset 在 Windows 上默认关闭）。
//! - 失败文件列表中的“忽略”用 `exact_rule` 写出只匹配一个文件的规则：特殊字符用 `\` 转义，根目录下的文件以 `/` 开头。
//! - 格式不对的规则跳过并记录警告；保存规则前用 `exclude_glob` 检查写法。
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

pub(super) const EXCLUDED_DIRS: &[&str] = &[
    ".git",
    ".svn",
    ".hg",
    "node_modules",
    "__pycache__",
    ".venv",
    "target",
    "$RECYCLE.BIN",
    "System Volume Information",
    "AppData",
];
/// 同级有项目文件时才排除的构建输出目录
pub(super) const BUILD_DIRS: &[&str] = &["build", "dist"];
pub(super) const PROJECT_FILES: &[&str] = &[
    "package.json",
    "Cargo.toml",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "pyproject.toml",
    "setup.py",
    "CMakeLists.txt",
    "go.mod",
];
pub(super) const PROJECT_EXTS: &[&str] = &["sln", "csproj"];
pub(super) const TEMP_SUFFIXES: &[&str] = &[".tmp", ".part", ".crdownload"];

/// 用户排除规则，语义见模块文档
pub(super) struct UserGlobs {
    /// 不含 `/` 的规则，匹配文件名或目录名
    names: GlobSet,
    /// 含 `/` 的规则，匹配相对根目录的路径
    paths: GlobSet,
}

impl UserGlobs {
    /// `rel` 以 `/` 分隔
    pub(super) fn is_match(&self, name: &str, rel: &str) -> bool {
        self.names.is_match(name) || self.paths.is_match(rel)
    }
}

/// `\` 后跟这些字符时是转义，其他 `\` 是路径分隔符
const ESCAPED: [char; 7] = ['*', '?', '[', ']', '{', '}', '\\'];

/// 编译一条用户排除规则（语义见模块文档）；保存规则前也用它检查写法
pub(crate) fn exclude_glob(pat: &str) -> Result<globset::Glob, globset::Error> {
    GlobBuilder::new(pat.strip_prefix('/').unwrap_or(pat))
        .case_insensitive(true)
        .literal_separator(pat.contains('/'))
        .backslash_escape(true)
        .build()
}

/// 把规则中作路径分隔符的 `\` 换成 `/`，转义（`\` 后跟 `ESCAPED` 中的字符）原样保留
pub(crate) fn slashes(rule: &str) -> String {
    let mut out = String::with_capacity(rule.len());
    let mut chars = rule.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next_if(|n| ESCAPED.contains(n)) {
            Some(n) => out.extend(['\\', n]),
            None => out.push('/'),
        }
    }
    out
}

/// 只匹配根目录下这一个文件的排除规则；`rel` 为相对根目录的路径，以 `/` 分隔，特殊字符用 `\` 转义
pub(crate) fn exact_rule(rel: &str) -> String {
    let mut rule = String::new();
    if !rel.contains('/') {
        rule.push('/');
    }
    for c in rel.chars() {
        if ESCAPED.contains(&c) {
            rule.push('\\');
        }
        rule.push(c);
    }
    rule
}

/// 根目录设置中的用户排除规则；格式不对的规则跳过并记录警告
pub(super) fn user_globs(settings: &str) -> UserGlobs {
    let value: serde_json::Value = serde_json::from_str(settings).unwrap_or_else(|err| {
        log::warn!("根目录设置不是合法 JSON，忽略用户排除规则：{err}");
        serde_json::Value::Null
    });
    let mut names = GlobSetBuilder::new();
    let mut paths = GlobSetBuilder::new();
    for pat in value["exclude"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
    {
        match exclude_glob(pat) {
            Ok(glob) if pat.contains('/') => {
                paths.add(glob);
            }
            Ok(glob) => {
                names.add(glob);
            }
            Err(err) => log::warn!("忽略无效的排除规则 {pat}：{err}"),
        }
    }
    let build = |b: GlobSetBuilder| {
        b.build().unwrap_or_else(|err| {
            log::warn!("无法编译排除规则：{err}");
            GlobSet::empty()
        })
    };
    UserGlobs {
        names: build(names),
        paths: build(paths),
    }
}
