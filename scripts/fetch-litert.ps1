# 获取 LiteRT-LM C 接口运行库（rummage-litert 在运行时加载）。
# 来源是 PyPI 官方包 litert-lm-api 0.18.0 的二进制轮子：通过 PyPI JSON API 找到对应平台的轮子，
# 下载到 .cache/third-party/，用下方钉死的 SHA-256 校验，再取出动态库到
# .cache/third-party/litert-lm/0.18.0/<平台>/ 并逐个校验。
# 幂等：目标库文件都已存在且哈希正确时直接结束，不访问网络；轮子已缓存且哈希正确时不再下载。
# 升级运行库时同时更新这里的哈希、third_party/litert-lm/v<版本>/ 的头文件快照和 FFI 签名。
#
# GPU 安全：只取 litert-lm.dll，不取 WebGPU 用的 dxcompiler.dll、dxil.dll 与 NPU 的 dispatch 库；
# LiteRT 的 WebGPU 后端会让外接显卡驱动反复超时重置，电脑上只用 CPU 后端。
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File scripts/fetch-litert.ps1
#   powershell -ExecutionPolicy Bypass -File scripts/fetch-litert.ps1 -Platform android_arm64

param(
  [ValidateSet('windows_x86_64', 'android_arm64')]
  [string]$Platform = 'windows_x86_64'
)

. (Join-Path $PSScriptRoot 'common.ps1')

$Package = 'litert-lm-api'
$Version = '0.18.0'

# 轮子哈希取自 PyPI JSON API 的 digests；库文件哈希取自本机解压后的计算结果，
# 并与轮子内 RECORD 中记录的哈希一致（Android 的 .so 哈希即取自其轮子的 RECORD）。
$Targets = @{
  'windows_x86_64' = @{
    Wheel = 'litert_lm_api-0.18.0-py3-none-win_amd64.whl'
    WheelSize = 26403380
    WheelSha256 = 'eb02dc5d0fc6a894a664cfcc63e3aaf3100973ba2c9a6a7eebf4df8749a01038'
    Files = @(
      @{ Entry = 'litert_lm/litert-lm.dll'; Name = 'litert-lm.dll'; Size = 50051584; Sha256 = 'c6421334f409a5b4b50b32e7925d6eb9d7e62245d00a6b01a7541596c8a1a1e3' }
    )
  }
  'android_arm64' = @{
    Wheel = 'litert_lm_api-0.18.0-py3-none-android_23_arm64_v8a.whl'
    WheelSize = 14243327
    WheelSha256 = 'a045af401dc199c35b1f44c29e8899cecbf2712a8194c9e35489ab5e2a95d47d'
    Files = @(
      @{ Entry = 'litert_lm/liblitert-lm.so'; Name = 'liblitert-lm.so'; Size = 39860448; Sha256 = '3d591b09116e7be3ab6b2b5b1a27bb3079dda0eb66550271533e6aee19e2dfd9' }
    )
  }
}

$target = $Targets[$Platform]
$cacheDir = Join-Path $RepoRoot '.cache\third-party'
$outDir = Join-Path $cacheDir "litert-lm\$Version\$Platform"
$wheelPath = Join-Path $cacheDir $target.Wheel

$missing = @($target.Files | Where-Object { -not (Test-FileHash (Join-Path $outDir $_.Name) $_.Sha256 $_.Size) })
if ($missing.Count -eq 0) {
  Write-Host "LiteRT-LM $Version（$Platform）的运行库已就绪，无需下载：$outDir"
  foreach ($f in $target.Files) { Write-Host ("  {0}  {1}  {2}" -f $f.Name, $f.Size, $f.Sha256) }
  exit 0
}

if (Test-FileHash $wheelPath $target.WheelSha256 $target.WheelSize) {
  Write-Host "使用已缓存的轮子：$wheelPath"
} else {
  Write-Host "查询 PyPI 上 $Package $Version 的文件列表……"
  $meta = Invoke-JsonGet "https://pypi.org/pypi/$Package/$Version/json"
  $entry = $meta.urls | Where-Object { $_.filename -eq $target.Wheel }
  if (-not $entry) { throw "PyPI 上没有找到 $($target.Wheel)，请确认版本号和平台。" }
  if ($entry.digests.sha256 -ne $target.WheelSha256) {
    throw "PyPI 公布的哈希（$($entry.digests.sha256)）与脚本钉定的哈希不一致，已停止。轮子可能被替换，请人工核实。"
  }
  Write-Host ("下载 {0}（{1}）……" -f $target.Wheel, (Format-Bytes $target.WheelSize))
  Invoke-VerifiedDownload $entry.url $wheelPath $target.WheelSize $target.WheelSha256
  Write-Host "轮子下载完成，SHA-256 校验通过。"
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$zip = [IO.Compression.ZipFile]::OpenRead($wheelPath)
try {
  foreach ($f in $missing) {
    $zipEntry = $zip.GetEntry($f.Entry)
    if (-not $zipEntry) { throw "轮子中没有 $($f.Entry)。" }
    $dest = Join-Path $outDir $f.Name
    $tmp = "$dest.part"
    [IO.Compression.ZipFileExtensions]::ExtractToFile($zipEntry, $tmp, $true)
    $actual = Get-Sha256 $tmp
    if ($actual -ne $f.Sha256) {
      Remove-Item -LiteralPath $tmp
      throw "$($f.Name) 的 SHA-256 校验失败：期望 $($f.Sha256)，实际 $actual。"
    }
    Move-Item -LiteralPath $tmp -Destination $dest -Force
    Write-Host "已取出并校验 $($f.Name)。"
  }
} finally {
  $zip.Dispose()
}

Write-Host "LiteRT-LM $Version（$Platform）的运行库已就绪：$outDir"
foreach ($f in $target.Files) { Write-Host ("  {0}  {1}  {2}" -f $f.Name, $f.Size, $f.Sha256) }
