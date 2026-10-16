// 构建核心进程 rummage-core，由 tauri.conf.json 的 beforeDevCommand / beforeBuildCommand 调用。
// 核心会常驻，正在运行的 rummage-core.exe 无法被覆盖：构建前先用已有的可执行文件执行 --shutdown，
// 请开发版数据目录上的核心退出并等它结束（核心没有运行时直接返回）。
// 开发：debug 构建的产物与界面进程同在 target/debug/，界面进程从自己所在目录找到它。
// 打包（--release）：release 构建后复制到 externalBin 要求的 src-tauri/binaries/rummage-core-<目标三元组>.exe，
// 安装后去掉三元组后缀、放在主程序旁边。externalBin 只写在 tauri.bundle.conf.json 中，由 tauri:build 合并，
// 这样平时的 cargo build / clippy / test 不要求 binaries/ 中已有文件。
// Windows 打包时还把 llama.cpp 运行库（scripts/fetch-llama.ps1 解压到 .cache 的官方预编译包，只取 DLL 和 libomp 的许可）
// 复制到 src-tauri/bundle-res/llama/{cpu,vulkan}/，连同 llama.cpp 的 LICENSE（预编译包不带）作为 bundle.resources
// 安装到 <安装目录>\llama\，与核心进程 runtime_dirs 的约定一致。
// ffmpeg / ffprobe 与它们依赖的 DLL（scripts/fetch-ffmpeg.ps1 取到 .cache 的 LGPL 共享库构建）连同 LGPL 许可与我们的说明复制到
// src-tauri/bundle-res/ffmpeg/，安装到 <安装目录>\ffmpeg\，与核心进程 ffmpeg_dir 的约定一致；版本与文件清单取自获取脚本，不另写一份。
// 第三方许可清单由 scripts/build-notices.mjs 生成到 src-tauri/bundle-res/THIRD-PARTY-NOTICES.txt，安装到安装目录
import { execFileSync } from 'node:child_process'
import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync } from 'node:fs'
import { resolve } from 'node:path'

const release = process.argv.includes('--release')
const ext = process.platform === 'win32' ? '.exe' : ''
const exe = `target/${release ? 'release' : 'debug'}/rummage-core${ext}`
const llama = '.cache/third-party/llama.cpp/b11461'
// [.cache 中的目录, 安装后 llama\ 下的目录, 获取命令, 该包特有的文件]
const runtimes = [
  ['win-cpu-x64', 'cpu', 'scripts/fetch-llama.ps1', []],
  ['win-vulkan-x64', 'vulkan', 'scripts/fetch-llama.ps1 -Variant vulkan', ['ggml-vulkan.dll']],
]

const fetchFfmpeg = readFileSync('scripts/fetch-ffmpeg.ps1', 'utf8')
const ffmpeg = `.cache/third-party/ffmpeg/${fetchFfmpeg.match(/^\$Version = '([^']+)'/m)[1]}`
const ffmpegFiles = [...fetchFfmpeg.matchAll(/Name = '([^']+)'/g)].map((m) => m[1])

// 先检查运行库是否齐全，免得 release 构建完才失败
if (release && process.platform === 'win32') {
  const missing = ffmpegFiles.filter((name) => !existsSync(`${ffmpeg}/${name}`))
  if (missing.length) {
    console.error(`缺少 ${ffmpeg}/ 中的 ${missing.join('、')}，请先运行：powershell -ExecutionPolicy Bypass -File scripts/fetch-ffmpeg.ps1`)
    process.exit(1)
  }
  for (const [dir, , cmd, extra] of runtimes) {
    const missing = ['llama.dll', 'mtmd.dll', 'ggml.dll', 'ggml-base.dll', 'libomp.dll', 'LICENSE-LLVM-OpenMP', ...extra]
      .filter((name) => !existsSync(`${llama}/${dir}/${name}`))
    // CPU 后端按指令集分成多个 ggml-cpu-*.dll，至少要有一个
    if (!existsSync(`${llama}/${dir}`) || !readdirSync(`${llama}/${dir}`).some((name) => /^ggml-cpu-.+\.dll$/.test(name))) missing.push('ggml-cpu-*.dll')
    if (missing.length) {
      console.error(`缺少 llama.cpp 运行库 ${llama}/${dir}/ 中的 ${missing.join('、')}，请先运行：powershell -ExecutionPolicy Bypass -File ${cmd}`)
      process.exit(1)
    }
  }
}

if (existsSync(exe)) {
  try {
    execFileSync(exe, ['--shutdown', '--data-dir', resolve('src-tauri/dev-data')], { stdio: 'inherit' })
  } catch (err) {
    // 核心没有按时退出时，交给 cargo build 报告能否覆盖
    console.warn('请核心进程退出失败：', err.message)
  }
}

execFileSync('cargo', ['build', '-p', 'rummage-core', ...(release ? ['--release'] : [])], { stdio: 'inherit' })

if (release) {
  const triple = execFileSync('rustc', ['-vV'], { encoding: 'utf8' }).match(/^host: (\S+)$/m)[1]
  mkdirSync('src-tauri/binaries', { recursive: true })
  copyFileSync(exe, `src-tauri/binaries/rummage-core-${triple}${ext}`)
}

if (release && process.platform === 'win32') {
  const out = 'src-tauri/bundle-res/llama'
  // 清空后重新复制，不留下旧版本的文件
  rmSync(out, { recursive: true, force: true })
  for (const [dir, target] of runtimes) {
    mkdirSync(`${out}/${target}`, { recursive: true })
    for (const name of readdirSync(`${llama}/${dir}`)) {
      if (name.endsWith('.dll') || name === 'LICENSE-LLVM-OpenMP') copyFileSync(`${llama}/${dir}/${name}`, `${out}/${target}/${name}`)
    }
  }
  copyFileSync('third_party/llama.cpp/b11461/LICENSE', `${out}/LICENSE`)

  const ffOut = 'src-tauri/bundle-res/ffmpeg'
  rmSync(ffOut, { recursive: true, force: true })
  mkdirSync(ffOut, { recursive: true })
  for (const name of ffmpegFiles) copyFileSync(`${ffmpeg}/${name}`, `${ffOut}/${name}`)
  copyFileSync('third-party-licenses/FFmpeg-NOTICE.txt', `${ffOut}/NOTICE.txt`)

  execFileSync('node', ['scripts/build-notices.mjs'], { stdio: 'inherit' })
}
