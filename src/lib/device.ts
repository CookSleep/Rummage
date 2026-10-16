import type { BlockedGpu, DeviceResult, SelftestStatus } from '../types'
import { isAndroid } from './platform'

/** 显示名称用到的字段，检测结果与被屏蔽的显卡都有 */
type Named = Pick<DeviceResult, 'gpu' | 'pciId' | 'name'>

/** CPU 的名称（检测结果中的处理器型号）；还没有检测过时为“CPU” */
export function cpuName(st: SelftestStatus) {
  return st.devices.find((d) => !d.gpu)?.name ?? 'CPU'
}

/** 检测通过的显卡都比 CPU 慢（与核心的选择规则相同：有图片耗时按图片，否则按文字），所以选用了 CPU */
export function cpuFaster(st: SelftestStatus) {
  const ms = (d: DeviceResult) => d.imageMs ?? d.textMs
  const cpu = st.devices.find((d) => !d.gpu && d.status === 'passed')
  const gpus = st.devices.filter((d) => d.gpu && d.status === 'passed')
  const t = cpu && ms(cpu)
  return t != null && gpus.length > 0 && gpus.every((g) => (ms(g) ?? Infinity) >= t)
}

/** 引导与设备页共用的检测设备说明 */
export const SELFTEST_TEXT = isAndroid
  ? '用 CPU 计算几条样本，确认结果正确并测速。检测约需几分钟。'
  : '用 CPU 和每块显卡计算几条样本，确认结果正确并测速，选用最快的设备。检测约需几分钟。'

/** 用户没有选择只用 CPU，却用 CPU 建立索引的原因，见 `cpuFallbackReason` */
export type CpuFallback = 'detecting' | 'gpusOff' | 'notDetected' | 'stale' | 'failed' | 'blocked'

/** 原因的文字，放在“正在用 CPU 建立索引”后的括号中 */
export const CPU_FALLBACK_TEXT: Record<CpuFallback, string> = {
  detecting: '正在检测显卡',
  gpusOff: '显卡出现问题',
  notDetected: '尚未检测显卡',
  stale: '尚未重新检测显卡',
  failed: '显卡检测未通过',
  blocked: '显卡已屏蔽',
}

/**
 * 原本要用的设备本次运行中出了问题、已改用另一个设备（核心给出 `failed`，`selected` 为改用的设备）时的两个设备名；
 * 不是这种情况、选了只用 CPU 或正在检测时为空
 */
export function switchedDevice(st: SelftestStatus | null | undefined): { from: string; to: string } | null {
  if (!st || st.preferred === 'cpu' || st.running || st.failed === null) return null
  const to = st.selected !== null ? st.devices[st.selected] : undefined
  const from = st.devices.find((d) => d.id === st.failed)
  if (!to) return null
  if (from) return { from: deviceName(from, st.devices), to: deviceName(to, st.devices) }
  const b = st.blocked.find((x) => x.id === st.failed)
  return b ? { from: blockedName(b, st), to: deviceName(to, st.devices) } : null
}

/** 设备页、引导中说明换了设备的整句；设备名用「」 */
export function switchedText(s: { from: string; to: string }) {
  return `「${s.from}」出现问题，已改用「${s.to}」。`
}

/**
 * 用户没有选择只用 CPU，却用 CPU 建立索引的原因；不是这种情况时为空。
 * 不算：只有 CPU 的电脑（检测过且没有显卡）、检测后因 CPU 更快而选用 CPU、手动检测中（建立索引已暂停）、
 * 原本要用的设备出了问题而改用 CPU（见 `switchedDevice`）。
 * 结果过期（`stale`）多为自动检测还没开始，或出错、被中止后不再重试
 */
export function cpuFallbackReason(st: SelftestStatus | null | undefined): CpuFallback | null {
  if (!st || st.preferred === 'cpu') return null
  if (st.running) return st.auto ? 'detecting' : null
  if (st.failed !== null) return null
  // 本次运行显卡出现可能与驱动有关的问题（如列出显卡时卡住），所有显卡都停用
  if (st.gpusOff) return 'gpusOff'
  if (!st.devices.length) return 'notDetected'
  const d = st.selected !== null ? st.devices[st.selected] : undefined
  // 只有 CPU 的电脑不算；显卡都已屏蔽时照样说明
  if (d?.gpu || cpuFaster(st) || (!st.devices.some((x) => x.gpu) && !st.blocked.length)) return null
  if (st.stale) return 'stale'
  // 检测结果中没有显卡，可用的显卡只有已屏蔽的
  return st.devices.some((x) => x.gpu) ? 'failed' : 'blocked'
}

/**
 * 主页索引提示“正在用「{设备}」建立索引（{原因}）”的设备与原因：原本要用的设备出了问题时为改用的设备，
 * 否则为用户没选只用 CPU 却用 CPU 时的 CPU 与原因；都不是时为空
 */
export function indexDevice(st: SelftestStatus | null | undefined): { name: string; why: string } | null {
  const s = switchedDevice(st)
  if (s) return { name: s.to, why: `「${s.from}」出现问题` }
  const r = cpuFallbackReason(st)
  return r && st ? { name: cpuName(st), why: CPU_FALLBACK_TEXT[r] } : null
}

/** 已屏蔽的显卡的名称；与检测结果及其他已屏蔽的显卡同型号时附 PCI 号 */
export function blockedName(b: BlockedGpu, st: SelftestStatus) {
  // 本次测过、未通过的已屏蔽显卡同时在检测结果中，只算一次
  const all: Named[] = [...st.devices, ...st.blocked.filter((x) => !st.devices.some((d) => d.id === x.id)).map((x) => ({ ...x, gpu: true }))]
  return deviceName({ ...b, gpu: true }, all)
}

/** 设备名称；有同型号的显卡时附上 PCI 号区分（去掉开头的域号 `0000:`），如“RTX 3070（PCI 01:00.0）” */
export function deviceName(d: Named, all: Named[]) {
  if (!d.gpu || !d.pciId || all.filter((x) => x.gpu && x.name === d.name).length < 2) return d.name
  return `${d.name}（PCI ${d.pciId.replace(/^0000:/, '')}）`
}
