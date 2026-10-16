# 获取 Windows 版 ffmpeg / ffprobe。
# 来源：BtbN/FFmpeg-Builds 的 LGPL 共享库构建 win64-lgpl-shared，release 9.0 分支。BtbN 的 latest 是滚动标签，
# 这里钉在带日期的 autobuild 标签上的固定文件，用 GitHub Release 接口给出的 digest 校验压缩包，再逐个校验取出的文件。
# 不用静态构建 win64-lgpl：两个 exe 各自内嵌全部库，解压后约 269 MB；共享库构建两个 exe 共用一套 DLL，约 157 MB。
# 只取 bin/ 下的 ffmpeg.exe、ffprobe.exe 与它们导入的 7 个 DLL（dumpbin /dependents 核实，两个 exe 都直接导入全部 7 个；
# 不取 ffplay.exe），加压缩包里的 LICENSE.txt，放到 .cache/third-party/ffmpeg/<版本>/，压缩包也缓存在那里。
# exe 与 DLL 放在同一目录即可被找到（Windows 先搜索 exe 所在目录）。
# 幂等：取出的文件都已存在且哈希正确时直接结束，不访问网络；压缩包已缓存且哈希正确时不再下载。
#
# 协议核实（ffmpeg -version 与 ffmpeg -L）：configuration 中没有 --enable-gpl、--enable-nonfree，
# 有 --enable-version3，即整体为 LGPL v3 或更高版本（-L 输出与 LICENSE.txt 都是 LGPL v3）。
# 程序只以独立可执行文件调用它（子进程 + 管道），不链接其库，也不修改它；许可说明见 third-party-licenses/FFmpeg-NOTICE.txt。
# scripts/build-core.mjs 从这里读取版本与下面的文件清单打包，改清单时同步 NOTICE。
#
# BtbN 只保留最近 14 天的每日构建，这个标签过期后下载地址会失效：先从本仓库固定的 deps Release 镜像下载同一个文件，
# 失败再用 BtbN 原地址；两者都按下面钉定的 SHA-256 校验。本机首次下载后一直复用缓存。
#
# 用法：powershell -ExecutionPolicy Bypass -File scripts/fetch-ffmpeg.ps1

. (Join-Path $PSScriptRoot 'common.ps1')

$Tag = 'autobuild-2026-10-13-13-05'
# 与 crates/rummage-media/src/ffmpeg.rs 的 FFMPEG_BUILD 一致
$Version = 'n9.0.2-27-g7e93014c5b'
$Zip = "ffmpeg-$Version-win64-lgpl-shared-9.0.zip"
$ZipSize = 77230840
$ZipSha256 = '9df4db960f3f0cd3ed76be0a2be96265d00db4c3be2526a6d39b03c2b547100d'

# 压缩包内的路径（相对顶层目录）与取出后的文件；哈希为本机解压后的计算结果。
$Files = @(
  @{ Entry = 'bin/ffmpeg.exe'; Name = 'ffmpeg.exe'; Size = 540160; Sha256 = '4dbccc4083154a99b52519125ea484b69c5a1a911be4e549147561f2998a5bfa' },
  @{ Entry = 'bin/ffprobe.exe'; Name = 'ffprobe.exe'; Size = 229888; Sha256 = '08c81ab3131dabb614cee7c61951a97e9dcacb2f4738ac6e2d793b68b889c360' },
  @{ Entry = 'bin/avcodec-63.dll'; Name = 'avcodec-63.dll'; Size = 91136000; Sha256 = '8513a33e56de4111e4f02d5c901b4f5f1baab60850bb505c4ce370aa106c4460' },
  @{ Entry = 'bin/avdevice-63.dll'; Name = 'avdevice-63.dll'; Size = 4948480; Sha256 = '53e4412211433ab642a30c14bd82aefc32cd029e3846c24c9f7b748cda438ba1' },
  @{ Entry = 'bin/avfilter-12.dll'; Name = 'avfilter-12.dll'; Size = 31540224; Sha256 = '2cc21714dbfc1b4ef4ecf2595917c1c98591a2bfef8c50e9c959085ac2e90448' },
  @{ Entry = 'bin/avformat-63.dll'; Name = 'avformat-63.dll'; Size = 22770176; Sha256 = '7da93e1097bbb1f9268d1470526c539b041a685e5b245a0b6fc90414c8c78008' },
  @{ Entry = 'bin/avutil-61.dll'; Name = 'avutil-61.dll'; Size = 3016704; Sha256 = 'd14d4d2b406e6660de6622d870e2e1f84ee413405ebefa0b3e02905316309d59' },
  @{ Entry = 'bin/swresample-7.dll'; Name = 'swresample-7.dll'; Size = 734720; Sha256 = '944671b04587817ff244c12814416dfbed271e08c5f2a7cd147cf83bcc2e2332' },
  @{ Entry = 'bin/swscale-10.dll'; Name = 'swscale-10.dll'; Size = 2332160; Sha256 = 'b42a0b73a63eac48ae5c4eb186a480e8c71cd8bcaab72d01c57e1d31fb411cb5' },
  @{ Entry = 'LICENSE.txt'; Name = 'LICENSE.txt'; Size = 7651; Sha256 = 'da7eabb7bafdf7d3ae5e9f223aa5bdc1eece45ac569dc21b3b037520b4464768' }
)

$dir = Join-Path $RepoRoot ".cache\third-party\ffmpeg\$Version"
$zipPath = Join-Path $dir $Zip

$missing = @($Files | Where-Object { -not (Test-FileHash (Join-Path $dir $_.Name) $_.Sha256 $_.Size) })
if ($missing.Count -eq 0) {
  Write-Host "ffmpeg $Version（Windows x64，LGPL）已就绪：$dir"
  return
}

if (Test-FileHash $zipPath $ZipSha256 $ZipSize) {
  Write-Host "使用已缓存的压缩包：$zipPath"
} else {
  Write-Host ("下载 {0}（{1}）……" -f $Zip, (Format-Bytes $ZipSize))
  try {
    Invoke-VerifiedDownload "https://github.com/CookSleep/Rummage/releases/download/deps/$Zip" $zipPath $ZipSize $ZipSha256
  } catch {
    Write-Host "从 deps 镜像下载失败（$($_.Exception.Message)），改用 BtbN 原地址。"
    Invoke-VerifiedDownload "https://github.com/BtbN/FFmpeg-Builds/releases/download/$Tag/$Zip" $zipPath $ZipSize $ZipSha256
  }
  Write-Host "压缩包下载完成，SHA-256 校验通过。"
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
$top = [IO.Path]::GetFileNameWithoutExtension($Zip)
$zip = [IO.Compression.ZipFile]::OpenRead($zipPath)
try {
  foreach ($f in $missing) {
    $entry = $zip.GetEntry("$top/$($f.Entry)")
    if (-not $entry) { throw "压缩包中没有 $top/$($f.Entry)。" }
    $dest = Join-Path $dir $f.Name
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
Write-Host "已取出并校验 $($missing.Count) 个文件。"
Write-Host "ffmpeg $Version（Windows x64，LGPL）已就绪：$dir"
