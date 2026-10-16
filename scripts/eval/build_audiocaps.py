"""文本 → 声音事件：AudioCaps 多语言版（`piyushsinghpasi/audiocaps-multilingual`）测试集。

- 957 段 10 秒音频（源自 YouTube / AudioSet），每段 5 条英文人工描述，另有 Aya-23 机器翻译的简体中文列。
- 抽 150 段作语料，其中 50 段作查询正例；下载失败的音频直接剔除。
- 中文查询：参照 5 条英文原文和机器译文逐条人工校对改写为自然中文，保存在
  `_downloads/curated/audiocaps_zh.json`（键为音频文件名）。缺少时写出 `audiocaps_zh.todo.json` 供整理后再运行。
- 为覆盖不同解码路径，语料按固定顺序轮流保存为 `.wav`（原样）、`.mp3`、`.m4a`。
"""

import csv
import json
import sys

from common import CURATED, ffmpeg, hf, load_curated, reset_dir, rng, try_hf

REPO = 'piyushsinghpasi/audiocaps-multilingual'
NAME = 'audio_audiocaps'
N_FILES = 150
N_QUERIES = 50
FORMATS = ['.wav', '.mp3', '.m4a']


def build(root):
    csv.field_size_limit(1 << 30)
    meta_path = hf(REPO, 'test/metadata.csv')
    with open(meta_path, encoding='utf-8', newline='') as f:
        rows = list(csv.DictReader(f))
    clips = {}
    for row in rows:
        clips.setdefault(row['file_name'], []).append(row)

    r = rng(NAME)
    candidates = r.sample(sorted(clips), N_FILES)
    local = {}
    for name in candidates:
        path = try_hf(REPO, f'test/{name}')
        if path:
            local[name] = path
    query_clips = sorted(r.sample(sorted(local), N_QUERIES))

    curated = load_curated('audiocaps_zh.json')
    if curated is None or any(c not in curated for c in query_clips):
        todo = {c: {'en': [x['caption'] for x in clips[c]], 'zh_mt': [x['zho_Hans_AYA'] for x in clips[c]],
                    'zh': (curated or {}).get(c, '')} for c in query_clips}
        CURATED.mkdir(parents=True, exist_ok=True)
        (CURATED / 'audiocaps_zh.todo.json').write_text(json.dumps(todo, ensure_ascii=False, indent=1), encoding='utf-8')
        sys.exit('缺少人工校对的中文查询，已写出 _downloads/curated/audiocaps_zh.todo.json')

    out = reset_dir(root / 'files' / NAME)
    names = {}
    for i, name in enumerate(sorted(local)):
        ext = FORMATS[i % len(FORMATS)]
        stem = name.removeprefix('audio/').removesuffix('.wav')
        dst = out / f'{stem}{ext}'
        if ext == '.wav':
            dst.write_bytes(local[name].read_bytes())
        elif ext == '.mp3':
            ffmpeg('-i', local[name], '-c:a', 'libmp3lame', '-q:a', '4', dst)
        else:
            ffmpeg('-i', local[name], '-c:a', 'aac', '-b:a', '96k', dst)
        names[name] = dst.name

    result = [{
        'id': f"{NAME}-{names[c].rsplit('.', 1)[0].removeprefix('Y')}",
        'subset': NAME,
        'query': curated[c],
        'relevant': [{'file': f'files/{NAME}/{names[c]}'}],
    } for c in query_clips]

    meta = {
        'name': NAME,
        'modality': 'audio',
        'description': '10 秒环境声 / 声音事件片段（AudioSet），按中文描述找声音；每条查询 1 个正例，其余为干扰项。',
        'source': f'https://huggingface.co/datasets/{REPO}（AudioCaps 测试集，多语言描述）',
        'license': '描述 CC BY-NC 4.0；音频源自 YouTube（AudioSet），归原作者，仅限非商业研究，只在本机评测，不得分发。',
        'query_lang': 'zh',
        'query_origin': 'mt-reviewed',
        'count_files': len(names),
        'count_queries': len(result),
        'dropped': sorted(set(candidates) - set(local)),
    }
    return meta, result
