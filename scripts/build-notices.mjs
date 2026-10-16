// 生成随 Windows 安装包分发的第三方许可清单 src-tauri/bundle-res/THIRD-PARTY-NOTICES.txt，由 scripts/build-core.mjs 在打包时调用。
// 内容：third_party/ 与 third-party-licenses/ 中随程序分发的组件（llama.cpp、LLVM OpenMP、FFmpeg、字体、移植的代码），
// cargo-about 汇总的 Rust 依赖（配置见 about.toml，Windows 目标的普通与构建依赖），以及 npm ls --omit=dev 列出的前端依赖。
// 用纯文本：安装目录中用任何编辑器都能打开，不依赖浏览器，也便于把各来源的原文拼在一起。
// 需要 cargo-about：cargo install cargo-about --version 0.9.2 --locked --features cli
import { execFileSync } from 'node:child_process'
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

const OUT = 'src-tauri/bundle-res/THIRD-PARTY-NOTICES.txt'
const RULE = '='.repeat(80)
const THIN = '-'.repeat(80)

const read = (path) => readFileSync(path, 'utf8').replace(/^﻿/, '').replace(/\r\n/g, '\n').trim()

const parts = [
  'Rummage 第三方许可清单 / Third-Party Notices',
  '',
  'Rummage 以 MIT 协议发布。本文件列出 Rummage Windows 版随附的第三方组件及其许可原文。',
  'Rummage is released under the MIT License. This file lists the third-party components shipped with',
  'Rummage for Windows and their license texts.',
]

const section = (title, body) => parts.push('', '', RULE, title, RULE, '', body)

section('llama.cpp / ggml（llama\\cpu\\、llama\\vulkan\\）', read('third_party/llama.cpp/b11461/LICENSE'))
section('LLVM OpenMP 运行库 libomp.dll（llama\\cpu\\、llama\\vulkan\\）', read('.cache/third-party/llama.cpp/b11461/win-cpu-x64/LICENSE-LLVM-OpenMP'))
section('FFmpeg（ffmpeg\\，LGPL 许可全文见 ffmpeg\\LICENSE.txt）', read('third-party-licenses/FFmpeg-NOTICE.txt'))
section('HarmonyOS Sans SC 字体', read('third-party-licenses/HarmonyOS-Sans-LICENSE.txt'))
section('Maple Mono NF CN 字体', read('third-party-licenses/MapleMono-LICENSE.txt'))
section('Pillow（图像缩放算法的移植）', read('third-party-licenses/Pillow-LICENSE.txt'))
section('libwebp（静态链接，用于编码缩略图）', read('third-party-licenses/libwebp-LICENSE.txt'))

// Rust：按许可原文分组，每组列出使用它的包；工作区自己的包（没有 source）不列出
let about
try {
  about = JSON.parse(execFileSync('cargo', ['about', 'generate', '--format', 'json', '--workspace', '--locked', '--fail'], { encoding: 'utf8', maxBuffer: 256 * 1024 * 1024 }))
} catch (err) {
  console.error('cargo about 运行失败。未安装时请先运行：cargo install cargo-about --version 0.9.2 --locked --features cli')
  throw err
}
const rust = []
const rustSeen = new Set()
for (const lic of about.licenses) {
  const users = lic.used_by.map((u) => u.crate).filter((c) => c.source)
  if (!users.length) continue
  const names = [...new Set(users.map((c) => `${c.name} ${c.version}`))].sort()
  for (const name of names) rustSeen.add(name)
  // MPL-2.0 要求说明源码获取方式；这些包都未经修改
  const source = lic.id === 'MPL-2.0'
    ? `\n以上包未经修改，源码见：\n${[...new Set(users.map((c) => c.name))].sort().map((n) => `  https://crates.io/crates/${n}`).join('\n')}`
    : ''
  rust.push(`${THIN}\n${lic.name}（${lic.id}）\n使用者：${names.join('、')}${source}\n${THIN}\n\n${lic.text.replace(/\r\n/g, '\n').trim()}`)
}
section(`Rust 依赖（共 ${rustSeen.size} 个包）`, rust.join('\n\n'))

// 前端：生产依赖，同名同版本只列一次；许可原文取包目录中的全部许可文件（如 LICENSE-MIT 与 LICENSE-APACHE-2.0），不含 .spdx
const tree = JSON.parse(execFileSync('npm', ['ls', '--omit=dev', '--all', '--json', '--long'], { encoding: 'utf8', shell: true, maxBuffer: 256 * 1024 * 1024 }))
const npm = new Map()
const walk = (deps) => {
  for (const [name, d] of Object.entries(deps ?? {})) {
    // 未安装的可选 peer 依赖没有版本，不随程序分发
    if (!d.version || npm.has(`${name} ${d.version}`)) continue
    const files = d.path && existsSync(d.path)
      ? readdirSync(d.path).filter((f) => /^(licen[cs]e|copying)([-_.].*)?$/i.test(f) && !/\.spdx$/i.test(f)).sort()
      : []
    const license = typeof d.license === 'object' ? d.license?.type : d.license
    const text = files.length ? files.map((f) => read(join(d.path, f))).join(`\n\n${THIN}\n\n`) : `包中没有许可文件，许可为 ${license}。`
    npm.set(`${name} ${d.version}`, `${THIN}\n${name} ${d.version}（${license ?? '未声明'}）\n${THIN}\n\n${text}`)
    walk(d.dependencies)
  }
}
walk(tree.dependencies)
section(`前端依赖（共 ${npm.size} 个包）`, [...npm.keys()].sort().map((k) => npm.get(k)).join('\n\n'))

mkdirSync('src-tauri/bundle-res', { recursive: true })
writeFileSync(OUT, `${parts.join('\n')}\n`)
console.log(`已生成 ${OUT}：Rust 依赖 ${rustSeen.size} 个、前端依赖 ${npm.size} 个。`)
