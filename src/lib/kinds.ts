import type { ParamValues } from '../types'

// 引导与设置中的“文件类型”开关

/** 开启内容索引的类型，对应参数 `index_documents`、`index_images`、`index_audio`、`index_video` */
export interface Kinds {
  documents: boolean
  images: boolean
  audio: boolean
  video: boolean
}

/** 各类型对应的参数 */
export const KIND_PARAMS: Record<keyof Kinds, string> = {
  documents: 'index_documents',
  images: 'index_images',
  audio: 'index_audio',
  video: 'index_video',
}

/** 参数值换成类型开关；没有保存过的参数默认开启 */
export function kindsOf(values: ParamValues): Kinds {
  return {
    documents: values.index_documents !== false,
    images: values.index_images !== false,
    audio: values.index_audio !== false,
    video: values.index_video !== false,
  }
}

/** 图片、音频和视频要用多模态模型 */
export function needsMmproj(kinds: Kinds) {
  return kinds.images || kinds.audio || kinds.video
}

/**
 * 视频识别方式，对应参数 `video_preset` 的三个预设；相对耗时与代价同参数表的说明一致。
 * `windowS`、`frames` 为每段视频的秒数与帧数，与 rummage-core `pipeline::VIDEO_PRESETS` 一致，预估索引时间用
 */
export const VIDEO_PRESETS = [
  { value: 1, title: '找物体', desc: '找画面里出现过的人和东西，定位最准，用时最短', windowS: 2, frames: 2 },
  { value: 2, title: '找动作', desc: '能看出动作和前后变化，用时与“找物体”相近', windowS: 4, frames: 4 },
  { value: 3, title: '找说话内容', desc: '还能按说话内容找，画面查找稍差，用时约为“找物体”的 1.4 倍', windowS: 6, frames: 6 },
]
