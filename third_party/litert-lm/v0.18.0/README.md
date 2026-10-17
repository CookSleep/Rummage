# LiteRT-LM v0.18.0 头文件快照

Rust FFI（`crates/rummage-litert`）的签名以本目录为准，不要对照 main 分支：main 在 2026-10-01 把嵌入接口改成了“状态码 + 输出参数”，与 v0.18.0 不兼容。

- 来源仓库：<https://github.com/google-ai-edge/LiteRT-LM>
- 标签：`v0.18.0`
- 提交：`b2f686e2ed4718fb84ec398a61dd59ca0f0aff27`（2026-10-06）
- 协议：Apache 2.0，见同目录 `LICENSE`（取自同一提交的仓库根目录）。

## 文件

| 文件 | 来源 | 说明 |
|---|---|---|
| `c/embedding_engine.h` | 仓库 `c/embedding_engine.h` | 嵌入引擎接口 |
| `c/engine.h` | 仓库 `c/engine.h` | 输入数据、激活数据类型、日志级别等公共类型 |
| `c/api_export.h` | 仓库 `c/api_export.h` | 导出宏 |
| `c/error_reporter.h` | 仓库 `c/error_reporter.h` | 取得最近一次错误的代码和信息 |

各文件均未改动。

## 对应的二进制

C 接口动态库取自 PyPI `litert-lm-api==0.18.0` 的轮子（Windows 为 `litert-lm.dll`，Android 为 `liblitert-lm.so`），由 `scripts/fetch-litert.ps1` 下载、校验并取出，哈希钉定在该脚本中。
