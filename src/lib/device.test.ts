import { describe, expect, it } from 'vitest'
import type { DeviceResult, SelftestStatus } from '../types'
import { cpuFallbackReason, cpuFaster, deviceName, indexDevice, switchedDevice, switchedText } from './device'

const dev = (name: string, pciId: string | null, gpu = true): DeviceResult => ({
  id: `gpu:${name}|${pciId ?? ''}`,
  gpu,
  pciId,
  name,
  status: 'passed',
  reason: null,
  textMs: null,
  imageMs: null,
  audioMs: null,
  videoMs: null,
  state: null,
})

describe('deviceName', () => {
  it('只有同型号的显卡附 PCI 号', () => {
    const a = dev('RTX 3070', '0000:01:00.0')
    const b = dev('RTX 3070', '0000:02:00.0')
    const c = dev('Iris Xe', null)
    expect(deviceName(a, [a, b, c])).toBe('RTX 3070（PCI 01:00.0）')
    expect(deviceName(b, [a, b, c])).toBe('RTX 3070（PCI 02:00.0）')
    expect(deviceName(a, [a, c])).toBe('RTX 3070')
    expect(deviceName(c, [a, b, c])).toBe('Iris Xe')
  })
})

describe('cpuFallbackReason', () => {
  const cpu = dev('CPU', null, false)
  const gpu = dev('RTX 3070', null)
  const st = (over: Partial<SelftestStatus>): SelftestStatus => ({
    running: false, preferred: 'auto', preferredError: null, devices: [cpu, gpu], selected: 0, error: null, stale: false, auto: false, failed: null, gpusOff: false, blocked: [], ...over,
  })

  it('有显卡却用 CPU 时按原因区分', () => {
    expect(cpuFallbackReason(st({}))).toBe('failed')
    expect(cpuFallbackReason(st({ stale: true }))).toBe('stale')
    // 原本要用的显卡出了问题而改用 CPU：由 switchedDevice 说明，不算这几种原因
    expect(cpuFallbackReason(st({ failed: gpu.id }))).toBeNull()
    // 显卡出现可能与驱动有关的问题、没有指出具体设备（列出显卡时卡住）：说明原因，即使这次只列出了 CPU
    expect(cpuFallbackReason(st({ gpusOff: true, devices: [cpu] }))).toBe('gpusOff')
    expect(indexDevice(st({ gpusOff: true }))).toEqual({ name: 'CPU', why: '显卡出现问题' })
    expect(cpuFallbackReason(st({ devices: [], selected: null }))).toBe('notDetected')
    // 显卡都已屏蔽（不在检测结果中）：照样说明；只有 CPU 的电脑不算
    expect(cpuFallbackReason(st({ devices: [cpu], blocked: [{ id: gpu.id, name: gpu.name, pciId: null, reason: null }] }))).toBe('blocked')
    expect(cpuFallbackReason(st({ devices: [cpu] }))).toBeNull()
    expect(cpuFallbackReason(st({ running: true, auto: true, selected: null }))).toBe('detecting')
  })

  it('手动检测中（建立索引已暂停）、检测后因 CPU 更快而选用 CPU 时为空', () => {
    expect(cpuFallbackReason(st({ running: true, selected: null }))).toBeNull()
    const fast = { ...cpu, textMs: 10, imageMs: 100 }
    expect(cpuFallbackReason(st({ devices: [fast, { ...gpu, textMs: 5, imageMs: 300 }] }))).toBeNull()
    expect(cpuFallbackReason(st({ devices: [fast, { ...gpu, textMs: 5, imageMs: 50 }] }))).toBe('failed')
  })

  it('用户选了只用 CPU、正在用显卡、电脑没有显卡时为空', () => {
    expect(cpuFallbackReason(st({ preferred: 'cpu' }))).toBeNull()
    expect(cpuFallbackReason(st({ selected: 1 }))).toBeNull()
    expect(cpuFallbackReason(st({ devices: [cpu] }))).toBeNull()
    expect(cpuFallbackReason(null)).toBeNull()
  })
})

describe('cpuFaster', () => {
  const st = (devices: DeviceResult[]): SelftestStatus => ({
    running: false, preferred: 'auto', preferredError: null, devices, selected: 0, error: null, stale: false, auto: true, failed: null, gpusOff: false, blocked: [],
  })
  const cpu = { ...dev('CPU', null, false), textMs: 10, imageMs: 100 }

  it('按图片耗时比较，没有图片时按文字；未通过的显卡不算', () => {
    expect(cpuFaster(st([cpu, { ...dev('A', null), textMs: 5, imageMs: 300 }]))).toBe(true)
    expect(cpuFaster(st([cpu, { ...dev('A', null), textMs: 5, imageMs: 50 }]))).toBe(false)
    const textOnly = { ...cpu, imageMs: null }
    expect(cpuFaster(st([textOnly, { ...dev('A', null), textMs: 20 }]))).toBe(true)
    expect(cpuFaster(st([cpu, { ...dev('A', null), status: 'failed', imageMs: 10 }]))).toBe(false)
    expect(cpuFaster(st([cpu]))).toBe(false)
  })
})

describe('switchedDevice', () => {
  const cpu = dev('CPU', null, false)
  const a = dev('RTX 4080 SUPER', null)
  const b = dev('RTX 3070', null)
  const st = (over: Partial<SelftestStatus>): SelftestStatus => ({
    running: false, preferred: 'auto', preferredError: null, devices: [cpu, a, b], selected: 2, error: null, stale: false, auto: false, failed: a.id, gpusOff: false, blocked: [], ...over,
  })

  it('说明哪个设备出了问题、改用了哪个设备', () => {
    expect(switchedDevice(st({}))).toEqual({ from: 'RTX 4080 SUPER', to: 'RTX 3070' })
    expect(indexDevice(st({}))).toEqual({ name: 'RTX 3070', why: '「RTX 4080 SUPER」出现问题' })
    expect(indexDevice(st({ selected: 0 }))).toEqual({ name: 'CPU', why: '「RTX 4080 SUPER」出现问题' })
    expect(switchedText({ from: 'RTX 4080 SUPER', to: 'CPU' })).toBe('「RTX 4080 SUPER」出现问题，已改用「CPU」。')
    // 出问题的显卡之后被屏蔽：不在检测结果中，名称取自已屏蔽的显卡
    const blocked = st({ devices: [cpu, b], selected: 1, blocked: [{ id: a.id, name: a.name, pciId: null, reason: null }] })
    expect(switchedDevice(blocked)).toEqual({ from: 'RTX 4080 SUPER', to: 'RTX 3070' })
  })

  it('没有出问题、选了只用 CPU、正在检测时为空；其余仍是用 CPU 的原因', () => {
    expect(switchedDevice(st({ failed: null }))).toBeNull()
    expect(switchedDevice(st({ preferred: 'cpu' }))).toBeNull()
    expect(switchedDevice(st({ running: true }))).toBeNull()
    expect(indexDevice(st({ failed: null, selected: 0 }))).toEqual({ name: 'CPU', why: '显卡检测未通过' })
    expect(indexDevice(st({ failed: null }))).toBeNull()
  })
})