/// <reference types="vitest/config" />
import { readFileSync } from 'fs'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

const pkg = JSON.parse(readFileSync('./package.json', 'utf-8'))

export default defineConfig({
  plugins: [react()],
  base: './',
  // 设置页标题旁显示的版本号
  define: { __APP_VERSION__: JSON.stringify(pkg.version) },
  server: {
    // Tauri 开发模式的 devUrl 固定为 5173，端口被占用时直接报错而不是换端口
    strictPort: Boolean(process.env.TAURI_ENV_PLATFORM),
    watch: { ignored: ['**/src-tauri/**', '**/target/**', '**/.cache/**', '**/crates/**', '**/third_party/**'] },
  },
  // .cache 中有 llama.cpp 源码自带的测试，只收集本项目的测试
  test: { include: ['src/**/*.test.{ts,tsx}'] },
})
