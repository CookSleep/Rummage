# 黄金样本与黄金向量

用于后端自检与跨后端兼容性测试：在设备上用某个“后端 × 模态 × 精度”组合重算这些样本，与这里的黄金向量逐条比较余弦相似度和前 10 重合率。

## 来源与授权

全部图片、音频和视频素材都由 AI 于 2026-10 生成，不含第三方版权素材，也不含官方 demo 素材或私人文件；照片与视频的提示词都要求不出现人脸，图片提示词要求不出现品牌标志和水印：

- 图片 5 张：`gpt-image-2.5-sunburst`。
- 音频 3 段、视频窗口 1 个：`doubao-seedance-2.5`（生成带音频的短视频；音频样本取自生成视频的音轨，视频窗口取自生成视频的画面与音轨）。

每个素材的生成模型、生成时间、参数和完整提示词记录在 `manifest.json` 对应条目的 `source` 字段中；朗读类音频和带解说的视频另有逐字稿（`transcript`），逐字稿已用 `gemini-3.8-flash` 转写核对，与实际朗读一致。查询与文档文本由开发者编写。本目录随仓库按 MIT 协议分发（见仓库根目录 `LICENSE`）。

## 文件

| 路径 | 内容 |
|---|---|
| `manifest.json` | 样本清单：每条文本的原文与加前缀后的输入、每张图片 / 每段音频的说明与生成来源、视频窗口的输入序列，以及所有素材文件的 SHA-256 |
| `images/` | 5 张图片：照片 2 张（JPEG），界面截图、图表、长截图各 1 张（PNG） |
| `audio/` | 3 段音频，16 kHz 单声道 16 位 PCM WAV，5–10 秒：中文朗读、英文朗读、环境声各 1 段 |
| `video/vid01/` | 1 个短视频窗口的素材：画面帧（640×360 JPEG）、逐帧对应的音频段（WAV）、`window.json`（窗口参数）、`transcript.txt`（有解说时的逐字稿） |
| `vectors-gguf-q8-<主模型 SHA-256 前 16 位>.json` | 黄金向量，见下文 |
| `THIRD_PARTY.md` | 第三方素材声明（本目录没有第三方素材） |

样本（全部素材文件加 `manifest.json`）合计 5,906,247 字节（5.63 MiB）：图片 4.7 MB、音频 0.8 MB、视频窗口 0.4 MB、清单 0.02 MB。导入时照片转为 JPEG 质量 90，截图、图表与长截图保留 PNG 只做无损重压缩，音频与视频窗口原样复制。黄金向量按模型文件另行打包，不计入样本体积。

### 文本（15 条）

9 条查询、6 条文档，中英文混合。每条文本的 `target` 是它对应的媒体样本编号：9 个媒体样本各有一条查询（`q01`–`q09`，按 `pairs` 顺序）；小票照片、截图、图表、长截图、英文朗读和视频窗口另有一条描述其内容的文档（`d01`–`d06`），用于计算“文本查询检索文本 + 图像库”的前 10 重合率。按文本前缀方案 A（与正式索引相同）加前缀：

- 查询：`task: search result | query: {查询}`
- 文档：`title: none | text: {内容}`（文档没有标题）

### 图片（5 张）

| 文件 | 尺寸 | 内容 |
|---|---|---|
| `img02_photo_corgi.jpg` | 1536×1152 | 照片：公园草地上叼着红色飞盘奔跑的柯基 |
| `img08_photo_receipt.jpg` | 1152×1536 | 照片：便利店热敏纸小票（中文，含二维码） |
| `img14_ui_error_en.png` | 1920×1200 | 桌面截图（英文）：同步失败，磁盘空间不足的错误对话框 |
| `img22_chart_bar_zh.png` | 1536×1072 | 图表（中文）：上半年门店月客流量柱状图 |
| `img28_long_chat_zh.png` | 1280×3840 | 长截图（中文，文字密集）：读书会群聊记录 |

照片用 JPEG、其余用 PNG，覆盖两种解码路径。长截图在这里整张送入引擎（引擎会缩放到块数上限以内），只用于数值比较；正式索引中长图先切片再分别编码。

### 音频（3 段）

| 文件 | 时长 | 内容 |
|---|---|---|
| `aud01_zh.wav` | 9.66 秒 | 中文朗读：天气提醒（带伞、添衣） |
| `aud07_en.wav` | 9.18 秒 | 英文朗读：刚烤好面包 |
| `aud09_env.wav` | 5.00 秒 | 环境声：雨打窗户 |

逐字稿见 `manifest.json` 的 `transcript`；环境声没有人声，`transcript` 为 `null`。

### 视频窗口（1 个）

与正式索引中视频窗口的输入格式相同，不保存视频文件，只保存构造输入所需的素材和清单：

| 编号 | 窗口 | 帧数 | 画面 | 音频 |
|---|---|---|---|---|
| `vid01` | 00:01 起 6 秒 | 6 | 揉面、切剂子、擀饺子皮（只拍到手） | 中文女声解说 |

- 抽帧：窗口内均匀分布、首尾各一帧，640×360 JPEG；每帧 70 个视觉 token。帧时刻见 `window.json` 与清单的 `frame_times`。
- 音频：窗口音频按帧数等分，每帧配一段（每段 1 秒）。
- `manifest.json` 中每个窗口有两种输入序列：
  - `inputs_av`（带音频）：`时间戳文字, 音频段, 时间戳文字, 画面帧` 依次重复，时间戳为窗口起点加帧时刻取整秒的 `mm:ss`，不加任务前缀；
  - `inputs_v`（不带音频）：只放画面帧，不加时间戳。
- 两种序列分别对应子空间 `video_av` / `video_v`，黄金向量中记为 `vid01_av`、`vid01_v`。

## 黄金向量

文件 `vectors-gguf-q8-2188ac1deca4b77d.json`，由 `scripts/golden/golden_vectors.py` 计算：

- 运行库：llama.cpp **b11461** 官方纯 CPU 预编译包 `llama-b11461-bin-win-cpu-x64.zip`（Python ctypes 调 `llama.dll` + `mtmd.dll`），设备列表只有 `CPU`。
- 模型：`ggml-org/embeddinggemma-2-GGUF` 提交 `bfcd298762cc34d0357ece5ebdd31791a3a374d8` 的 Q8_0 主模型 `embeddinggemma-2-Q8_0.gguf`（SHA-256 `2188ac1deca4b77d…`，文件名中的 16 位即其前缀）与 Q8_0 mmproj `mmproj-embeddinggemma-2-Q8_0.gguf`（SHA-256 `c4a8a52691ecef40…`）。
- 推理：文本、视觉、音频都在 CPU 上；**Flash Attention 显式关闭**（llama 上下文与 mtmd 均设为 DISABLED，默认的 AUTO 会把注意力的 K、V 转成 F16）；非因果注意力，平均池化后 L2 归一化；6 线程；单个输入最多 2048 token；图像 140 档视觉 token（视频帧 70）；文本前缀方案 A（见上文“文本”）。
- 共 25 条：文本 15、图像 5、音频 3、视频窗口 2（1 个窗口 × 带 / 不带音频）。

格式为单行 UTF-8 JSON（约 0.27 MB）：

| 字段 | 含义 |
|---|---|
| `format` | 向量编码的文字说明 |
| `runtime`、`runtime_package_sha256`、`runtime_dll_sha256` | 运行库版本、预编译包与 `llama.dll`、`mtmd.dll`、`ggml.dll`、`ggml-base.dll` 的 SHA-256 |
| `gguf_source` | GGUF 的来源仓库与提交 |
| `model_file`、`model_sha256`、`mmproj_file`、`mmproj_sha256` | 主模型与 mmproj 的文件名和完整 SHA-256（向量文件名中的 16 位是主模型哈希的前缀） |
| `backend`、`devices` | 文本 / 视觉 / 音频后端，均为 `cpu`；加载后枚举到的设备（只有 `CPU`） |
| `precision` | 权重 Q8_0；CPU 后端把矩阵乘的激活按权重类型量化为 Q8_0，其余运算 F32，不使用 F16 |
| `flash_attn` | `off`（显式关闭） |
| `pooling`、`threads`、`max_tokens` | 平均池化、线程数 6、单个输入上限 2048 token |
| `vision_tokens_per_image`、`video_vision_tokens_per_image` | 每图 140 视觉 token、视频帧 70 |
| `prefix_scheme` | 文本前缀方案 `A` |
| `manifest_sha256` | 计算时 `manifest.json` 的 SHA-256，用来发现样本已改而向量未重算 |
| `engine_init_seconds` | 引擎初始化耗时（仅供参考） |
| `vectors` | 25 项，每项 `{id, modality, norm, seq_len, vector}`；`modality` 为 `text`、`image`、`audio`、`video_av`、`video_v`；`vector` 是 768 个 float32，以 9 位有效数字的十进制写出（可无损还原为 float32），已 L2 归一化；`norm` 是写出前的范数；`seq_len` 是送入编码器的 token 数（含软 token） |

全部向量已检查无 NaN / Inf、范数与 1 相差不超过 10⁻⁶；用同一运行库在 CPU 上重算复核（`golden_vectors.py --check`），25 条与文件中的值逐位相同。

## 重算黄金向量

```powershell
.cache\venv\Scripts\python.exe -m pip install pillow==12.3.0   # 仅首次
.cache\venv\Scripts\python.exe -I scripts\golden\golden_vectors.py
.cache\venv\Scripts\python.exe -I scripts\golden\golden_vectors.py --check   # 只复核，不改文件
```

- 推理只用 CPU：`golden_vectors.py` 只加载 `scripts/fetch-llama.ps1` 取出的纯 CPU 预编译包，枚举到非 CPU 设备即中止。
- **样本一旦改动，必须重算黄金向量**，`golden_vectors.py` 会先核对 `manifest.json` 中的文件哈希。
- 黄金向量只对应本目录中的这一份样本，并按“模型文件 SHA-256 → 向量”打包：换模型文件时新增一个 `vectors-gguf-<量化>-<主模型 SHA-256 前 16 位>.json`。
