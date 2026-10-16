import type { ApiError } from '../types'

/** 后台服务与 Rust 命令返回的错误是带 message 的对象（ApiError），前端抛出的是 Error，两者都取 message */
export function errorMessage(err: unknown, fallback: string) {
  return (err as ApiError)?.message ?? fallback
}
