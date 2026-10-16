import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './index.css'
import App from './App'
import { installBrowserBehaviorGuards, installCloseWatch, isTauri } from './lib/platform'
import { installTauriLog } from './lib/tauriLog'

if (isTauri) {
  installTauriLog()
  installBrowserBehaviorGuards()
  void installCloseWatch().catch((err) => console.error('注册关闭回执失败：', err))
}

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
