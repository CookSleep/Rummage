import { Fragment } from 'react'

/** 路径在每个“/”“\”之后可以换行：手机宽度下长路径按目录分行，不从文件夹名中间断开（单个名称放不下时仍会断开） */
export default function PathText({ path }: { path: string }) {
  return path.split(/(?<=[\\/])/).map((part, i) => (
    <Fragment key={i}>
      {i > 0 && <wbr />}
      {part}
    </Fragment>
  ))
}
