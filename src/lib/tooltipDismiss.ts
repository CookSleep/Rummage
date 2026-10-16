// 收起所有说明气泡的全局通知：打开弹窗前调用，气泡不必知道是哪个弹窗

const bus = new EventTarget()
const EVENT_NAME = 'dismiss-tooltips'

export function dismissAllTooltips() {
  bus.dispatchEvent(new Event(EVENT_NAME))
}

/** 返回取消订阅的函数 */
export function onDismissTooltips(callback: () => void) {
  bus.addEventListener(EVENT_NAME, callback)
  return () => bus.removeEventListener(EVENT_NAME, callback)
}
