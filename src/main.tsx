import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './index.css'
import App from './App'
import { installBackButton } from './lib/back'
import { installBrowserBehaviorGuards, installCloseWatch, isAndroid, isTauri } from './lib/platform'
import { installSafeArea } from './lib/safeArea'
import { installTauriLog } from './lib/tauriLog'

if (isTauri) {
  installTauriLog()
  installBrowserBehaviorGuards()
  void installCloseWatch().catch((err) => console.error('注册关闭回执失败：', err))
}
if (isTauri && isAndroid) {
  installSafeArea()
  void installBackButton().catch((err) => console.error('注册返回键处理失败：', err))
}

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
