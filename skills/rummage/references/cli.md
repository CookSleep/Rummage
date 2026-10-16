# rummage-core 命令行参考

只有安装时勾选了“添加到 PATH”才能直接输入 `rummage-core`（改了 PATH 后要重新打开终端），否则用安装目录中 `rummage-core.exe` 的完整路径。

所有子命令都可以加 `--data-dir <目录>` 指定数据目录（一般不需要，默认用 Rummage 当前的数据目录）。`-h` / `--help` 显示用法。

## search

```
rummage-core search [<查询>] [--image <图片>] [--type <类型,…>] [--folder <文件夹>]
                    [--since <日期>] [--until <日期>] [--limit <数量>] [--include-weak] [--json]
```

| 参数 | 说明 |
|---|---|
| `<查询>` | 描述或关键词，可以不加引号直接写多个词。没有 `--image` 时必填 |
| `--image <图片>` | 以图搜图，本地图片路径（相对路径按当前目录）；会读取这张图片，但只用于计算查询向量。可同时给出查询文字作为补充描述 |
| `--type` | `document`、`image`、`audio`、`video`、`other`（只按文件名索引的文件），逗号分隔，可重复 |
| `--folder` | 已索引的文件夹之一（`status` 中列出的），不能是子文件夹 |
| `--since` / `--until` | `YYYY-MM-DD`，本地时间，包含当天 |
| `--limit` | 1–500，默认 20 |
| `--include-weak` | 同时显示匹配度较低的结果 |
| `--json` | 输出 JSON |

文本输出：每个结果一段，首行是序号、文件名、类型和匹配度（有校准分数时），下面是路径，修改日期、大小、匹配方式，“位置：”行（页码、时间段等），文档结果还有“命中 ID”和一段片段。

`--json` 输出一行：

```json
{"protocol":1,"hits":[…],"tookMs":12,"partial":false,"semanticUnavailable":false,"unavailable":null,"weakOmitted":0}
```

- `hits[]` 的常用字段：`path`、`name`、`kind`、`size`（字节）、`mtime`（Unix 毫秒）、`matchPct`（0–100，可能为 null）、`lowRelevance`、`cloud`（仅在云端）、`offline`（设备未连接）、`segments[]`（`segmentId`、`locatorJson`）、`nameHit`、`textSegments`、`snippet`（`segmentId` 即命中 ID，`text`）。
- `partial`：索引尚未完成，结果可能不完整。
- `semanticUnavailable` / `unavailable`：内容识别不可用及原因（`paused`、`no_model`、`images_off`、`no_mmproj`、`engine`、`query_too_long`），此时只有文件名和正文原词的匹配。
- `weakOmitted`：没有列出的匹配度较低的结果，至少这么多。

## status

```
rummage-core status [--json]
```

显示版本、首次设置是否完成、索引状态与暂停原因、进度（各类型）、失败数、推理设备、已索引的文件夹。
JSON：`{"protocol":1,"coreVersion":…,"onboarded":…,"index":{…},"roots":[{"id","path","offline","files","excludes"}]}`。

## context

```
rummage-core context <命中 ID> [--json]
```

输出命中所在文件、位置，以及已索引的前文、命中段、后文。命中已失效（文件重新建立过索引）或没有文字内容时退出码为 1。
JSON：`{"protocol":1,"file":{…}|null,"segment":{"fileId","locatorJson","text","before","after"}}`。

## folders

```
rummage-core folders [--json]
rummage-core folders add <文件夹> [--types <类型,…>] [--exclude <规则>]… [--json]
rummage-core folders set <文件夹> [--types <类型,…>] [--exclude <规则>]… [--no-exclude] [--json]
rummage-core folders remove <文件夹> [--json]
```

| 参数 | 说明 |
|---|---|
| `<文件夹>` | 相对路径按当前目录。`add` 要求是存在的本地文件夹，不能与已索引的文件夹互相包含；`set`、`remove` 要求是已索引的文件夹 |
| `--types` | 识别内容的文件类型：`document`、`image`、`audio`、`video`，逗号分隔。所有文件夹共用这一设置，未列出的类型只按文件名索引 |
| `--exclude` | 一条排除规则，写法同 `.gitignore`（`node_modules` 匹配任意层级，`/草稿` 只匹配文件夹根下，`*.tmp` 按通配符），可重复。`set` 时整体替换原有的规则 |
| `--no-exclude` | 清空排除规则（Windows PowerShell 5.1 不会把空字符串参数传给程序，不要用 `--exclude ""`） |

`folders` 列出识别内容的文件类型、索引进度和每个文件夹的文件数与排除规则；没有已索引的文件夹时退出码为 1。`remove` 只移除索引，不删除文件。
JSON：`folders` 为 `{"protocol":1,"kinds":[…],"roots":[{"id","path","offline","files","excludes"}],"index":{…}|null}`；`add`、`set` 为 `{"protocol":1,"message":…,"root":{…}}`，`remove` 为 `{"protocol":1,"message":…}`。

## settings

```
rummage-core settings [<键>] [--json]
rummage-core settings set <键> <值> [--json]
```

列出参数的键、当前值、默认值、允许范围或选项、是否修改后重建索引，以及名称与说明。值的写法：开关写 `true` / `false`；有名称的选项写数值或名称（如 `settings set video_preset 找动作`）。超出范围时退出码为 2。
改了标明“修改后重建索引”的参数后，Rummage 在后台重建受影响类型的索引，期间旧索引仍可搜索。
JSON：`settings` 为 `{"protocol":1,"settings":[{"key","group","section","kind","default","affectsIndex","indexKinds","label","help","basis","value"}]}`；`settings set` 为 `{"protocol":1,"key","value","changed","rebuild":[…],"message"}`。

设备选择、模型下载与删除、清空索引、数据目录不能在命令行中修改。

## pause、resume

```
rummage-core pause [--minutes <分钟>] [--json]
rummage-core resume [--json]
```

暂停建立索引，`--minutes` 为 1–10080，到时自动恢复；不给时一直暂停到 `resume`。暂停期间照常可以搜索，重新启动 Rummage 后仍然有效。`resume` 只取消手动暂停，使用电池、全屏、磁盘空间不足时的自动暂停仍按设置生效。JSON：`{"protocol":1,"message":…}`。

## failed

```
rummage-core failed [--json]
rummage-core failed retry [--json]
```

`failed` 列出最近 100 个建立索引失败的文件与原因，以及总数和其中能重试的数量（加密文档不能重试）；没有失败的文件时退出码为 1。
`failed retry` 把能重试的全部重新排队；没有能重试的时退出码为 1。
JSON：`failed` 为 `{"protocol":1,"total","retryable","files":[{"fileId","name","path","kind","reason","failedAt","retryable"}]}`；`failed retry` 为 `{"protocol":1,"message":…}`。

## 退出码

| 码 | 含义 |
|---|---|
| 0 | 有结果，或操作成功 |
| 1 | 没有结果、命中已失效、没有文字内容，或没有已索引的文件夹、没有失败的文件 |
| 2 | 出错：参数不对、不是 Rummage 的数据目录、连不上后台服务、请求被拒绝；原因写在 stderr |

## 编码

- 控制台中直接显示中文没有问题。
- 输出被管道或变量捕获时是 UTF-8。Windows PowerShell 5.1 默认按系统代码页解码，捕获文本前先执行 `[Console]::OutputEncoding = [Text.Encoding]::UTF8`；PowerShell 7 与 Git Bash 不需要。
- `--json` 只含 ASCII（中文写成 `\uXXXX`），`ConvertFrom-Json`、`jq`、Python `json` 都能直接还原。
