# 获取 llama.cpp 的 Windows 预编译包与 C 头文件。
# 来源：GitHub Release ggml-org/llama.cpp 标签 b11461 的 llama-b11461-bin-win-cpu-x64.zip（MIT，附带的 libomp
# 为 Apache 2.0 with LLVM exception），下载到 .cache/third-party/llama.cpp/<标签>/，用下方钉死的 SHA-256 校验，
# 再取出所需文件到 .cache/third-party/llama.cpp/<标签>/win-cpu-x64/ 并逐个校验；头文件从同一标签的源码
# 取到 .cache/third-party/llama.cpp/<标签>/include/（ctypes 绑定的签名以它们为准）。
# 幂等：目标文件都已存在且哈希正确时直接结束，不访问网络；压缩包已缓存且哈希正确时不再下载。
#
# GPU 安全：默认（-Variant cpu）只取 CPU 后端（ggml-cpu-*.dll），不取 ggml-rpc.dll 与任何 GPU 包。
# GPU 测试可能让显卡驱动反复超时重置，Vulkan 包只在显式指定 -Variant vulkan 时获取。
#
# 版本选择：b11452 起支持 EmbeddingGemma 2 全模态；b11461 与参考项目 DigUp 相同，便于对照它的实测结论。
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File scripts/fetch-llama.ps1
#   powershell -ExecutionPolicy Bypass -File scripts/fetch-llama.ps1 -Variant vulkan
#
# -Variant vulkan：取 llama-<标签>-bin-win-vulkan-x64.zip，取出到
# .cache/third-party/llama.cpp/<标签>/win-vulkan-x64/。其中的 llama.dll、mtmd.dll、ggml*.dll 与 CPU 包逐字节相同，只多出 ggml-vulkan.dll；
# Vulkan 加载器由显卡驱动提供，包内不带。
#

param([ValidateSet('cpu', 'vulkan')][string]$Variant = 'cpu')

. (Join-Path $PSScriptRoot 'common.ps1')

$Tag = 'b11461'
$Zip = "llama-$Tag-bin-win-cpu-x64.zip"
$ZipSize = 19437105
# GitHub Release 接口给出的 digest，与本机下载后的计算结果一致。
$ZipSha256 = 'e283199134e74405a3684cfaa3911708ca7d051a8c2a8dcb10b3dfa1678913c3'

# 嵌入只需要 llama.dll、mtmd.dll 与 ggml 的 CPU 后端；ggml.dll 运行时按 CPU 特性从 ggml-cpu-*.dll 中选一个加载。
$Files = @(
  @{ Name = 'llama.dll'; Size = 3337728; Sha256 = 'c1d0905c47a396dae0f7ae6ea7988da3c20ed5009f980d308ee0571a366f2782' },
  @{ Name = 'mtmd.dll'; Size = 1796096; Sha256 = 'c20af0dd7cdaf888aca22e2cb614bbdd5b3d479b0fa02d4e3e752765ab6f46cb' },
  @{ Name = 'ggml.dll'; Size = 79872; Sha256 = '579c7a15967ff31812a2990b9cdc16d4db7c8a71cb44dfef5acb034fa0ea1cc4' },
  @{ Name = 'ggml-base.dll'; Size = 801792; Sha256 = '517b4e91214e61da6cc63ae3279c76c529017e6adc0fff6ee44129c8416baa10' },
  @{ Name = 'libomp.dll'; Size = 768000; Sha256 = 'a12116ba72d1d6820407cf30be23da04ce79d6bb8a71a5ee71759c5a1faa6f1c' },
  @{ Name = 'LICENSE-LLVM-OpenMP'; Size = 19741; Sha256 = 'fdad1758a9e1f9d5a81e18879b3406772115edc92c24bfa36b70c654f325e8e4' },
  @{ Name = 'ggml-cpu-alderlake.dll'; Size = 1435136; Sha256 = '649cfcf912cdceb699e30d097dac91feba61c8e43d105295c09e9134d07399b9' },
  @{ Name = 'ggml-cpu-cannonlake.dll'; Size = 1614848; Sha256 = '44be32f68ba7ff25363ba31d0378992a6d798caf7c1117979e3d469164481f06' },
  @{ Name = 'ggml-cpu-cascadelake.dll'; Size = 1599488; Sha256 = '4f4277e7d2b2115b27d6d82e5dfe6c35b381a806820b8c3048f418b95a10243d' },
  @{ Name = 'ggml-cpu-cooperlake.dll'; Size = 1609216; Sha256 = 'b8dd7719e41e7bf7e291dfc13acff215903ca21040ac1cf537d83a5afdd10fb1' },
  @{ Name = 'ggml-cpu-haswell.dll'; Size = 1439744; Sha256 = 'a9d28ddeaa127968498d1d04e928de2b2c5e04713ad9468f2b5f0dcc79759ac8' },
  @{ Name = 'ggml-cpu-icelake.dll'; Size = 1606656; Sha256 = 'e89473d3e0cb15248ac44e6a001cee4320135359c2fad6c63eeb77c5697b396f' },
  @{ Name = 'ggml-cpu-ivybridge.dll'; Size = 1315328; Sha256 = '5ede3815f0583a147c33a4208bd71e66a18892bbac2e947131e0abdb2020dd27' },
  @{ Name = 'ggml-cpu-piledriver.dll'; Size = 1308160; Sha256 = 'b41ef369f4fe14b0aee38c247f605a5cc4b7708d95bd8819f946bc987e85c252' },
  @{ Name = 'ggml-cpu-sandybridge.dll'; Size = 1290240; Sha256 = 'f7e8919c6f5a8cb508d49f12d2ff84f97d3ec7eeabcbbaa15f873afbcb00f00b' },
  @{ Name = 'ggml-cpu-sapphirerapids.dll'; Size = 1886208; Sha256 = 'd706f3a818227bd9de0a577fbea5bf0cc3048bb52b25be8a96ae352cbdb35061' },
  @{ Name = 'ggml-cpu-skylakex.dll'; Size = 1608704; Sha256 = '7417d4e1792edca5d7228b2b213ac7a2b7438d3455aa37bd45effe00bc2aad05' },
  @{ Name = 'ggml-cpu-sse42.dll'; Size = 915968; Sha256 = '6ff7ad5e8efe284700056c4d07407115c7c73bfb657f1d56e0f1c0d5e30a4363' },
  @{ Name = 'ggml-cpu-x64.dll'; Size = 908800; Sha256 = '8654c89afaa49ab15480b1fcde1f9a2b4c63deff94bcbaf5c204dfa9fe9c2a04' },
  @{ Name = 'ggml-cpu-zen4.dll'; Size = 1615872; Sha256 = 'e4bf749d63a4880f4026075f2f41b1a0e2d543c38df348c9f13d914d8d308b4f' }
)

# 源码中的头文件（raw.githubusercontent.com/ggml-org/llama.cpp/<标签>/<路径>），哈希为本机下载后的计算结果。
$Headers = @(
  @{ Path = 'include/llama.h'; Sha256 = '84867ea210156fd2618871c8d3181b5b7703750e829bdac6a12ad54e76570faa' },
  @{ Path = 'ggml/include/ggml.h'; Sha256 = '12ee71f99db7db9b353bc02b1fbb57c344ee17c01ac5fb7952b41a637a747ea9' },
  @{ Path = 'ggml/include/ggml-backend.h'; Sha256 = '6f10f2abca95dd3e7040c650b1bc005111a495c2f2d8ceea57dfe3a438b8b412' },
  @{ Path = 'ggml/include/ggml-cpu.h'; Sha256 = '316279e004cdeb8e6ef78599acb602bf79a8abdf897fed9fd1914808c1518c6e' },
  @{ Path = 'tools/mtmd/mtmd.h'; Sha256 = 'ff6798e8ec56a409b8f2e99821cdbdb857286ca085a762b7ed3e5599c1cc77d5' },
  @{ Path = 'tools/mtmd/mtmd-helper.h'; Sha256 = '893b42990d4b386b8c52d8078499f5615c2d7b0c92d0c30501816d65cab8db6c' }
)

$baseDir = Join-Path $RepoRoot ".cache/third-party/llama.cpp/$Tag"

if ($Variant -ne 'cpu') {
  # 压缩包大小与 SHA-256 取自 GitHub Release 接口的 digest；文件哈希为本机解压后的计算结果。
  $GpuZips = @(
    @{ Name = "llama-$Tag-bin-win-vulkan-x64.zip"; Size = 33377638; Sha256 = '6b9ca8ad3e43ac3e166d1997875709e4547214bb5bf1c2ebfdd17a2350a8380c'
      Files = @($Files) + @(@{ Name = 'ggml-vulkan.dll'; Size = 45305856; Sha256 = '905256b09c55a0d9df17ea74fb296b8da80bbde23a217efdfd5f0b1bc6695b8f' }) }
  )
  $gpuDir = Join-Path $baseDir 'win-vulkan-x64'
  Add-Type -AssemblyName System.IO.Compression.FileSystem
  foreach ($z in $GpuZips) {
    $need = @($z.Files | Where-Object { -not (Test-FileHash (Join-Path $gpuDir $_.Name) $_.Sha256 $_.Size) })
    if ($need.Count -eq 0) { continue }
    $zp = Join-Path $baseDir $z.Name
    if (-not (Test-FileHash $zp $z.Sha256 $z.Size)) {
      Write-Host ("下载 {0}（{1}）……" -f $z.Name, (Format-Bytes $z.Size))
      Invoke-VerifiedDownload "https://github.com/ggml-org/llama.cpp/releases/download/$Tag/$($z.Name)" $zp $z.Size $z.Sha256
    }
    New-Item -ItemType Directory -Force -Path $gpuDir | Out-Null
    $zip = [IO.Compression.ZipFile]::OpenRead($zp)
    try {
      foreach ($f in $need) {
        $entry = $zip.GetEntry($f.Name)
        if (-not $entry) { throw "$($z.Name) 中没有 $($f.Name)。" }
        $dest = Join-Path $gpuDir $f.Name
        [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, "$dest.part", $true)
        $actual = Get-Sha256 "$dest.part"
        if ($actual -ne $f.Sha256) {
          Remove-Item -LiteralPath "$dest.part"
          throw "$($f.Name) 的 SHA-256 校验失败：期望 $($f.Sha256)，实际 $actual。"
        }
        Move-Item -LiteralPath "$dest.part" -Destination $dest -Force
      }
    } finally {
      $zip.Dispose()
    }
    Write-Host "已从 $($z.Name) 取出并校验 $($need.Count) 个文件。"
  }
  Write-Host "llama.cpp $Tag（Windows $Variant x64）已就绪：$gpuDir"
  return
}

$binDir = Join-Path $baseDir 'win-cpu-x64'
$incDir = Join-Path $baseDir 'include'
$zipPath = Join-Path $baseDir $Zip

$missing = @($Files | Where-Object { -not (Test-FileHash (Join-Path $binDir $_.Name) $_.Sha256 $_.Size) })
$missingHeaders = @($Headers | Where-Object { -not (Test-FileHash (Join-Path $incDir (Split-Path -Leaf $_.Path)) $_.Sha256) })

if ($missing.Count -gt 0) {
  if (Test-FileHash $zipPath $ZipSha256 $ZipSize) {
    Write-Host "使用已缓存的压缩包：$zipPath"
  } else {
    $url = "https://github.com/ggml-org/llama.cpp/releases/download/$Tag/$Zip"
    Write-Host ("下载 {0}（{1}）……" -f $Zip, (Format-Bytes $ZipSize))
    Invoke-VerifiedDownload $url $zipPath $ZipSize $ZipSha256
    Write-Host "压缩包下载完成，SHA-256 校验通过。"
  }
  Add-Type -AssemblyName System.IO.Compression.FileSystem
  New-Item -ItemType Directory -Force -Path $binDir | Out-Null
  $zip = [IO.Compression.ZipFile]::OpenRead($zipPath)
  try {
    foreach ($f in $missing) {
      $entry = $zip.GetEntry($f.Name)
      if (-not $entry) { throw "压缩包中没有 $($f.Name)。" }
      $dest = Join-Path $binDir $f.Name
      $tmp = "$dest.part"
      [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $tmp, $true)
      $actual = Get-Sha256 $tmp
      if ($actual -ne $f.Sha256) {
        Remove-Item -LiteralPath $tmp
        throw "$($f.Name) 的 SHA-256 校验失败：期望 $($f.Sha256)，实际 $actual。"
      }
      Move-Item -LiteralPath $tmp -Destination $dest -Force
    }
  } finally {
    $zip.Dispose()
  }
  Write-Host "已取出并校验 $($missing.Count) 个文件。"
}

foreach ($h in $missingHeaders) {
  $name = Split-Path -Leaf $h.Path
  $dest = Join-Path $incDir $name
  New-Item -ItemType Directory -Force -Path $incDir | Out-Null
  $req = [Net.HttpWebRequest]::Create("https://raw.githubusercontent.com/ggml-org/llama.cpp/$Tag/$($h.Path)")
  $req.UserAgent = 'Rummage-dev-scripts'
  $req.Timeout = 30000
  $resp = $req.GetResponse()
  try {
    $out = New-Object IO.FileStream("$dest.part", [IO.FileMode]::Create, [IO.FileAccess]::Write)
    try { $resp.GetResponseStream().CopyTo($out) } finally { $out.Close() }
  } finally {
    $resp.Close()
  }
  $actual = Get-Sha256 "$dest.part"
  if ($actual -ne $h.Sha256) {
    Remove-Item -LiteralPath "$dest.part"
    throw "$name 的 SHA-256 校验失败：期望 $($h.Sha256)，实际 $actual。"
  }
  Move-Item -LiteralPath "$dest.part" -Destination $dest -Force
  Write-Host "已下载并校验 $name。"
}

Write-Host "llama.cpp $Tag（Windows CPU x64）已就绪：$binDir"
Write-Host "头文件：$incDir"
