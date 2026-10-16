# 开发脚本共用的下载与校验函数，由 fetch-*.ps1 以点源方式引入。
# 约束：兼容 Windows PowerShell 5.1；开发网络按流量计费，所有下载都写入 .part 并用 HTTP Range 断点续传，
# 校验 SHA-256 通过后才改名为正式文件，中断后再次运行只补下剩余部分。

$ErrorActionPreference = 'Stop'
# PowerShell 5.1 默认可能不启用 TLS 1.2，Hugging Face、GitHub 都要求它。
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$RepoRoot = Split-Path -Parent $PSScriptRoot

function Get-Sha256([string]$Path) {
  (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

# 文件存在且哈希（以及给定时的大小）正确时返回 $true。
function Test-FileHash([string]$Path, [string]$Sha256, [long]$Size = -1) {
  if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return $false }
  if ($Size -ge 0 -and (Get-Item -LiteralPath $Path).Length -ne $Size) { return $false }
  (Get-Sha256 $Path) -eq $Sha256.ToLowerInvariant()
}

function Format-Bytes([long]$Bytes) {
  if ($Bytes -ge 1GB) { return '{0:N2} GB' -f ($Bytes / 1GB) }
  if ($Bytes -ge 1MB) { return '{0:N1} MB' -f ($Bytes / 1MB) }
  '{0:N0} KB' -f ($Bytes / 1KB)
}

function Invoke-JsonGet([string]$Url) {
  $req = [Net.HttpWebRequest]::Create($Url)
  $req.UserAgent = 'Rummage-dev-scripts'
  $req.Timeout = 30000
  $resp = $req.GetResponse()
  try {
    $reader = New-Object IO.StreamReader($resp.GetResponseStream(), [Text.Encoding]::UTF8)
    $reader.ReadToEnd() | ConvertFrom-Json
  } finally {
    $resp.Close()
  }
}

# 断点续传下载到 $Dest.part，校验大小与 SHA-256 后改名为 $Dest。
# 网络中断与 5xx 时自动重试并从已下载的位置继续，4xx 直接失败；哈希不符时删除 .part（它是本脚本的临时文件）并报错。
function Invoke-VerifiedDownload([string]$Url, [string]$Dest, [long]$Size, [string]$Sha256) {
  $part = "$Dest.part"
  New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Dest) | Out-Null
  $maxAttempts = 20
  for ($attempt = 1; ; $attempt++) {
    $offset = 0
    if (Test-Path -LiteralPath $part) { $offset = (Get-Item -LiteralPath $part).Length }
    if ($offset -gt $Size) {
      Remove-Item -LiteralPath $part
      # 本次下载后仍超出说明下载源的文件与预期不符，再下载只会重复浪费流量。
      if ($attempt -gt 1) { throw "下载到的内容比预期的 $Size 字节大，下载源的文件可能已更换。已删除临时文件，请核实后重试。" }
      Write-Host "临时文件比预期大，重新下载。"
      $offset = 0
    }
    if ($offset -eq $Size) { break }
    # 服务器提前正常结束连接时不会进入 catch，这里同样限制总次数，避免无限循环。
    if ($attempt -gt $maxAttempts) { throw "下载失败（已重试 $maxAttempts 次），文件仍不完整。" }
    try {
      $req = [Net.HttpWebRequest]::Create($Url)
      $req.UserAgent = 'Rummage-dev-scripts'
      $req.Timeout = 60000
      $req.ReadWriteTimeout = 120000
      if ($offset -gt 0) {
        $req.AddRange([long]$offset)
        Write-Host ("从 {0} 处继续下载。" -f (Format-Bytes $offset))
      }
      $resp = $req.GetResponse()
      try {
        # 服务器忽略 Range 时会返回完整内容，只能从头写。
        $mode = [IO.FileMode]::Append
        if ($offset -gt 0 -and [int]$resp.StatusCode -ne 206) {
          Write-Host "服务器不支持断点续传，从头下载。"
          $mode = [IO.FileMode]::Create
          $offset = 0
        }
        $src = $resp.GetResponseStream()
        $out = New-Object IO.FileStream($part, $mode, [IO.FileAccess]::Write)
        try {
          $buf = New-Object byte[] (1MB)
          $done = $offset
          $nextReport = $done + 32MB
          while (($n = $src.Read($buf, 0, $buf.Length)) -gt 0) {
            $out.Write($buf, 0, $n)
            $done += $n
            if ($done -ge $nextReport) {
              Write-Host ("  {0} / {1}（{2:N1}%）" -f (Format-Bytes $done), (Format-Bytes $Size), ($done * 100.0 / $Size))
              $nextReport = $done + 32MB
            }
          }
        } finally {
          $out.Close()
          $src.Close()
        }
      } finally {
        $resp.Close()
      }
    } catch {
      # 4xx（文件不存在、无权访问等）重试也不会好，直接失败，调用方可以立即换下载源；网络错误与 5xx 仍重试。
      $webErr = $_.Exception.GetBaseException()
      if ($webErr -is [Net.WebException] -and $webErr.Response) {
        $code = [int]$webErr.Response.StatusCode
        if ($code -ge 400 -and $code -lt 500) { throw "下载失败：服务器返回 HTTP $code（$Url）。" }
      }
      if ($attempt -ge $maxAttempts) { throw "下载失败（已重试 $maxAttempts 次）：$($_.Exception.Message)" }
      Write-Host "下载中断：$($_.Exception.Message)。5 秒后续传（第 $attempt 次重试）。"
      Start-Sleep -Seconds 5
    }
  }
  $actual = Get-Sha256 $part
  if ($actual -ne $Sha256.ToLowerInvariant()) {
    Remove-Item -LiteralPath $part
    throw "SHA-256 校验失败：期望 $Sha256，实际 $actual。已删除临时文件，请检查下载源后重试。"
  }
  Move-Item -LiteralPath $part -Destination $Dest -Force
}
