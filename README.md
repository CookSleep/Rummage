<div align="center">

# Rummage

[![License](https://img.shields.io/badge/license-MIT-10b981?style=flat-square)](LICENSE)
[![Release](https://img.shields.io/github/v/release/CookSleep/Rummage?style=flat-square)](https://github.com/CookSleep/Rummage/releases/latest)

**本地运行的多模态文件语义搜索器**

用一句话描述文件的内容，在你选定的文件夹中找到文档、照片、截图、录音和视频，也可以以图搜图。<br>
基于 Google DeepMind 的 EmbeddingGemma 2 与 llama.cpp，全部在本机计算，不上传任何文件。

</div>

> Windows 版。Android 版开发中。

---

## ✨ 功能

### 🔍 按内容搜索
- **一句话描述**：如“京都酒店的收据”“猫趴在键盘上的照片”，中英文都可以；同时匹配文件名和文档中的原词。
- **以图搜图**：点搜索框右侧的按钮选择图片，或把图片拖进窗口；可以再加一句文字描述，如“同一只狗，在草地上”。
- **找相似**：任意结果都可以作为新的查询，找出同类的图片、文档、音频或视频。
- **筛选**：按文件类型、文件夹、修改时间缩小范围。
- **命中位置**：视频和音频标出命中的时间段，文档标出页码并显示命中的段落，长截图框出命中的部分。

### 📁 支持的文件
- **文档**：PDF、Word、Excel、PowerPoint、OpenDocument、RTF、TXT、Markdown、HTML、XML、CSV、JSON。
- **图片**：JPG、PNG、WebP、BMP、TIFF、GIF、HEIC、AVIF（HEIC / AVIF 需要系统已安装对应的图像扩展）。
- **音频**：MP3、M4A / AAC、WAV、FLAC、OGG / Opus、WMA、AMR。
- **视频**：MP4、MOV、MKV、WebM、AVI、WMV、FLV、TS / M2TS、3GP。
- 其他文件只按文件名索引，同样可以搜到。

### 🖼️ 结果与预览
- 结果以瀑布流展示，图片和视频显示缩略图，按匹配度排列。
- 详情栏中直接预览：图片看大图，视频和音频从命中时刻开始播放，文档显示命中片段及前后段落。
- “打开到 03:12”：默认播放器或 PDF 阅读器支持时，直接跳到命中的时间或页码（VLC、mpv、PotPlayer、MPC-HC / MPC-BE、SumatraPDF、Adobe Acrobat / Reader、Microsoft Edge）。
- 全键盘操作：方向键选择，`Enter` 打开，`Ctrl+Enter` 在资源管理器中显示，空格收起或展开详情。

### ⚙️ 索引与设置
- 只索引你添加的文件夹，支持 `.gitignore` 写法的排除规则；文件增删改后自动更新。
- 自动检测显卡（Vulkan），通过检测的显卡用来加速，否则使用 CPU。
- 后台建立索引，使用电池、全屏时自动暂停，可手动暂停；关闭窗口时可留在托盘。
- 网盘“按需下载”的占位文件不会被读取，不会触发下载。
- 分段长度、图片识别精度、视频识别方式、匹配度门槛等参数都可以在设置中调整，修改影响索引的参数后只在后台重建受影响的部分，期间旧索引照常可搜。
- 数据目录可以改到其他磁盘。

### 🤖 命令行与 MCP
- `rummage-core` 命令行可以搜索、查看状态、管理文件夹与设置，供脚本和启动器调用。
- `rummage-core mcp` 提供 MCP 服务，Claude Code、Claude Desktop、VS Code 等 AI 客户端可以直接搜索你的文件。

---

## 💻 系统要求

- 64 位 Windows 10 / 11。
- 磁盘：文字模型约 310 MB，图片与音视频模型约 555 MB（选了图片、音频或视频时才需要），另加索引的占用（引导中会预估）。
- 显卡可选：支持 Vulkan 的独立显卡或核显可以明显加快建立索引的速度。

## 📦 安装

1. 在 [Releases](https://github.com/CookSleep/Rummage/releases/latest) 中下载以 `_x64-setup.exe` 结尾的安装包。
2. 安装包没有代码签名，遇到 SmartScreen 提示时点击“更多信息 → 仍要运行”。
3. 按当前用户安装，不需要管理员权限。默认安装到 `%LOCALAPPDATA%\Rummage`，可以改到普通文件夹（如 `D:\Apps\Rummage`）；不要装到 `C:\Program Files` 等需要管理员权限写入的位置。
4. 安装时默认勾选“添加到 PATH”，之后可以在终端中直接运行 `rummage-core`。

数据默认保存在 `<安装目录>\data` 中，可以在“设置 → 数据”中更改。新版本发布后，应用会提示更新。

## 🚀 首次使用

打开 Rummage 后按引导完成五步：

1. **选择文件夹**：要搜索的文件夹，至少一个。
2. **文件类型**：希望按内容查找的类型（文档、图片、音频、视频）。选了视频时再选识别方式：找物体、找动作或找说话内容。
3. **下载模型**：从 Hugging Face 下载（文字模型约 310 MB，图片与音视频模型约 555 MB），下载后校验。国内连接较慢时可以切换到镜像或设置代理，也可以导入已下载的模型文件。
4. **检测设备**：检测 CPU 和各块显卡能否正确计算以及速度，自动选择最快的；也可以只用 CPU。
5. **预估**：根据文件数量与大小预估建立索引的时间和占用空间。

完成后在后台建立索引。建立索引期间就可以搜索，已处理的文件随时能搜到。

## 🤖 命令行与 MCP

安装时勾选了“添加到 PATH”才能直接运行 `rummage-core`，否则写安装目录中 `rummage-core.exe` 的完整路径。后台服务没在运行时，命令行会自动启动它。

```
rummage-core search 京都酒店的收据 --type document --limit 10
rummage-core search 在草地上 --image "D:\照片\柯基.jpg"
rummage-core search 猫 --since 2026-01-01 --folder "D:\照片" --json
rummage-core context <命中 ID>
rummage-core status
rummage-core folders add "D:\资料" --exclude "*.tmp"
rummage-core settings
rummage-core pause --minutes 60
rummage-core failed retry
```

- 不带参数运行 `rummage-core` 查看全部命令与参数。
- `--json` 输出单行 JSON，便于脚本处理。退出码：0 有结果或操作成功，1 没有结果，2 出错。
- 不能通过命令行选择设备、下载或删除模型、清空索引、更改数据目录，请在 Rummage 中操作。

**MCP**：在 AI 客户端中配置运行 `rummage-core mcp`（stdio），例如 Claude Code：

```
claude mcp add --scope user rummage -- rummage-core mcp
```

提供 `search_files`、`get_hit_context`、`index_status` 以及管理文件夹、设置、暂停与失败文件的工具。其他客户端的配置方法与给 AI 用的使用说明（Skill）见安装目录中的 `skills\rummage\`，或仓库的 [`skills/rummage/`](skills/rummage/)。

## 🔒 数据与隐私

- 所有计算都在本机完成，文件内容、索引和搜索记录不会离开你的电脑。不收集使用数据。
- 只在下载模型和检查更新时联网。
- Rummage 不修改、不移动、不删除你的文件；所有数据保存在数据目录中，不写入被索引的文件夹。
- 移除文件夹只删除它的索引。卸载时可选择删除应用数据，被索引的文件夹任何情况下都不会被触碰。

## 🛠️ 从源码构建

需要 Node.js 22、Rust（1.92 以上）和 Tauri 的 Windows 构建环境，见 [Tauri 前置条件](https://v2.tauri.app/start/prerequisites/)。

```powershell
npm install
powershell -ExecutionPolicy Bypass -File scripts/fetch-llama.ps1
powershell -ExecutionPolicy Bypass -File scripts/fetch-ffmpeg.ps1
npm run tauri:dev
```

打包安装程序还需要：

```powershell
powershell -ExecutionPolicy Bypass -File scripts/fetch-llama.ps1 -Variant vulkan
npm run tauri:build
```

- 获取脚本下载 llama.cpp 预编译运行库与 LGPL 版 ffmpeg，校验钉定的 SHA-256 后放到 `.cache/third-party/`。
- `tauri:build` 需设置 `TAURI_SIGNING_PRIVATE_KEY`（私钥内容或路径）和 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`，用于给更新包签名。自行分发时用 `npx tauri signer generate` 生成自己的密钥，并替换 `src-tauri/tauri.conf.json` 中的 `plugins.updater.pubkey` 和更新地址。
- 开发规范见 [`AGENTS.md`](AGENTS.md)。

## 📄 许可

本项目基于 [MIT License](LICENSE) 开源。

- 模型 [EmbeddingGemma 2](https://huggingface.co/google/embeddinggemma-2) 以 Apache 2.0 许可发布，由用户下载，不随程序分发。
- 随程序分发的 llama.cpp（MIT）、FFmpeg（LGPL v3，以独立程序调用）、字体与其他依赖的许可见应用的“设置 → 关于 → 第三方许可”（清单原文为 [`src/assets/third-party-notices.txt`](src/assets/third-party-notices.txt)），FFmpeg 的源码获取方式见 [`third-party-licenses/FFmpeg-NOTICE.txt`](third-party-licenses/FFmpeg-NOTICE.txt)。
