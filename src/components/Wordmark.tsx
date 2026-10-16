import type { SVGProps } from 'react'

/**
 * 首页的单线小写字标“rummage”，a 画成带高光的放大镜镜片。
 * 几何网格：x 高 20–56，描边 7；a、g、e 的字怀半径 18，u 的底弧 15.5，r、m 的拱形 14.5 / 13.5
 */
export default function Wordmark(props: SVGProps<SVGSVGElement>) {
  return (
    <svg viewBox="-2 14 362 70" fill="none" stroke="currentColor" strokeWidth={7} strokeLinecap="round" strokeLinejoin="round" role="img" aria-label="Rummage" {...props}>
      <path d="M3.5 56V20M3.5 34.5A14.5 14.5 0 0 1 18 20H21" />
      <path transform="translate(29 0)" d="M3.5 20V40.5A15.5 15.5 0 0 0 34.5 40.5M34.5 20V56" />
      <path transform="translate(75 0)" d="M3.5 56V20M3.5 33.5A13.5 13.5 0 0 1 30.5 33.5V56M30.5 33.5A13.5 13.5 0 0 1 57.5 33.5V56" />
      <path transform="translate(144 0)" d="M3.5 56V20M3.5 33.5A13.5 13.5 0 0 1 30.5 33.5V56M30.5 33.5A13.5 13.5 0 0 1 57.5 33.5V56" />
      <g transform="translate(213 0)" className="text-blue-500 dark:text-blue-400">
        <circle cx={21.5} cy={38} r={18} />
        <path d="M39.5 20V56" />
        <path d="M12.1 34.6A10 10 0 0 1 18.1 28.6" strokeWidth={4} />
      </g>
      <g transform="translate(264 0)">
        <circle cx={21.5} cy={38} r={18} />
        <path d="M39.5 20V62A16 16 0 0 1 8.46 67.47" />
      </g>
      <path transform="translate(315 0)" d="M3.5 38H39.5A18 18 0 1 0 36.24 48.32" />
    </svg>
  )
}
