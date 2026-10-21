# 获取 LiteRT-LM C 接口运行库（rummage-litert 在运行时加载）。
# 来源是 PyPI 官方包 litert-lm-api 0.18.0 的二进制轮子：通过 PyPI JSON API 找到对应平台的轮子，
# 下载到 .cache/third-party/，用下方钉死的 SHA-256 校验，再取出动态库到
# .cache/third-party/litert-lm/0.18.0/<平台>/ 并逐个校验。
# Android 的库放在 android_arm64/arm64-v8a/（Gradle 的 jniLibs 目录按 ABI 分子目录），另取（都放进同一目录）：
#   - GPU 加速器：LiteRT-LM v0.18.0 tag 下 prebuilt/android_arm64/ 的 libLiteRtOpenClAccelerator.so 与
#     libLiteRtGpuAccelerator.so（Git LFS，经 media.githubusercontent.com 下载）。WebGPU 那组（约 23 MB）不取。
#   - NPU 分发库：Google Maven 的 litert-npu-runtime-{qualcomm,mediatek,google-tensor}:2.3.0（Apache-2.0）中的
#     libLiteRtDispatch_*.so；AAR 先下载到 .cache/third-party/ 并校验。高通 NPU 要的 QNN 运行库是专有许可，不在这里取，
#     由应用按需下载（rummage_embed::npu）。
# 幂等：目标库文件都已存在且哈希正确时直接结束，不访问网络；轮子与 AAR 已缓存且哈希正确时不再下载。
# 升级运行库时同时更新这里的哈希、third_party/litert-lm/v<版本>/ 的头文件快照和 FFI 签名。
#
# GPU 安全：电脑上只取 litert-lm.dll，不取 WebGPU 用的 dxcompiler.dll、dxil.dll 与 NPU 的 dispatch 库；
# LiteRT 的 WebGPU 后端会让外接显卡驱动反复超时重置，电脑上只用 CPU 后端。
#
# 这一项也在发布工作流的 Linux 上用 PowerShell 7 运行，路径用 / 分隔。
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
# GPU 加速器的哈希即 Git LFS 指针中的 oid；AAR 与分发库的哈希取自本机下载、解压后的计算结果。
$Targets = @{
  'windows_x86_64' = @{
    Wheel = 'litert_lm_api-0.18.0-py3-none-win_amd64.whl'
    WheelSize = 26403380
    WheelSha256 = 'eb02dc5d0fc6a894a664cfcc63e3aaf3100973ba2c9a6a7eebf4df8749a01038'
    Files = @(
      @{ Entry = 'litert_lm/litert-lm.dll'; Name = 'litert-lm.dll'; Size = 50051584; Sha256 = 'c6421334f409a5b4b50b32e7925d6eb9d7e62245d00a6b01a7541596c8a1a1e3' }
    )
    Direct = @()
    Aars = @()
  }
  'android_arm64' = @{
    Wheel = 'litert_lm_api-0.18.0-py3-none-android_23_arm64_v8a.whl'
    WheelSize = 14243327
    WheelSha256 = 'a045af401dc199c35b1f44c29e8899cecbf2712a8194c9e35489ab5e2a95d47d'
    Files = @(
      @{ Entry = 'litert_lm/liblitert-lm.so'; Name = 'liblitert-lm.so'; Size = 39860448; Sha256 = '3d591b09116e7be3ab6b2b5b1a27bb3079dda0eb66550271533e6aee19e2dfd9' }
    )
    Direct = @(
      @{ Name = 'libLiteRtOpenClAccelerator.so'; Size = 3187496; Sha256 = 'dfc714a0b59d5e1bf5155db7533070bf13da28574a3bc52396a4ba22d23d893c' }
      @{ Name = 'libLiteRtGpuAccelerator.so'; Size = 3416880; Sha256 = '75ee49f6f31faf44b3b5ac54f15d58abd75438bc9e2f9abcf47f24279d337bc8' }
    )
    Aars = @(
      @{ Artifact = 'litert-npu-runtime-qualcomm'; Size = 509900; Sha256 = '55aa22e89b7decb9e2f56548188b9e8c42cc8f7a828e2ec4a17715079b313679'
         Name = 'libLiteRtDispatch_Qualcomm.so'; FileSize = 445712; FileSha256 = '22e2d581555617c4a4dabc46ed74833829816e9cd13a0eea27dd4d2178049751' }
      @{ Artifact = 'litert-npu-runtime-mediatek'; Size = 336230; Sha256 = '949fcd3a40d597f07473eb8d6b4d89ab6343b5a8fd2d21c118e2855e613c4587'
         Name = 'libLiteRtDispatch_MediaTek.so'; FileSize = 330968; FileSha256 = '62350e65cc6a2b9ab8cf9753740a22f1bd73327b7aec49a30f1214ebc0545b7f' }
      @{ Artifact = 'litert-npu-runtime-google-tensor'; Size = 672727; Sha256 = '5f38ab3de4b99ef75edf13c595cf0c090b10a82d9697f5b55703409757e786f7'
         Name = 'libLiteRtDispatch_GoogleTensor.so'; FileSize = 579024; FileSha256 = 'e38144002de11a15f653bb34f1c7efa4cb5af7acdddca424eb32297c5b5f9391' }
    )
  }
}
$GpuBase = 'https://media.githubusercontent.com/media/google-ai-edge/LiteRT-LM/v0.18.0/prebuilt/android_arm64'
$NpuVersion = '2.3.0'
$MavenBase = 'https://dl.google.com/android/maven2/com/google/ai/edge/litert'

$target = $Targets[$Platform]
$cacheDir = Join-Path $RepoRoot '.cache/third-party'
$outDir = Join-Path $cacheDir "litert-lm/$Version/$Platform"
if ($Platform -eq 'android_arm64') { $outDir = Join-Path $outDir 'arm64-v8a' }
$wheelPath = Join-Path $cacheDir $target.Wheel
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
Add-Type -AssemblyName System.IO.Compression.FileSystem

# 从 zip（轮子或 AAR）中取出 `$entry` 到 `$dest`，校验后改名
function Expand-Verified([string]$Zip, [string]$Entry, [string]$Dest, [string]$Sha256) {
  $z = [IO.Compression.ZipFile]::OpenRead($Zip)
  try {
    $e = $z.GetEntry($Entry)
    if (-not $e) { throw "$Zip 中没有 $Entry。" }
    $tmp = "$Dest.part"
    [IO.Compression.ZipFileExtensions]::ExtractToFile($e, $tmp, $true)
    $actual = Get-Sha256 $tmp
    if ($actual -ne $Sha256) {
      Remove-Item -LiteralPath $tmp
      throw "$(Split-Path $Dest -Leaf) 的 SHA-256 校验失败：期望 $Sha256，实际 $actual。"
    }
    Move-Item -LiteralPath $tmp -Destination $Dest -Force
  } finally {
    $z.Dispose()
  }
  Write-Host "已取出并校验 $(Split-Path $Dest -Leaf)。"
}

$missing = @($target.Files | Where-Object { -not (Test-FileHash (Join-Path $outDir $_.Name) $_.Sha256 $_.Size) })
if ($missing.Count -gt 0) {
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
  foreach ($f in $missing) {
    Expand-Verified $wheelPath $f.Entry (Join-Path $outDir $f.Name) $f.Sha256
  }
}

foreach ($f in $target.Direct) {
  $dest = Join-Path $outDir $f.Name
  if (Test-FileHash $dest $f.Sha256 $f.Size) { continue }
  Write-Host ("下载 {0}（{1}）……" -f $f.Name, (Format-Bytes $f.Size))
  Invoke-VerifiedDownload "$GpuBase/$($f.Name)" $dest $f.Size $f.Sha256
}

foreach ($a in $target.Aars) {
  $dest = Join-Path $outDir $a.Name
  if (Test-FileHash $dest $a.FileSha256 $a.FileSize) { continue }
  $aarName = "$($a.Artifact)-$NpuVersion.aar"
  $aar = Join-Path $cacheDir $aarName
  if (-not (Test-FileHash $aar $a.Sha256 $a.Size)) {
    Write-Host ("下载 {0}（{1}）……" -f $aarName, (Format-Bytes $a.Size))
    Invoke-VerifiedDownload "$MavenBase/$($a.Artifact)/$NpuVersion/$aarName" $aar $a.Size $a.Sha256
  }
  Expand-Verified $aar "jni/arm64-v8a/$($a.Name)" $dest $a.FileSha256
}

Write-Host "LiteRT-LM $Version（$Platform）的运行库已就绪：$outDir"
foreach ($f in $target.Files) { Write-Host ("  {0}  {1}  {2}" -f $f.Name, $f.Size, $f.Sha256) }
foreach ($f in $target.Direct) { Write-Host ("  {0}  {1}  {2}" -f $f.Name, $f.Size, $f.Sha256) }
foreach ($a in $target.Aars) { Write-Host ("  {0}  {1}  {2}" -f $a.Name, $a.FileSize, $a.FileSha256) }
