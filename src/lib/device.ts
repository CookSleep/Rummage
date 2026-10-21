import type { BlockedGpu, DeviceResult, ParamValues, SelftestStatus, SetupStatus } from '../types'
import { isAndroid } from './platform'

/** 显示名称用到的字段，检测结果与被屏蔽的显卡都有 */
type Named = Pick<DeviceResult, 'gpu' | 'pciId' | 'name'>

/** CPU 的名称（检测结果中的处理器型号）；还没有检测过时为“CPU” */
export function cpuName(st: SelftestStatus) {
  return st.devices.find((d) => !d.gpu)?.name ?? 'CPU'
}

/**
 * 比较设备速度用的耗时，与核心的选择规则相同：通过的设备都有测速结果、且测速时的参数相同时按测速结果，
 * 否则按黄金样本的耗时；有图片耗时按图片，否则按文字
 */
export function speedMs(passed: DeviceResult[]) {
  const params = (d: DeviceResult) => JSON.stringify(d.speed?.params ?? null)
  const real = passed.every((d) => (d.speed?.imageMs ?? d.speed?.textMs) != null && params(d) === params(passed[0]))
  return (d: DeviceResult) => (real && d.speed ? d.speed.imageMs ?? d.speed.textMs : d.imageMs ?? d.textMs)
}

/** 检测通过的显卡都比 CPU 慢（见 `speedMs`），所以选用了 CPU */
export function cpuFaster(st: SelftestStatus) {
  const ms = speedMs(st.devices.filter((d) => d.status === 'passed'))
  const cpu = st.devices.find((d) => !d.gpu && d.status === 'passed')
  const gpus = st.devices.filter((d) => d.gpu && d.status === 'passed')
  const t = cpu && ms(cpu)
  return t != null && gpus.length > 0 && gpus.every((g) => (ms(g) ?? Infinity) >= t)
}

/** 测速之后改过其中用到的参数：显示的耗时与预估用的速度都是修改前的。`values` 中没有的参数不算；显卡的耗时与线程数无关 */
export function speedStale(d: DeviceResult, values: ParamValues | null) {
  if (!d.speed || !values) return false
  return Object.entries(d.speed.params).some(([k, v]) => !(d.gpu && k === 'index_threads') && values[k] !== undefined && values[k] !== v)
}

/** 检测结果中的耗时是修改参数前测得的 */
export const SPEED_STALE_TEXT = '参数已修改，以下耗时为修改前的结果，重新检测后更新'

/** 引导与设备页共用的检测设备说明。Android 上按装好的模型说明测哪些设备：CPU 与 GPU 用通用模型，NPU 用 NPU 模型 */
export function selftestText(s: SetupStatus) {
  if (!isAndroid) return '用 CPU 和每块显卡计算几条样本，确认结果正确并测速，选用最快的设备。检测约需几分钟。'
  const npu = Boolean(s.npu?.ready)
  if (npu && !s.model.ready) return '用 NPU 计算几条样本，确认结果正确并测速。检测约需几分钟。CPU 和 GPU 要用通用模型，下载后才会检测。'
  return `用 ${npu ? 'CPU、GPU 和 NPU' : 'CPU 和 GPU'} 计算几条样本，确认结果正确并测速，选用最快的设备。检测约需几分钟。`
}


/** 用户没有选择只用 CPU，却用 CPU 建立索引的原因，见 `cpuFallbackReason` */
export type CpuFallback = 'detecting' | 'gpusOff' | 'notDetected' | 'stale' | 'failed' | 'blocked'

/** 原因的文字，放在“正在用 CPU 建立索引”后的括号中 */
export const CPU_FALLBACK_TEXT: Record<CpuFallback, string> = isAndroid
  ? {
      detecting: '正在检测 GPU 与 NPU',
      gpusOff: 'GPU 或 NPU 出现问题',
      notDetected: '尚未检测 GPU 与 NPU',
      stale: '尚未重新检测 GPU 与 NPU',
      failed: 'GPU 与 NPU 检测未通过',
      blocked: 'GPU 与 NPU 已屏蔽',
    }
  : {
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
