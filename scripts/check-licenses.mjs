// 本地许可检查：列出 Rust（cargo metadata，Windows 与 Android 目标的普通与构建依赖）和前端
// （npm ls --omit=dev）随程序分发的依赖，出现 GPL / AGPL / LGPL、没有声明许可、许可表达式无法解析，
// 或是 UNLICENSED / “SEE LICENSE IN …”的包时以非零退出码结束。
// SPDX 表达式中只要有一种选法全是宽松许可就算通过（如 “MIT OR GPL-2.0”）；只给出许可文件（license-file）的包
// 无法自动判断，列出来提醒人工确认，不算失败。LGPL 组件（ffmpeg）以独立可执行文件分发，不在这两类依赖中，
// 许可说明在 third-party-licenses/FFmpeg-NOTICE.txt：这里核对其中写的版本、随附的 exe 与 DLL 与 scripts/fetch-ffmpeg.ps1 钉定的一致
import { execFileSync, execSync } from 'node:child_process'
import { readFileSync } from 'node:fs'

const COPYLEFT = /^(A|L)?GPL/i
const OPERATORS = ['AND', 'OR', 'WITH']

// 解析 SPDX 表达式，返回是否存在不含 copyleft 的选法；旧式的 “MIT/Apache-2.0” 按 OR 处理。
// 有多余的词、括号不配对等无法解析时返回 null，如 “MIT, GPL-3.0”“GNU GPL v3”
function permissive(expr) {
  const tokens = expr.replace(/\//g, ' OR ').match(/\(|\)|[^\s()]+/g) ?? []
  let pos = 0
  const peek = () => tokens[pos]?.toUpperCase()
  // 应当是许可 ID 的位置
  const id = () => {
    const tok = tokens[pos++]
    if (!tok || tok === '(' || tok === ')' || OPERATORS.includes(tok.toUpperCase())) throw new Error()
    return tok
  }
  const term = () => {
    if (tokens[pos] === '(') {
      pos++
      const ok = or()
      if (tokens[pos++] !== ')') throw new Error()
      return ok
    }
    const lic = id()
    // “WITH 例外”不改变基础许可的性质
    if (peek() === 'WITH') {
      pos++
      id()
    }
    return !COPYLEFT.test(lic)
  }
  const and = () => {
    let ok = term()
    while (peek() === 'AND') {
      pos++
      ok = term() && ok
    }
    return ok
  }
  const or = () => {
    let ok = and()
    while (peek() === 'OR') {
      pos++
      ok = and() || ok
    }
    return ok
  }
  try {
    const ok = or()
    return pos === tokens.length ? ok : null
  } catch {
    return null
  }
}

const offenders = []
const manual = []

function check(side, name, version, license, licenseFile) {
  if (!license) {
    if (licenseFile) manual.push(`${side} ${name} ${version}：${licenseFile}`)
    else offenders.push(`${side} ${name} ${version}：没有声明许可`)
    return
  }
  if (/^(UNLICENSED$|SEE LICENSE IN\b)/i.test(license.trim())) {
    offenders.push(`${side} ${name} ${version}：${license}（未授予开源许可）`)
    return
  }
  const ok = permissive(license)
  if (ok === null) offenders.push(`${side} ${name} ${version}：无法解析许可表达式“${license}”`)
  else if (!ok) offenders.push(`${side} ${name} ${version}：${license}`)
}

// Rust：Windows 与 Android 目标分别取依赖图，从工作区成员出发沿普通与构建依赖遍历（开发依赖不随程序分发），合并结果
const packages = new Map()
const crates = new Set()
for (const target of ['x86_64-pc-windows-msvc', 'aarch64-linux-android']) {
  const meta = JSON.parse(execFileSync('cargo', ['metadata', '--format-version', '1', '--filter-platform', target], { encoding: 'utf8', maxBuffer: 256 * 1024 * 1024 }))
  for (const p of meta.packages) packages.set(p.id, p)
  const nodes = new Map(meta.resolve.nodes.map((n) => [n.id, n]))
  const seen = new Set()
  const queue = [...meta.workspace_members]
  while (queue.length) {
    const id = queue.pop()
    if (seen.has(id)) continue
    seen.add(id)
    crates.add(id)
    for (const dep of nodes.get(id)?.deps ?? []) {
      if (dep.dep_kinds.some((k) => k.kind === null || k.kind === 'build')) queue.push(dep.pkg)
    }
  }
}
for (const id of crates) {
  const p = packages.get(id)
  check('Rust', p.name, p.version, p.license, p.license_file)
}
// 前端：生产依赖，同名同版本只算一次
const tree = JSON.parse(execSync('npm ls --omit=dev --all --json --long', { encoding: 'utf8', maxBuffer: 256 * 1024 * 1024 }))
const npmSeen = new Set()
const walk = (deps) => {
  for (const [name, d] of Object.entries(deps ?? {})) {
    // 未安装的可选 peer 依赖（如 zustand 的 immer）没有版本，不随程序分发
    if (!d.version) continue
    const key = `${name}@${d.version}`
    if (npmSeen.has(key)) continue
    npmSeen.add(key)
    // 早期的包把许可写成 { type } 或 licenses 数组
    const license = typeof d.license === 'object' ? d.license?.type : d.license ?? d.licenses?.map((l) => l.type ?? l).join(' OR ')
    check('npm', name, d.version, license, null)
    walk(d.dependencies)
  }
}
walk(tree.dependencies)

const fetchFfmpeg = readFileSync('scripts/fetch-ffmpeg.ps1', 'utf8')
const ffmpegVersion = fetchFfmpeg.match(/^\$Version = '([^']+)'/m)[1]
const notice = readFileSync('third-party-licenses/FFmpeg-NOTICE.txt', 'utf8')
const unlisted = [ffmpegVersion, ...[...fetchFfmpeg.matchAll(/Name = '([^']+\.(?:exe|dll))'/g)].map((m) => m[1])]
  .filter((item) => !notice.includes(item))
if (unlisted.length) {
  offenders.push(`ffmpeg：third-party-licenses/FFmpeg-NOTICE.txt 中没有写明 ${unlisted.join('、')}，与 scripts/fetch-ffmpeg.ps1 不一致`)
}

console.log(`已检查 Rust 依赖 ${crates.size} 个、前端生产依赖 ${npmSeen.size} 个，以及 ffmpeg 的许可说明。`)
if (manual.length) console.log(`以下包只给出许可文件，请人工确认：\n  ${manual.join('\n  ')}`)
if (offenders.length) {
  console.error(`发现 ${offenders.length} 个不允许、无法判断或缺少许可的依赖：\n  ${offenders.join('\n  ')}`)
  process.exit(1)
}
console.log('没有 GPL / AGPL / LGPL、无法判断或缺少许可的依赖。')
