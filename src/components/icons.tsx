import type { SVGProps } from 'react'

// 内联 SVG 图标，描边 2

function Icon({ d, ...props }: SVGProps<SVGSVGElement> & { d: string }) {
  return (
    <svg fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true" {...props}>
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d={d} />
    </svg>
  )
}

export function SearchIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M21 21l-4.35-4.35M11 18a7 7 0 100-14 7 7 0 000 14z" />
}

export function CloseIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M6 18L18 6M6 6l12 12" />
}

export function ImageIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M4 16l4.586-4.586a2 2 0 012.828 0L16 16m-2-2l1.586-1.586a2 2 0 012.828 0L20 14m-6-6h.01M6 20h12a2 2 0 002-2V6a2 2 0 00-2-2H6a2 2 0 00-2 2v12a2 2 0 002 2z" />
}

export function DocumentIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M9 12h6m-6 4h6m2 5H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z" />
}

export function FileIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M7 21h10a2 2 0 002-2V9.414a1 1 0 00-.293-.707l-5.414-5.414A1 1 0 0012.586 3H7a2 2 0 00-2 2v14a2 2 0 002 2z" />
}

export function CloudIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M3 15a4 4 0 004 4h9a5 5 0 10-.1-9.999 5.002 5.002 0 10-9.78 2.096A4.001 4.001 0 003 15z" />
}

export function UnplugIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M18.364 5.636a9 9 0 010 12.728M5.636 18.364a9 9 0 010-12.728M3 3l18 18" />
}

export function OpenIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M10 6H6a2 2 0 00-2 2v10a2 2 0 002 2h10a2 2 0 002-2v-4M14 4h6m0 0v6m0-6L10 14" />
}

export function ShareIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M8.59 13.51l6.83 3.98m-.01-10.98l-6.82 3.98M21 5a3 3 0 11-6 0 3 3 0 016 0zM9 12a3 3 0 11-6 0 3 3 0 016 0zm12 7a3 3 0 11-6 0 3 3 0 016 0z" />
}

export function FolderIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M3 7a2 2 0 012-2h4l2 2h8a2 2 0 012 2v8a2 2 0 01-2 2H5a2 2 0 01-2-2V7z" />
}

export function KindIcon({ kind, ...props }: SVGProps<SVGSVGElement> & { kind: string }) {
  if (kind === 'image') return <ImageIcon {...props} />
  if (kind === 'document') return <DocumentIcon {...props} />
  return <FileIcon {...props} />
}

export function PanelCollapseIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M13 5l7 7-7 7M5 5l7 7-7 7" />
}

export function PanelExpandIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M11 19l-7-7 7-7m8 14l-7-7 7-7" />
}

export function CheckIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M5 13l4 4L19 7" />
}

export function PlusIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M12 4v16m8-8H4" />
}

export function DownloadIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4" />
}

export function CubeIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M20 7l-8-4-8 4m16 0l-8 4m8-4v10l-8 4m0-10L4 7m8 4v10M4 7v10l8 4" />
}

export function CpuIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M9 3v2m6-2v2M9 19v2m6-2v2M5 9H3m2 6H3m18-6h-2m2 6h-2M7 19h10a2 2 0 002-2V7a2 2 0 00-2-2H7a2 2 0 00-2 2v10a2 2 0 002 2zM9 9h6v6H9V9z" />
}

/** 显卡：挡板、卡身、两个风扇与金手指 */
export function GpuIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M2 21V3m0 2h18a2 2 0 012 2v8a2 2 0 01-2 2H2m5 0v3a1 1 0 001 1h5a1 1 0 001-1v-3M8 13a2 2 0 100-4 2 2 0 000 4zm8 0a2 2 0 100-4 2 2 0 000 4z" />
}

/** NPU：芯片中一颗四角星（AI 加速） */
export function NpuIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M9 3v2m6-2v2M9 19v2m6-2v2M5 9H3m2 6H3m18-6h-2m2 6h-2M7 19h10a2 2 0 002-2V7a2 2 0 00-2-2H7a2 2 0 00-2 2v10a2 2 0 002 2zm5-10l.9 2.1L15 12l-2.1.9L12 15l-.9-2.1L9 12l2.1-.9L12 9z" />
}

export function AudioIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M9 19V6l12-3v13M9 19c0 1.105-1.343 2-3 2s-3-.895-3-2 1.343-2 3-2 3 .895 3 2zm12-3c0 1.105-1.343 2-3 2s-3-.895-3-2 1.343-2 3-2 3 .895 3 2zM9 10l12-3" />
}

export function VideoIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M15 10l4.553-2.276A1 1 0 0121 8.618v6.764a1 1 0 01-1.447.894L15 14M5 18h8a2 2 0 002-2V8a2 2 0 00-2-2H5a2 2 0 00-2 2v8a2 2 0 002 2z" />
}

export function WarningIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z" />
}

export function EyeIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8zM12 15a3 3 0 100-6 3 3 0 000 6z" />
}

export function EyeOffIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M17.94 17.94A10.07 10.07 0 0112 20c-7 0-11-8-11-8a18.45 18.45 0 015.06-5.94M9.9 4.24A9.12 9.12 0 0112 4c7 0 11 8 11 8a18.5 18.5 0 01-2.16 3.19M14.12 14.12a3 3 0 11-4.24-4.24M1 1l22 22" />
}

export function SettingsIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M10.325 4.317c.426-1.756 2.924-1.756 3.35 0a1.724 1.724 0 002.573 1.066c1.543-.94 3.31.826 2.37 2.37a1.724 1.724 0 001.065 2.572c1.756.426 1.756 2.924 0 3.35a1.724 1.724 0 00-1.066 2.573c.94 1.543-.826 3.31-2.37 2.37a1.724 1.724 0 00-2.572 1.065c-.426 1.756-2.924 1.756-3.35 0a1.724 1.724 0 00-2.573-1.066c-1.543.94-3.31-.826-2.37-2.37a1.724 1.724 0 00-1.065-2.572c-1.756-.426-1.756-2.924 0-3.35a1.724 1.724 0 001.066-2.573c-.94-1.543.826-3.31 2.37-2.37.996.608 2.296.07 2.572-1.065zM15 12a3 3 0 11-6 0 3 3 0 016 0z" />
}

export function ChevronDownIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M6 9l6 6 6-6" />
}

export function InfoIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M13 16h-1v-4h-1m1-4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
}

export function ResetIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15" />
}

export function DatabaseIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M4 7v10c0 2.21 3.582 4 8 4s8-1.79 8-4V7M4 7c0 2.21 3.582 4 8 4s8-1.79 8-4M4 7c0-2.21 3.582-4 8-4s8 1.79 8 4" />
}

export function StorageIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M5 12h14M5 12a2 2 0 01-2-2V6a2 2 0 012-2h14a2 2 0 012 2v4a2 2 0 01-2 2M5 12a2 2 0 00-2 2v4a2 2 0 002 2h14a2 2 0 002-2v-4a2 2 0 00-2-2m-2-4h.01M17 16h.01" />
}

export function GaugeIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M13 10V3L4 14h7v7l9-11h-7z" />
}

export function GlobeIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M21 12a9 9 0 01-9 9m9-9a9 9 0 00-9-9m9 9H3m9 9a9 9 0 01-9-9m9 9c1.657 0 3-4.03 3-9s-1.343-9-3-9m0 18c-1.657 0-3-4.03-3-9s1.343-9 3-9m-9 9a9 9 0 019-9" />
}

export function PaletteIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M7 21a4 4 0 01-4-4V5a2 2 0 012-2h4a2 2 0 012 2v12a4 4 0 01-4 4zm0 0h12a2 2 0 002-2v-4a2 2 0 00-2-2h-2.343M11 7.343l1.657-1.657a2 2 0 012.828 0l2.829 2.829a2 2 0 010 2.828l-8.486 8.485M7 17h.01" />
}

export function SlidersIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M12 6V4m0 2a2 2 0 100 4m0-4a2 2 0 110 4m-6 8a2 2 0 100-4m0 4a2 2 0 110-4m0 4v2m0-6V4m6 6v10m6-2a2 2 0 100-4m0 4a2 2 0 110-4m0 4v2m0-6V4" />
}

export function ChevronRightIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M9 6l6 6-6 6" />
}

export function ExternalLinkIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M10 6H6a2 2 0 00-2 2v10a2 2 0 002 2h10a2 2 0 002-2v-4M14 4h6m0 0v6m0-6L10 14" />
}

export function HeartIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M4.318 6.318a4.5 4.5 0 000 6.364L12 20.364l7.682-7.682a4.5 4.5 0 00-6.364-6.364L12 7.636l-1.318-1.318a4.5 4.5 0 00-6.364 0z" />
}

export function SparklesIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M5 3v4M3 5h4M6 17v4m-2-2h4m5-16l2.286 6.857L21 12l-5.714 2.143L13 21l-2.286-6.857L5 12l5.714-2.143L13 3z" />
}

export function CopyIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M8 16H6a2 2 0 01-2-2V6a2 2 0 012-2h8a2 2 0 012 2v2m-6 12h8a2 2 0 002-2v-8a2 2 0 00-2-2h-8a2 2 0 00-2 2v8a2 2 0 002 2z" />
}

export function ChatIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M8 10h.01M12 10h.01M16 10h.01M9 16H5a2 2 0 01-2-2V6a2 2 0 012-2h14a2 2 0 012 2v8a2 2 0 01-2 2h-5l-5 5v-5z" />
}

export function ScaleIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M3 6l3 1m0 0l-3 9a5.002 5.002 0 006.001 0M6 7l3 9M6 7l6-2m6 2l3-1m-3 1l-3 9a5.002 5.002 0 006.001 0M18 7l3 9m-3-9l-6-2m0-2v2m0 16V5m0 16H9m3 0h3" />
}

/** GitHub 标志，实心填充 */
export function GithubIcon(props: SVGProps<SVGSVGElement>) {
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true" {...props}>
      <path d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0112 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.019 10.019 0 0022 12.017C22 6.484 17.522 2 12 2z" />
    </svg>
  )
}

export function CheckCircleIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z" />
}

export function XCircleIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M10 14l2-2m0 0l2-2m-2 2l-2-2m2 2l2 2m7-2a9 9 0 11-18 0 9 9 0 0118 0z" />
}

export function BanIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M18.364 18.364A9 9 0 005.636 5.636m12.728 12.728A9 9 0 015.636 5.636m12.728 12.728L5.636 5.636" />
}

export function MinusCircleIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M15 12H9m12 0a9 9 0 11-18 0 9 9 0 0118 0z" />
}

export function ClockIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M12 8v4l3 3m6-3a9 9 0 11-18 0 9 9 0 0118 0z" />
}

export function PauseIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M10 9v6m4-6v6m7-3a9 9 0 11-18 0 9 9 0 0118 0z" />
}

export function PlayIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M14.752 11.168l-3.197-2.132A1 1 0 0010 9.87v4.263a1 1 0 001.555.832l3.197-2.132a1 1 0 000-1.664zM21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
}

export function SpinnerIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M12 3a9 9 0 109 9" />
}

export function TrashIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M19 7l-.867 12.142A2 2 0 0116.138 21H7.862a2 2 0 01-1.995-1.858L5 7m5 4v6m4-6v6m1-10V4a1 1 0 00-1-1h-4a1 1 0 00-1 1v3M4 7h16" />
}

export function FilterIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M3 4a1 1 0 011-1h16a1 1 0 011 1v2.586a1 1 0 01-.293.707l-6.414 6.414a1 1 0 00-.293.707V17l-4 4v-6.586a1 1 0 00-.293-.707L3.293 7.293A1 1 0 013 6.586V4z" />
}

/** 扫帚：清除勾选 */
export function BroomIcon(props: SVGProps<SVGSVGElement>) {
  return <Icon {...props} d="M16 22l-1-4M8 22l1-4M19 14a1 1 0 001-1v-1a2 2 0 00-2-2h-3a1 1 0 01-1-1V4a2 2 0 00-4 0v5a1 1 0 01-1 1H6a2 2 0 00-2 2v1a1 1 0 001 1M5 14h14l1.973 6.767A1 1 0 0120 22H4a1 1 0 01-.973-1.233L5 14z" />
}

/** 找相似的图标右上角的放大镜 */
const SIMILAR_LENS = 'M17 4a3 3 0 100 6 3 3 0 000-6zm2.2 5.2l2.3 2.3'

/** 找相似的：按结果的种类画图片、文档页、波形或带播放键的画面，右上角叠一个放大镜；边框在放大镜处断开，不与它交叠 */
export function SimilarIcon({ kind, ...props }: SVGProps<SVGSVGElement> & { kind: string }) {
  if (kind === 'document') return <Icon {...props} d={`M12.5 7H6a2 2 0 00-2 2v10a2 2 0 002 2h8a2 2 0 002-2v-7.5M7.5 10.5h3M7.5 14h5m-5 3.5h5${SIMILAR_LENS}`} />
  if (kind === 'audio') return <Icon {...props} d={`M4 12.5v3M7 9.5v9M10 7.5v13M13 11v6M16 12.5v3${SIMILAR_LENS}`} />
  if (kind === 'video') return <Icon {...props} d={`M12.5 7H5a2 2 0 00-2 2v10a2 2 0 002 2h10a2 2 0 002-2v-7.5M8.5 11v6l4.5-3-4.5-3z${SIMILAR_LENS}`} />
  return <Icon {...props} d={`M12.5 7H5a2 2 0 00-2 2v10a2 2 0 002 2h10a2 2 0 002-2v-7.5M3 17.5l4.01-4.01a1.75 1.75 0 012.475 0L13.5 17.5m-1.75-1.75l1.39-1.39a1.75 1.75 0 012.475 0L17 15.75M6.5 10.5h.01${SIMILAR_LENS}`} />
}

// AI 工具的品牌图标（单色），填充绘制；VS Code 取自 Devicon 2.17.0（MIT），其余取自 Lobe Icons（@lobehub/icons 6.0.0，MIT）
function BrandIcon({ d, viewBox = '0 0 24 24', ...props }: SVGProps<SVGSVGElement> & { d: string }) {
  return (
    <svg fill="currentColor" fillRule="evenodd" viewBox={viewBox} aria-hidden="true" {...props}>
      <path clipRule="evenodd" d={d} />
    </svg>
  )
}

export function ClaudeCodeIcon(props: SVGProps<SVGSVGElement>) {
  return <BrandIcon {...props} d="M20.998 10.949H24v3.102h-3v3.028h-1.487V20H18v-2.921h-1.487V20H15v-2.921H9V20H7.488v-2.921H6V20H4.487v-2.921H3V14.05H0V10.95h3V5h17.998v5.949zM6 10.949h1.488V8.102H6v2.847zm10.51 0H18V8.102h-1.49v2.847z" />
}

export function CodexIcon(props: SVGProps<SVGSVGElement>) {
  return <BrandIcon {...props} d="M8.086.457a6.105 6.105 0 013.046-.415c1.333.153 2.521.72 3.564 1.7a.117.117 0 00.107.029c1.408-.346 2.762-.224 4.061.366l.063.03.154.076c1.357.703 2.33 1.77 2.918 3.198.278.679.418 1.388.421 2.126a5.655 5.655 0 01-.18 1.631.167.167 0 00.04.155 5.982 5.982 0 011.578 2.891c.385 1.901-.01 3.615-1.183 5.14l-.182.22a6.063 6.063 0 01-2.934 1.851.162.162 0 00-.108.102c-.255.736-.511 1.364-.987 1.992-1.199 1.582-2.962 2.462-4.948 2.451-1.583-.008-2.986-.587-4.21-1.736a.145.145 0 00-.14-.032c-.518.167-1.04.191-1.604.185a5.924 5.924 0 01-2.595-.622 6.058 6.058 0 01-2.146-1.781c-.203-.269-.404-.522-.551-.821a7.74 7.74 0 01-.495-1.283 6.11 6.11 0 01-.017-3.064.166.166 0 00.008-.074.115.115 0 00-.037-.064 5.958 5.958 0 01-1.38-2.202 5.196 5.196 0 01-.333-1.589 6.915 6.915 0 01.188-2.132c.45-1.484 1.309-2.648 2.577-3.493.282-.188.55-.334.802-.438.286-.12.573-.22.861-.304a.129.129 0 00.087-.087A6.016 6.016 0 015.635 2.31C6.315 1.464 7.132.846 8.086.457zm-.804 7.85a.848.848 0 00-1.473.842l1.694 2.965-1.688 2.848a.849.849 0 001.46.864l1.94-3.272a.849.849 0 00.007-.854l-1.94-3.393zm5.446 6.24a.849.849 0 000 1.695h4.848a.849.849 0 000-1.696h-4.848z" />
}

export function ClaudeIcon(props: SVGProps<SVGSVGElement>) {
  return <BrandIcon {...props} d="M4.709 15.955l4.72-2.647.08-.23-.08-.128H9.2l-.79-.048-2.698-.073-2.339-.097-2.266-.122-.571-.121L0 11.784l.055-.352.48-.321.686.06 1.52.103 2.278.158 1.652.097 2.449.255h.389l.055-.157-.134-.098-.103-.097-2.358-1.596-2.552-1.688-1.336-.972-.724-.491-.364-.462-.158-1.008.656-.722.881.06.225.061.893.686 1.908 1.476 2.491 1.833.365.304.145-.103.019-.073-.164-.274-1.355-2.446-1.446-2.49-.644-1.032-.17-.619a2.97 2.97 0 01-.104-.729L6.283.134 6.696 0l.996.134.42.364.62 1.414 1.002 2.229 1.555 3.03.456.898.243.832.091.255h.158V9.01l.128-1.706.237-2.095.23-2.695.08-.76.376-.91.747-.492.584.28.48.685-.067.444-.286 1.851-.559 2.903-.364 1.942h.212l.243-.242.985-1.306 1.652-2.064.73-.82.85-.904.547-.431h1.033l.76 1.129-.34 1.166-1.064 1.347-.881 1.142-1.264 1.7-.79 1.36.073.11.188-.02 2.856-.606 1.543-.28 1.841-.315.833.388.091.395-.328.807-1.969.486-2.309.462-3.439.813-.042.03.049.061 1.549.146.662.036h1.622l3.02.225.79.522.474.638-.079.485-1.215.62-1.64-.389-3.829-.91-1.312-.329h-.182v.11l1.093 1.068 2.006 1.81 2.509 2.33.127.578-.322.455-.34-.049-2.205-1.657-.851-.747-1.926-1.62h-.128v.17l.444.649 2.345 3.521.122 1.08-.17.353-.608.213-.668-.122-1.374-1.925-1.415-2.167-1.143-1.943-.14.08-.674 7.254-.316.37-.729.28-.607-.461-.322-.747.322-1.476.389-1.924.315-1.53.286-1.9.17-.632-.012-.042-.14.018-1.434 1.967-2.18 2.945-1.726 1.845-.414.164-.717-.37.067-.662.401-.589 2.388-3.036 1.44-1.882.93-1.086-.006-.158h-.055L4.132 18.56l-1.13.146-.487-.456.061-.746.231-.243 1.908-1.312-.006.006z" />
}

export function CursorIcon(props: SVGProps<SVGSVGElement>) {
  return <BrandIcon {...props} d="M22.106 5.68L12.5.135a.998.998 0 00-.998 0L1.893 5.68a.84.84 0 00-.419.726v11.186c0 .3.16.577.42.727l9.607 5.547a.999.999 0 00.998 0l9.608-5.547a.84.84 0 00.42-.727V6.407a.84.84 0 00-.42-.726zm-.603 1.176L12.228 22.92c-.063.108-.228.064-.228-.061V12.34a.59.59 0 00-.295-.51l-9.11-5.26c-.107-.062-.063-.228.062-.228h18.55c.264 0 .428.286.296.514z" />
}

export function OpenCodeIcon(props: SVGProps<SVGSVGElement>) {
  return <BrandIcon {...props} d="M16 6H8v12h8V6zm4 16H4V2h16v20z" />
}

export function VscodeIcon(props: SVGProps<SVGSVGElement>) {
  return <BrandIcon {...props} viewBox="0 0 128 128" d="M90.767 127.126a7.968 7.968 0 0 0 6.35-.244l26.353-12.681a8 8 0 0 0 4.53-7.209V21.009a8 8 0 0 0-4.53-7.21L97.117 1.12a7.97 7.97 0 0 0-9.093 1.548l-50.45 46.026L15.6 32.013a5.328 5.328 0 0 0-6.807.302l-7.048 6.411a5.335 5.335 0 0 0-.006 7.888L20.796 64 1.74 81.387a5.336 5.336 0 0 0 .006 7.887l7.048 6.411a5.327 5.327 0 0 0 6.807.303l21.974-16.68 50.45 46.025a7.96 7.96 0 0 0 2.743 1.793Zm5.252-92.183L57.74 64l38.28 29.058V34.943Z" />
}
