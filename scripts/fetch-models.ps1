# 下载 EmbeddingGemma 2 模型到开发版数据目录 src-tauri/dev-data/models/，两种格式：
# 1. GGUF Q8_0（ggml-org/embeddinggemma-2-GGUF，正式使用的模型，默认下载）：
#    主模型 embeddinggemma-2-Q8_0.gguf（分词器 + 文本骨干）与 mmproj-embeddinggemma-2-Q8_0.gguf（视觉 + 音频编码器），
#    存到 models/embeddinggemma-2-gguf/。-Gguf text 只取主模型（只处理文本时不需要 mmproj），-Gguf none 不取。
# 2. LiteRT-LM 通用版（-LiteRt，手机端的推理后端，在电脑上只用于评测）：litert-community/embeddinggemma-2-740m-litert-lm
#    的 embeddinggemma-2-740m.litertlm（不取芯片专用版）与 google/embeddinggemma-2 的 tokenizer.json（数切片长度用），
#    存到 models/embeddinggemma-2-litert/。
# 各仓库都钉定提交（resolve/<提交>/<路径>）：先调用 /api/models/<仓库>/tree/<提交>
# 核对文件大小和 lfs.oid（即 SHA-256）与内置已知表一致，接口不可用时直接按已知表。
# main 分支有新提交时只提示、不停止。-AllowUpdate 时该仓库的所有文件整体改用 main 当前指向的同一个提交
# （不混用两个提交），核实后需把该提交与新哈希写回脚本。
# 都用 HTTP Range 断点续传到 .part，校验 SHA-256 后改名；已存在且哈希正确时跳过。
# 模型文件下载后一直保留，不要在测试或清理脚本里删除。
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File scripts/fetch-models.ps1
#   powershell -ExecutionPolicy Bypass -File scripts/fetch-models.ps1 -Gguf text
#   powershell -ExecutionPolicy Bypass -File scripts/fetch-models.ps1 -Endpoint https://hf-mirror.com
#   powershell -ExecutionPolicy Bypass -File scripts/fetch-models.ps1 -Gguf none -LiteRt

param(
  # GGUF：all 为主模型 + mmproj（默认），text 只取主模型，none 不取。
  [ValidateSet('all', 'text', 'none')][string]$Gguf = 'all',
  # 另取 LiteRT-LM 模型与分词器。
  [switch]$LiteRt,
  # Hugging Face 或接口兼容的镜像地址，例如 https://hf-mirror.com。
  [string]$Endpoint = 'https://huggingface.co',
  # 改用 main 分支当前指向的提交下载（默认按钉定的提交）。
  [switch]$AllowUpdate
)

. (Join-Path $PSScriptRoot 'common.ps1')

# GGUF 已知表：从提交 bfcd298… 的文件列表读取（与黄金向量元数据中的哈希相同）。
# Modal 表示属于哪个下载段：text 为主模型，media 为 mmproj。
$GgufRepo = 'ggml-org/embeddinggemma-2-GGUF'
$GgufCommit = 'bfcd298762cc34d0357ece5ebdd31791a3a374d8'
$GgufDir = 'embeddinggemma-2-gguf'
$GgufFiles = @(
  @{ Path = 'embeddinggemma-2-Q8_0.gguf'; Modal = 'text'; Size = 309855456; Sha256 = '2188ac1deca4b77dffefd603c2776a9d76d9d74ec01841392982ebb840b09135' }
  @{ Path = 'mmproj-embeddinggemma-2-Q8_0.gguf'; Modal = 'media'; Size = 554821024; Sha256 = 'c4a8a52691ecef40618438928bdf9e68379b854e24166f292592353db0aab64f' }
)

# LiteRT-LM 已知表：从两个提交的文件列表读取（与 rummage_embed::space 的 LITERT_MODEL、TOKENIZER 相同）。
$LiteRtDir = 'embeddinggemma-2-litert'
$LiteRtModelRepo = 'litert-community/embeddinggemma-2-740m-litert-lm'
$LiteRtModelCommit = '24d962e906c7d332c6428e71c9676855024569e2'
$LiteRtModelFiles = @(
  @{ Path = 'embeddinggemma-2-740m.litertlm'; Size = 484622336; Sha256 = 'e7a8a2204b91e0f96e92960e84a09a89212e1633dcb7575a9bf3378b4df77f4c' }
)
$TokenizerRepo = 'google/embeddinggemma-2'
$TokenizerCommit = '914f7f89142e33e77833254d9c9b90c3cef7303b'
$TokenizerFiles = @(
  @{ Path = 'tokenizer.json'; Size = 32170510; Sha256 = '4d777ef5bdc1aa36227abdfb77c3e49e7b9c892d16e1b6bda41c393504828be4' }
)

# git blob SHA-1："blob <长度>\0" + 内容。用于核对不走 LFS 的小配置文件。
function Get-GitBlobSha1([byte[]]$Content) {
  $header = [Text.Encoding]::ASCII.GetBytes("blob $($Content.Length)`0")
  $sha1 = New-Object Security.Cryptography.SHA1Managed
  -join ($sha1.ComputeHash([byte[]]($header + $Content)) | ForEach-Object { $_.ToString('x2') })
}

# 从钉定提交（-AllowUpdate 时为 main 当前指向的提交）下载 $Files 到 models/$Dir/，结果追加到 $script:ready。
# 模型哈希参与向量空间 ID，远端更新时默认不跟随，避免静默换模型和白花流量。
function Sync-PinnedFiles([string]$Label, [string]$Repo, [string]$Commit, [string]$Dir, $Files) {
  # 所有文件都取自同一个提交：默认是钉定的提交；-AllowUpdate 时整体换成 main 当前指向的提交，不混用。
  $ref = $Commit
  $mainSha = $null
  try {
    $mainSha = (Invoke-JsonGet "$base/api/models/$Repo/revision/main").sha
  } catch {
    Write-Warning "无法读取 $Repo main 分支的提交（$($_.Exception.Message)）。"
  }
  if ($AllowUpdate) {
    if (-not $mainSha) { throw '已指定 -AllowUpdate，但读不到 main 分支的提交，无法确定要下载的版本。已停止。' }
    $ref = $mainSha
    if ($ref -ne $Commit) {
      Write-Warning "已指定 -AllowUpdate，$Repo 改按 main 分支的提交 $ref 下载（钉定的是 $Commit）。核实后把脚本中的钉定提交改为该提交，并按下方输出更新已知表。"
    }
  } elseif ($mainSha -and $mainSha -ne $Commit) {
    Write-Warning "$Repo 的 main 分支已有新提交 $mainSha，本次仍按钉定的提交 $Commit 下载。要跟随更新，核实后加 -AllowUpdate。"
  }
  Write-Host "== $Label：$Repo@$($ref.Substring(0, 7))"
  # 接口只用来核对该提交的文件与已知表是否一致；读不到时（仅限钉定的提交）直接按已知表。
  $remoteByPath = @{}
  try {
    $tree = Invoke-JsonGet "$base/api/models/$Repo/tree/$($ref)?recursive=true"
    foreach ($e in @($tree | Where-Object { $_.type -eq 'file' })) { $remoteByPath[$e.path] = $e }
  } catch {
    if ($ref -ne $Commit) { throw "无法读取提交 $ref 的文件列表（$($_.Exception.Message)），没有可用于校验的哈希。已停止。" }
    Write-Warning "无法读取文件列表（$($_.Exception.Message)），改用内置已知哈希表。"
  }
  $root = Join-Path $modelsDir $Dir
  foreach ($f in $Files) {
    $dest = Join-Path $root ($f.Path -replace '/', '\')
    $bytes = [long]$f.Size
    $sha = $f.Sha256
    $gitOid = $null
    $r = $remoteByPath[$f.Path]
    if ($remoteByPath.Count -gt 0 -and -not $r) { throw "提交 $ref 中没有 $($f.Path)，模型仓库可能已调整。已停止，请核实后更新脚本中的已知表。" }
    if ($r) {
      $changed = [long]$r.size -ne $bytes
      if ($f.GitOid) { $changed = $changed -or $r.oid -ne $f.GitOid -or $r.lfs } else { $changed = $changed -or -not $r.lfs -or $r.lfs.oid -ne $sha }
      if ($changed) {
        $remoteId = if ($r.lfs) { $r.lfs.oid } else { "git $($r.oid)" }
        $knownId = if ($f.GitOid) { "git $($f.GitOid)" } else { $sha }
        $msg = "提交 $ref 中的文件与内置已知表不同。`n  已知表：$($f.Path)  $bytes  $knownId`n  远端：  $($f.Path)  $($r.size)  $remoteId"
        # 钉定的提交内容不会变，对不上说明已知表写错或下载源有问题，不能继续。
        if ($ref -eq $Commit) { throw "$msg`n钉定的提交不应变化，请检查已知表或下载源。已停止。" }
        Write-Warning "$msg`n按该提交的结果下载，核实后写入脚本中的已知表。"
        $bytes = [long]$r.size
        # 非 LFS 文件事先不知道 SHA-256，按 git blob SHA-1 校验。
        if ($r.lfs) { $sha = $r.lfs.oid } else { $sha = $null; $gitOid = $r.oid }
      }
    }

    if ($sha -and (Test-FileHash $dest $sha $bytes)) {
      Write-Host "已存在且校验通过，跳过：$($f.Path)"
      $script:ready += "  $Dir/$($f.Path)  $bytes  $sha"
      continue
    }
    if (-not $sha -and (Test-Path -LiteralPath $dest -PathType Leaf) -and (Get-GitBlobSha1 ([IO.File]::ReadAllBytes($dest))) -eq $gitOid) {
      $sha = Get-Sha256 $dest
      Write-Host "已存在且校验通过（git $gitOid），跳过：$($f.Path)"
      Write-Warning "$($f.Path) 的新 SHA-256 为 $sha，请写入脚本中的已知表。"
      $script:ready += "  $Dir/$($f.Path)  $bytes  $sha"
      continue
    }
    $url = "$base/$Repo/resolve/$ref/$($f.Path)"
    Write-Host ("下载 {0}（{1}）……" -f $f.Path, (Format-Bytes $bytes))
    if ($sha) {
      Invoke-VerifiedDownload $url $dest $bytes $sha
    } else {
      # 小配置文件（几 KB）直接整个下载，核对 git blob SHA-1（"blob <长度>\0" + 内容）后再改名。
      New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dest) | Out-Null
      $req = [Net.HttpWebRequest]::Create($url)
      $req.UserAgent = 'Rummage-dev-scripts'
      $req.Timeout = 60000
      $resp = $req.GetResponse()
      try {
        $ms = New-Object IO.MemoryStream
        $resp.GetResponseStream().CopyTo($ms)
        $content = $ms.ToArray()
      } finally {
        $resp.Close()
      }
      $actual = Get-GitBlobSha1 $content
      if ($content.Length -ne $bytes -or $actual -ne $gitOid) { throw "$($f.Path) 校验失败：期望 $bytes 字节、git $gitOid，实际 $($content.Length) 字节、git $actual。" }
      [IO.File]::WriteAllBytes($dest, $content)
      $sha = Get-Sha256 $dest
      Write-Warning "$($f.Path) 已按远端更新，新的 SHA-256 为 $sha，请写入脚本中的已知表。"
    }
    Write-Host "下载完成，SHA-256 校验通过：$($f.Path)"
    $script:ready += "  $Dir/$($f.Path)  $bytes  $sha"
  }
}

if ($Gguf -eq 'none' -and -not $LiteRt) { throw '没有要下载的模型：-Gguf none 时请用 -LiteRt。' }

$base = $Endpoint.TrimEnd('/')
$modelsDir = Join-Path $RepoRoot 'src-tauri\dev-data\models'
New-Item -ItemType Directory -Force -Path $modelsDir | Out-Null
$script:ready = @()

if ($Gguf -ne 'none') {
  $want = @($GgufFiles | Where-Object { $Gguf -eq 'all' -or $_.Modal -eq 'text' })
  Sync-PinnedFiles "GGUF Q8_0（$(if ($Gguf -eq 'all') { '主模型 + mmproj' } else { '只取主模型' })）" $GgufRepo $GgufCommit $GgufDir $want
}

if ($LiteRt) {
  Sync-PinnedFiles 'LiteRT-LM 通用版' $LiteRtModelRepo $LiteRtModelCommit $LiteRtDir $LiteRtModelFiles
  Sync-PinnedFiles 'LiteRT-LM 分词器' $TokenizerRepo $TokenizerCommit $LiteRtDir $TokenizerFiles
}

Write-Host "模型就绪（$modelsDir）："
$script:ready | ForEach-Object { Write-Host $_ }
