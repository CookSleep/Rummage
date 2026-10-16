import { BUTTON, ERROR_TEXT } from './onboarding/styles'

interface Props {
  message: string
  /** 重新读取 */
  onRetry: () => void
  className?: string
}

/** 读取失败的错误条：原因与“重试”。后台服务不可用时不用它，改用 `CoreStatusNotice` */
export default function LoadError({ message, onRetry, className = '' }: Props) {
  return (
    <div className={`flex items-center gap-3 ${className}`}>
      <p data-selectable-text className={`flex-1 ${ERROR_TEXT}`}>{message}</p>
      <button type="button" className={BUTTON} onClick={onRetry}>
        重试
      </button>
    </div>
  )
}
