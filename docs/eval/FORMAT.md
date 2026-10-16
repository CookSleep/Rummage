# 评测集格式

本文件是本地评测集的格式规范，`rummage-core eval <评测集目录>` 按它读取。

评测集放在仓库外（默认为仓库的同级目录 `Rummage-eval/`，构建脚本可用环境变量 `RUMMAGE_EVAL_ROOT` 指定）。其中的素材多数仅限研究或非商业使用，**不得提交进仓库，也不得分发**；结果 CSV 只放指标，不放素材内容。

## 目录结构

```
Rummage-eval/
  manifest.json      子集清单
  queries.jsonl      全部查询，每行一条
  files/<子集>/...   语料文件
  _downloads/        下载缓存与人工整理的查询（构建脚本的输入，不参与评测）
```

- `files/` 中是真实文件（`jpg` / `png` / `pdf` / `docx` / `md` / `txt` / `wav` / `mp3` / `m4a` / `mp4` 等），和用户磁盘上的文件一样，由正式流水线扫描、提取、建索引。
- 评测时**整个 `files/` 是一个混合库**：不同子集、不同模态的文件混在一起检索，跨模态混排正是要测的内容。也可以只索引 `files/<子集>/` 单独评某个子集。
- 文件名只用原数据集中的编号，不含描述文字，避免文件名关键词检索直接命中。
- 评测程序只读 `files/`、`manifest.json`、`queries.jsonl`，不修改其中任何文件（部分视频是 `_downloads/` 中文件的硬链接）。

## 编码与通用规则

- `manifest.json` 与 `queries.jsonl` 均为 UTF-8（无 BOM），换行为 LF；`queries.jsonl` 每行一个完整 JSON 对象，没有空行。
- 读取时忽略不认识的字段（同一 `format_version` 内只会追加可选字段）；缺少必填字段或类型不符时报错退出。
- 路径一律相对评测集根目录，用 `/` 分隔，不以 `/` 开头，不含 `..`；读取时按平台拼接。
- `files/` 中没有被任何查询引用的文件就是干扰项。

## manifest.json

```json
{
  "format_version": 1,
  "subsets": [
    {
      "name": "photo_xflickrco",
      "modality": "image",
      "description": "……",
      "source": "数据来源 URL 与说明",
      "license": "协议与使用限制",
      "query_lang": "zh",
      "query_origin": "human",
      "count_files": 600,
      "count_queries": 100
    }
  ]
}
```

除注明外，字段均为必填字符串。

| 字段 | 说明 |
|---|---|
| `format_version` | 整数，格式版本，当前为 `1`。格式有不兼容的改动时提升，评测程序遇到不认识的版本应报错退出 |
| `name` | 子集名，与 `files/` 下的目录名、查询的 `subset` 一致 |
| `modality` | 语料的模态：`image`、`document`、`audio`、`video` |
| `description` | 子集内容与考查点 |
| `source` | 数据来源 |
| `license` | 协议与使用限制 |
| `query_lang` | 查询语言：`zh`，可选子集 `doc_mmlongbench` 为 `en` |
| `query_origin` | 查询的来源，见下表 |
| `count_files` / `count_queries` | 整数，该子集的文件数与查询数（仅供核对） |
| `dropped` | 可选，字符串数组。构建时因源文件已被删除等原因剔除的源条目编号 |
| `notes` | 可选，字符串。其他说明 |

`query_origin` 的取值：

| 值 | 含义 |
|---|---|
| `human` | 人工撰写的中文 |
| `mt` | 机器翻译，未校对 |
| `mt-reviewed` | 机器翻译后逐条人工校对改写 |
| `llm` | 由大模型生成或改写 |
| `template` | 按模板生成 |

一个子集默认独占 `files/<name>/` 目录。若多个子集共用同一个语料目录，在 `notes` 中写明，并在各自的查询中引用同一批文件。

## queries.jsonl

每行一个 JSON 对象：

```json
{"id": "speech_fleurs-1518", "subset": "speech_fleurs", "query": "说到马尼拉有旅游大巴被劫持的那段", "relevant": [{"file": "files/speech_fleurs/recording_01.mp3", "start": 78.64, "end": 96.88}]}
```

| 字段 | 说明 |
|---|---|
| `id` | 全局唯一，格式为 `<子集>-<原数据集编号>` |
| `subset` | 所属子集 |
| `query` | 中文查询文本 |
| `query_image` | 可选。以图搜图时的查询图片，相对评测集根目录的路径 |
| `relevant` | 数组，相关项列表，至少一项 |

`relevant` 中每一项：

| 字段 | 说明 |
|---|---|
| `file` | 相对评测集根目录的路径，使用 `/` 分隔 |
| `grade` | 可选，整数。相关度 1–3，省略视为 1 |
| `start` / `end` | 可选，数字（可带小数）。音视频中的相关时间段，单位为秒，从文件开头算起；两者同时出现或同时省略，且 `start < end`。省略表示整个文件相关 |
| `page` | 可选，整数。文档中的相关页码（从 1 开始），省略表示整个文档相关 |
| `region` | 可选，两个整数的数组 `[上, 下]`。图片中相关内容的纵向像素区间（从图片顶端算起），省略表示整张图相关 |

- 一条查询可以有多个相关文件；同一文件有多个相关时间段时，写成多个 `file` 相同的相关项。
- 文件级指标：结果列表中出现任一相关文件即算命中。
- 时间段指标：命中的时间段与任一相关时间段有重叠即算命中（具体的重叠阈值由评测程序参数决定）。
- 相关项只标在查询所属子集内。混合库检索时，其他子集中也可能有确实相关的文件（例如“猫叫”的音频查询命中了有猫的视频），这类结果会被算作未命中，所以混合库的指标是**保守下界**。评测程序应同时报告两组指标：整个 `files/` 混合检索的指标，以及每个子集只在 `files/<子集>/` 内检索的指标。

## 当前子集

由 `scripts/eval/build_all.py` 从 `_downloads/` 构建，详细的抽样与转换方法见各 `build_*.py` 顶部的说明。

| 子集 | 模态 | 来源 | 文件 | 查询 | 查询来源 |
|---|---|---|---|---|---|
| `photo_xflickrco` | 照片 | xFlickrCO 中文（Flickr30K / COCO） | 600 | 100 | human |
| `screenshot_airbnb` | 截图 | JinaVDR Airbnb Synthetic 中文 | 300 | 80 | template |
| `docimage_shanghai` | 文档图像 | JinaVDR 上海总体规划 | 23 | 57 | human |
| `screenshot_long` | 长截屏 | 自制（HTML 排版，Edge 截图） | 70 | 83 | llm |
| `doc_mldr` | 文档 | MLDR 中文测试集，轮流存为 md / txt / docx / pdf | 240 | 80 | llm |
| `audio_audiocaps` | 声音事件 | AudioCaps 多语言版，存为 wav / mp3 / m4a | 150 | 50 | mt-reviewed |
| `speech_fleurs` | 说话内容 | FLEURS 普通话验证集，拼接成约 2 分钟的录音 | 14 | 50 | llm |
| `video_vatex` | 视频片段 | VATEX 中英双语版（无音轨） | 100 | 60 | human |
| `video_qvhighlights` | 视频时刻 | QVHighlights 验证集（带音轨） | 60 | 61 | mt-reviewed |
| `doc_mmlongbench`（可选） | 长 PDF（内嵌图表） | MMLongBench-Doc 的 PDF，问题用 V2 修正后的标注，英文原问题 | 135 | 863 | human |

- 可选子集默认不在 `build_all.py` 中构建，需要时在命令行指定；构建后与其他子集一样写入 `manifest.json`。

## 构建

```powershell
# 在仓库根目录；依赖 pyarrow、python-docx、reportlab（装在 .cache/venv/），PATH 中的 ffmpeg，以及本机的 Edge
.cache/venv/Scripts/python.exe -X utf8 scripts/eval/build_all.py              # 全部子集
.cache/venv/Scripts/python.exe -X utf8 scripts/eval/build_all.py speech_fleurs # 只重建指定子集
```

- 评测集根目录可用环境变量 `RUMMAGE_EVAL_ROOT` 指定。
- 缺少的源文件自动下载到 `_downloads/hf/<仓库>/<路径>`，已有的直接复用；`_downloads/` 不要删除。
- 抽样使用固定种子，给定同一份 `_downloads/` 重建出的 `manifest.json`、`queries.jsonl` 和文件内容相同；其中 `.docx` / `.pdf` 内嵌生成时间，字节不完全相同。
- 人工整理的查询（机器翻译校对、改写）在 `_downloads/curated/*.json` 中。它们派生自非商业协议的标注，因此不放进仓库。缺少时构建脚本会写出 `*.todo.json`（含原文）后退出，整理完再运行。
- ffmpeg 仅作为本机工具调用，不进入仓库或产品。
- `screenshot_long` 的页面文本在 `scripts/eval/long_pages.py` 中（自行撰写），用本机 Edge 无头模式截图（路径可用环境变量 `EDGE` 指定）。
- `doc_mldr` 的 PDF 嵌入 Windows 自带的黑体（`C:\Windows\Fonts\simhei.ttf`），因此构建只能在 Windows 上进行。黑体缺少的少数字符（如 `ä`、`²`）在 PDF 中显示为空白，提取不到。
