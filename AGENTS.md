# Agent Instructions for Rummage

本文件定义 AI 编码助手在此仓库中应遵循的工作方式。

## 项目概况

- 本地多模态文件语义搜索器，Windows 与 Android 双端，Tauri 2。
- 前端：React 19 + Vite + TypeScript + Tailwind CSS 3 + Zustand，源码在 `src/`。构建产物由 Vite 生成，不要手动编辑 `dist/`。
- 后端：Rust 工作区（`crates/*` + `src-tauri`）。Windows 上界面进程（`src-tauri`）与核心进程（`rummage-core`，同时提供命令行与 MCP）分离；Android 上核心在应用进程内运行。
  - `rummage-proto`：界面、命令行与核心进程之间的消息类型、参数表、设置。
  - `rummage-embed`：推理（任务前缀、精度、视觉 token 档位、向量空间、自检），后端为 `rummage-llama`（llama.cpp FFI）与 `rummage-litert`（LiteRT-LM）。
  - `rummage-index`：数据库、扫描、作业、检索与分数校准。
  - `rummage-extract`：文档解析与切片；`rummage-media`：图像、音频、视频的解码与预处理。
  - `rummage-datadir`：数据目录与迁移；`tauri-plugin-rummage-android`：Android 平台接口。
- 包管理器为 npm（有 `package-lock.json`），不要使用 yarn 或 pnpm。

## 常用命令

| 操作 | 命令 |
|------|------|
| 安装前端依赖 | `npm install` |
| 前端开发服务器 | `npm run dev` |
| 客户端开发 | `npm run tauri:dev` |
| 前端构建 | `npm run build` |
| 前端测试 | `npm test` |
| 前端监听测试 | `npm run test:watch` |
| Rust 测试 | `cargo test --workspace` |
| Rust 检查 | `cargo clippy --workspace --all-targets` |
| Rust 格式化 | `cargo fmt --all` |
| 本机测试（需本机的运行库、模型或显卡，以 `#[ignore]` 标记） | `cargo test --workspace -- --ignored <测试名>`，按名称逐个运行；需要显卡的测试不要一次跑完 |
| 评测（仅本机） | `cargo run -p rummage-core --release -- eval <评测集目录>` |
| 评测 LiteRT-LM 空间（仅本机，只用 CPU；先运行 `scripts/fetch-litert.ps1` 与 `scripts/fetch-models.ps1 -Gguf none -LiteRt`） | `cargo run -p rummage-core --release -- eval <评测集目录> --runtime litert --param image_tokens=140` |
| 性能测试（仅本机） | `cargo run -p rummage-core --release -- bench` |
| 客户端打包 | `npm run tauri:build` |
| Android 调试包（arm64；需要 Windows 开发者模式与 `rustup target add aarch64-linux-android`，先在当前终端设 `ANDROID_HOME=<SDK 目录>`、`NDK_HOME=<SDK 目录>\ndk\<版本>`，以及纯英文路径的 `CARGO_TARGET_DIR`：NDK 的链接器包装脚本处理不了含中文的路径；运行库由 `scripts/fetch-litert.ps1 -Platform android_arm64` 取得） | `npx tauri android build --debug --target aarch64 --apk` |
| Android 插件单元测试（先构建过一次 Android 调试包，在 `src-tauri/gen/android` 下运行，需设 `ANDROID_HOME` 与纯英文路径的 `CARGO_TARGET_DIR`：测试进程处理不了含中文的路径） | `gradlew.bat --no-configuration-cache -I ..\..\..\scripts\android-ascii-build.gradle :tauri-plugin-rummage-android:testDebugUnitTest` |
| 依赖许可检查 | `npm run check:licenses` |
| 生成第三方许可清单（需要先安装 cargo-about：`cargo install cargo-about --version 0.9.2 --locked --features cli`，并运行 `scripts/fetch-llama.ps1` 取得 OpenMP 许可） | `npm run notices` |

- `npm run tauri:build` 需要 `scripts/fetch-llama.ps1`（含 `-Variant vulkan`）、`scripts/fetch-ffmpeg.ps1` 获取的第三方二进制。
- 核心进程会常驻，重新构建或运行 `cargo test` 前先运行 `target\debug\rummage-core.exe --shutdown`（`npm run tauri:dev` 会自动执行）。
- 前端测试使用 Vitest，文件命名 `*.test.ts`；组件测试命名 `*.test.tsx`，文件开头写 `// @vitest-environment jsdom`，用 React 的 `createRoot` + `act` 渲染，后台服务用 `src/test/tauri.ts`（基于 `@tauri-apps/api/mocks`）模拟，不引入 Testing Library。Rust 测试放在被测模块的 `#[cfg(test)] mod tests` 中，测试较多时可放在同名子模块文件（如 `local/tests.rs`）；文档提取与切片用 `insta` 快照测试。
- 不要新增 lint/formatter 配置文件，除非明确要求。Rust 使用 rustfmt 默认配置。

## 本机文件与测试

- 大文件只下载一次、重复使用：
  - 模型文件：开发版的数据目录是 `src-tauri/dev-data/`，模型下载到其中的 `models/` 后会一直保留，不要在测试或清理脚本里删除它。
  - llama.cpp 预编译包、ffmpeg、LiteRT-LM 等第三方二进制：`scripts/fetch-*.ps1` 下载到 `.cache/third-party/`，校验哈希后复用，已存在且哈希正确时不再下载。
  - 不随仓库分发的测试输入（如黄金样本的原视频）放在 `.cache/golden/`。
  - `src-tauri/dev-data/` 和 `.cache/` 都已列入 `.gitignore`，不要提交其中的内容。
- 测试用到模型或本机文件时，从上面的位置读取；找不到时跳过并提示，不要在测试中自动下载。
- GPU 测试会影响整台电脑（驱动超时重置可能需要重启）：在可强行结束的子进程中运行并设总时限；不批量连续跑 GPU 配置；出现第一次设备丢失或 TDR 就停止并报告。

## 代码风格（强制）

### 简单优先

写出能工作的**最简代码**。少抽象、少包装。有疑问就内联。

- 不要为单次使用的 1-5 行逻辑创建独立函数，直接内联。
- 函数只有在**多处调用**且**逻辑非平凡**时才值得提取。
- 不要引入项目中不存在的设计模式或架构层。Rust 中不要为只有一个实现的东西定义 trait。

### 完整实现

- 不要留 `// TODO: implement later`、`// ...`、`todo!()`、`unimplemented!()` 或 stub 函数。
- 如果不确定某个细节，给出完整的最佳猜测实现。错误但完整的代码优于正确但残缺的骨架。

### 跟随现有风格

这是最高优先级规则。修改文件时，遵循该文件及周围代码的已有风格。

### 格式（TypeScript）

- **2 空格缩进**。
- **单引号**（`'hello'`）。
- **无分号**。
- 箭头函数始终加括号：`(x) => x`。
- 行宽不做硬性限制，但尽量保持可读。

### TypeScript

- 使用 ESM import，`const` 优先，永远不用 `var`。
- Target `ES2020`（见 `tsconfig.json`）。
- 优先早返回，避免深层嵌套和 `else` 链。
- 尽量避免 `any`；需要时保持局部化。
- 利用类型推断，不写多余的类型注解。
- 共享类型放 `src/types.ts`，局部类型放文件顶部。与 Rust 侧对应的类型注明对应的 Rust 结构体，修改时两边同步。

### Rust

- 格式以 `cargo fmt` 为准（4 空格缩进）。
- 错误处理：库 crate 用 `thiserror` 定义错误类型；返回给前端的错误是带 `code` 和中文 `message` 的结构体，`message` 是用户能看懂、能自行处理的整句。
- 不在库代码中 `unwrap()` / `expect()` 外部输入（文件内容、网络数据、数据库读出的值）；只有不变量确实成立时才用 `expect`，并写明原因。
- 所有 `unsafe` 只允许出现在 `rummage-llama`（FFI）和平台接口模块中，每处写 `// SAFETY:` 注释说明为什么成立。
- 长时间运行或可能阻塞的操作（推理、解码、文件 I/O）不要放在 Tauri 命令的异步执行器上直接跑，用 `spawn_blocking` 或专用线程。
- 模块顶部用 `//!` 写一段中文说明：这个模块负责什么、关键的约束和流程。

### 命名

- **PascalCase**：组件、类型、接口；Rust 的类型与 trait。
- **camelCase**：TS 函数、变量、参数。
- **snake_case**：Rust 函数、变量、模块、Tauri 命令名。
- **UPPER_SNAKE_CASE**：模块级常量。
- TS 文件名小写驼峰：`searchResults.ts`、`indexProgress.ts`。
- 局部变量优先短名：`ctx`、`el`、`msg`、`idx`、`opts`、`err`。多词名仅在单词不够清晰时使用。

### 解构

避免无必要的解构。优先点号访问以保留上下文。

```ts
// 好
hit.locator
opts.settings

// 避免
const { locator } = hit
const { settings } = opts
```

例外：React 组件 props、hooks 返回值、函数参数解构是可以的。

### 控制流

```ts
// 好：早返回
function getPreset(name: string) {
  if (!name) return defaultPreset
  return presets.find((p) => p.name === name)
}

// 避免：多余的 else
function getPreset(name: string) {
  if (!name) return defaultPreset
  else return presets.find((p) => p.name === name)
}
```

### 变量

优先 `const`，用三元或早返回代替 `let` 重赋值。

### 常量提取

不要为一次性使用的字面量定义命名常量。只有满足以下条件之一才提取：
1. 多处使用，或
2. 含义不一目了然，或
3. 是需要调优的关键参数（切片长度、长图阈值、视频窗口、相关度门槛、自检门槛等）。这类参数必须登记在 `rummage-proto` 的参数表中（默认值、允许范围、是否影响索引、默认值的依据，只写结论和关键数字或“经验值”），由设置界面开放给用户调整，代码中只从参数表读取，不要另写字面量。

### 防御性代码

本项目涉及大量**外部输入**：用户磁盘上的任意文件、模型文件、数据库中的旧数据、命令行与 MCP 请求。对这些保留必要的校验和兼容处理。

但不要对已声明为非可选的内部类型加投机性空检查。

## Import 顺序

大致分组：
1. React / React DOM
2. 第三方包
3. 本地类型（`../types`、`./types`）
4. 本地模块（`./lib/*`、`./hooks/*`、`./components/*`）

Rust 按 `std` → 第三方 crate → 工作区 crate → `crate::` / `super::` 分组。

## React 组件

- 函数组件 + hooks，不使用 class 组件。
- 组件文件放 `src/components/`，hooks 放 `src/hooks/`，工具函数放 `src/lib/`。
- 复杂 UI 逻辑可以拆成独立组件或 hook，不必强行内联。
- Tailwind 类名不强制排序，但同类属性（布局、间距、颜色、交互）尽量分组书写，保持可读。
- 视觉风格沿用 GPT Image Playground 的设计令牌和组件风格，新组件先找已有组件参照。
- 结果列表、缩略图网格必须虚拟化，只渲染可视区域附近的条目。

## 错误处理

- 对文件 I/O、网络请求、推理调用使用 `try/catch`（TS）或 `Result`（Rust），用 `console.warn` / `console.error` 或 `log::warn!` / `log::error!` 记录。
- 不要对没有证据会失败的路径加投机性错误处理。
- 单个文件处理失败不能影响整个索引任务：记录到作业表，继续下一个。

## 注释与语言

- 代码注释使用**中文**。
- UI 文案默认中文，使用中文标点（，。“”）。
  - 引号：中文句中的设备名用「」；界面弹窗中的路径和界面中“设置 → 某页”这类位置用「」（加粗显示）；命令行与后台服务文字中的路径、文件名用英文直引号 `"…"`，便于复制到命令中；设置项名、类型名等其他名称用“”。
  - 句号：胶囊通知、空状态、状态行、按钮旁的简短提示等只有一句的提示不加句末句号，后台服务返回的一句话错误显示在胶囊通知中时也去掉；两句以上的说明、弹窗正文、设置说明照常用句号。
- 注释应简洁，说明“为什么”而非“做了什么”——除非逻辑复杂不易一眼看出。代码本身能说清的不写注释。
- 注释只描述代码现在的样子，读者是以后看代码的人，不是这次的审查者。不要写：
  - 开发过程：改动前是什么样、修了什么问题、哪轮审查发现的、“新增”“改为”“不再”“之前”这类相对说法；
  - 开发计划、里程碑或步骤编号、提交号、Issue / PR 编号及其讨论过程、会话或报告；
  - 本机内容：真实文件名、本机绝对路径、仓库外的目录；测试数据用 `D:\照片\a.jpg` 这类通用示例；
  - 复述函数名、参数名或下一行代码，以及“注意”“重要”之类没有信息量的提示；
  - 对不会发生的情况的辩解，或已删除代码的去向。
- 涉及实测数据的取舍（例如为什么 CPU 关闭 Flash Attention、为什么 GPU 用默认精度设置），注释只写结论和关键数字，如“Vulkan 上 1120 档不开 Flash Attention 会算错”，不写测试经过和日期。

## 架构约束

- **依赖协议**：只引入 MIT、Apache 2.0、BSD、ISC、Zlib、公有领域等宽松协议的依赖，**不引入 GPL / AGPL**。新增依赖前先确认协议；第三方许可清单 `src/assets/third-party-notices.txt` 是提交进仓库的生成文件，在“设置 → 关于 → 第三方许可”中显示（Windows 与 Android 相同），Rust 或前端依赖变化后运行 `npm run notices` 重新生成并一起提交；Rust 依赖中出现新的宽松协议时先确认，再加入 `about.toml` 的 `accepted`；npm 包中没有许可文件时生成会失败，需从上游仓库对应版本取许可原文放进 `third-party-licenses/npm/`。Tauri 等带进来的 MPL-2.0 间接依赖（未修改）可以接受，许可清单中注明其源码位置。ffmpeg 只用 LGPL 构建，以独立可执行文件调用。
- **版本钉死**：Rust 依赖写精确版本（`=x.y.z`）；前端依赖按常规写法即可，由 `package-lock.json` 锁定。llama.cpp 与 Xberg 升级时必须单独提交，并在提交说明中写明看过的更新日志要点；llama.cpp 升级必须用黄金向量（`assets/golden/`）对比新旧向量（余弦 ≥ 0.9999 视为一致），一致时登记为兼容空间，不一致时按向量空间变化重建；Xberg 升级后快照变化需人工审阅。
- **llama.cpp FFI**：只在 `rummage-llama` 中调用 C 接口（`llama.h`、`mtmd.h`、`ggml-backend.h`），签名以 `third_party/llama.cpp/<标签>/` 中的头文件快照为准（当前 b11461，不是 master 分支）。其他 crate 只通过 `rummage-embed` 的接口使用推理。
- **推理参数**：任务前缀、精度设置（**Flash Attention 必须显式设置**，llama 上下文与 mtmd 都要设，不用默认的 `AUTO`：CPU 后端关闭，黄金向量按关闭计算；GPU 后端打开，否则 1120 档在 Vulkan 上算错，且更慢、更占显存；Windows 的 Vulkan 后端其余用默认精度设置）、视觉 token 档位、截断后重新归一化等规则集中在 `rummage-embed` 中实现，不要在调用处拼前缀、改上下文参数或自行截断向量。
- **GPU 后端**：未经自检的 GPU 后端只能在可强行结束的子进程中初始化；设备丢失立即屏蔽，同一会话不重试。
- **向量来源**：每条向量都必须带向量空间 ID 和预处理参数版本。不同空间的向量不能直接比较。
- **网盘占位文件**：任何读取文件内容的操作（打开、读字节、哈希、缩略图、解码）之前，都必须先检查占位文件属性，避免触发下载。新增读取文件的代码路径时，复用统一的检查函数，不要自己再写一份。
- **只读原则**：程序不修改、不移动、不删除用户被索引的文件；命令行与 MCP 不提供读取任意文件内容的能力。
- **数据目录**：所有应用数据写在数据目录中，路径一律通过数据目录模块取得，不要硬编码 `AppData` 等位置。
- **数据库**：迁移脚本数组只能追加，已发布的项不能修改；新增表或字段时同时考虑旧数据的升级路径。
- **Tauri 命令与权限**：新增命令时，同步在 `src-tauri/permissions/*.toml` 中加入对应模块的权限，并确认 `capabilities/default.json` 只授予需要的权限。不要向前端开放任意路径读写或打开的命令。
- **新增较大功能**时，优先拆成独立模块（Rust 模块 / lib 函数 + hook + 组件），而非全部塞进现有大文件。
- 组件或单个 Rust 源文件超过 800 行时，考虑按逻辑边界拆分。
- **平台代码**：Windows / Android 专有实现用 `#[cfg(target_os = "...")]` 隔离在平台模块中，公共逻辑不得直接调用平台 API。非 Windows 平台的占位实现只用于编译和测试，必须能编译通过并有合理行为。

## 发布版本升级

- 准备新版本时，先检查当前分支相对远端的全部未推送提交和未提交修改，再据此编写 `RELEASE.md`。
- `RELEASE.md` 使用实际发布日期，只记录最终面向用户的新增、变更和修复；不要记录开发新内容时产生的中间调整或配套修复。
- `RELEASE.md` 格式：
  - 标题使用 `## v版本号（YYYY-MM-DD）`，日期括号使用中文全角括号。
  - 内容按需使用 `### 新增`、`### 变更`、`### 修复`、`### 贡献` 分类，没有内容的分类不要保留。
  - 每项使用 `- ` 开头，以完整中文句子概括用户可感知的结果，句末使用中文句号。
  - 较大的功能先用一条主条目概括，再用缩进两个空格的 `  - ` 子条目分点说明入口、主要能力和细节；子条目同样是以中文句号结尾的完整句子，每条只讲一件事。小改动保持单条，不必拆分。
  - 相关 PR 或 Issue 编号放在条目末尾，格式为 `（PR #42）` 或 `（Issue #42）`；有子条目时只标在主条目末尾。
  - 外部贡献在 `### 贡献` 中使用 `- @用户名 通过 PR #编号 贡献了……，特此感谢。` 格式致谢。
  - 更改了索引格式、需要用户重建索引的版本，在对应条目中明确说明，并说明重建期间旧索引仍可搜索。
- 提升版本号时必须同步更新以下位置：
  - `package.json` 的 `version`。
  - `package-lock.json` 顶层及根包的 `version`。
  - 根 `Cargo.toml` 的 `[workspace.package] version`（所有 crate 继承该版本），以及 `Cargo.lock` 中所有工作区 crate 的 `version`。
  - `RELEASE.md` 标题中的版本号和日期。
  - `tauri.conf.json` 直接引用 `package.json`；Android 的 `versionCode` 由版本号自动计算，均无需修改。
- 客户端更新包由 `tauri build` 用私钥签名（`TAURI_SIGNING_PRIVATE_KEY`，私钥设有密码，需同时提供 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`；GitHub Secrets 中两者都要设置，本机打包时以环境变量传入，密码不得写进任何文件或提交）。私钥不得提交进仓库。
- Android APK 的签名密钥库与密码只存在 GitHub Secrets（`ANDROID_KEYSTORE_BASE64`、`ANDROID_KEYSTORE_PASSWORD`、`ANDROID_KEY_ALIAS`、`ANDROID_KEY_PASSWORD`）和离线备份中，不得提交进仓库。
- 发布的 Release 中必须包含：Windows 安装包、对应的 `.sig` 和 `latest.json`；Android 的 `Rummage_<版本>_android-arm64.apk`（正式密钥签名）和 `android.json`（`version`、`url`、`sha256`、`size`）。缺少任一项时，对应平台的应用内更新会提示找不到更新包。
- 本机构建并签名 release APK（核对发布产物时用；环境变量同上方“Android 调试包”，另设 `ANDROID_KEYSTORE_PASSWORD`、`ANDROID_KEY_PASSWORD`；密钥库放在仓库外，用完删除产物）：
  1. `npx tauri android build --target aarch64 --apk`，得到未签名的 `src-tauri\gen\android\app\build\outputs\apk\universal\release\app-universal-release-unsigned.apk`。
  2. `%ANDROID_HOME%\build-tools\<版本>\zipalign -P 16 -f 4 <未签名 APK> aligned.apk`（必须在签名之前）。
  3. `apksigner sign --ks <密钥库> --ks-pass env:ANDROID_KEYSTORE_PASSWORD --ks-key-alias <别名> --key-pass env:ANDROID_KEY_PASSWORD --v4-signing-enabled false --out Rummage_<版本>_android-arm64.apk aligned.apk`，再运行 `apksigner verify --verbose <APK>`。
- 推送 `v版本号` tag 会触发 `.github/workflows/tauri-release.yml`（只由推送 tag 触发，不提供为已有 tag 手动重新构建的入口；已发布版本有问题时发布新版本修复）：校验 tag 与 `package.json`、Cargo 工作区版本一致，构建或获取并校验 llama.cpp 与 ffmpeg 等第三方二进制，并行构建 Windows 的 NSIS 安装包（`windows` job，Release 先建成预发布）和签名的 Android APK 与 `android.json`（`android` job，NDK 与 build-tools 的版本钉在工作流中）。两者都成功后，`publish` job 上传 APK 与 `android.json`，用 `gh release edit` 把 Release 的标题设为 tag、正文设为 `RELEASE.md` 中对应版本的段落，并设为正式版；任一平台失败时 Release 停在预发布，客户端看不到它，发布新版本修复。因此先在 `RELEASE.md` 写好该版本再打 tag；不要预先手动创建草稿 Release（tauri-action 查找已有 Release 时不匹配草稿，会另建一个），之后也不要把 Release 改为草稿或预发布，否则客户端的更新地址（`releases/latest/download/` 下的 `latest.json`、`android.json`）找不到它。
- 存放 ffmpeg 镜像的 `deps` Release（`scripts/fetch-ffmpeg.ps1` 先从这里下载）必须是预发布，否则它会成为最新 Release，`latest.json` 返回 404。
- 修改 `src-tauri/windows/installer.nsi`（基于 Tauri 内置模板的自定义 NSIS 模板，改动处以“Rummage：”注释标出）或升级 `@tauri-apps/cli` 时，需对照新版内置模板同步，并在本机用 `npm run tauri:build` 构建后实测安装、覆盖安装、应用内更新和卸载（含勾选与不勾选“删除应用数据”）。
- 更新后全局搜索旧版本号，确认没有遗漏仍需同步的位置；历史发布记录中的旧版本号不需要修改。
- 发布前运行 `npm run build`、`npm test`、`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`npm run check:licenses`，并复查最终差异。

## 注意事项

- 修改推理相关参数（前缀、视觉 token 数、切片长度、长图阈值、视频窗口、存储精度等）后，在本机跑一遍评测（评测集格式见 `docs/eval/FORMAT.md`），结果放在仓库外、不提交，在提交说明中写明指标变化；影响默认值的结论写进参数表中该参数的依据。
- 测试与调研报告、原始数据不进仓库，也不要在仓库中新建报告文件。`docs/eval/FORMAT.md` 这类规格说明不在此列。
- 修改预处理逻辑时，同步提升对应模态的预处理参数版本号，使旧向量能被识别为需要重建。
- 修改 `rummage-proto` 中的消息类型时，同步提升协议版本号；首次发布前不提升，保持 1。界面、命令行与核心进程都要能识别版本不一致并给出提示。
- 涉及 GPU、NPU 的改动必须在真机上验证，并在提交说明中写明测试的设备与后端。
- 仓库不设 CI，修改完成后、提交前在本地依次运行 `npm run build`、`npm test`、`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`npm run check:licenses`，全部通过再提交；改动 Android 插件的 Kotlin 代码时另跑“Android 插件单元测试”；依赖有变化时先运行 `npm run notices`。先构建前端：编译 `src-tauri` 需要 `dist/` 已存在。
