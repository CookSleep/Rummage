"""文本 → 视频时刻：QVHighlights 验证集。

- 标注来自 `jwnt4/qvhighlights-val`（`highlight_val_release.jsonl`，1,549 条查询、1,519 个视频），
  视频来自 `ayushsdev/qvhighlights-videos`（按文件名首字符的小写分目录，每个视频是原 YouTube 视频中的 150 秒片段）。
- 抽 60 个视频，下载失败的剔除；使用这些视频的全部查询。时间段取 `relevant_windows`（相对片段开头的秒数），
  一条查询有多个时间段时写成同一文件的多个相关项。
- 原查询为英文，逐条翻译为中文并人工校对，保存在 `_downloads/curated/qvh_zh.json`（键为 qid）；
  缺少时写出 `qvh_zh.todo.json`。
"""

import json
import os
import shutil
import sys

from common import CURATED, hf, load_curated, reset_dir, rng, try_hf

REPO_ANN = 'jwnt4/qvhighlights-val'
REPO_VIDEO = 'ayushsdev/qvhighlights-videos'
NAME = 'video_qvhighlights'
N_VIDEOS = 60


def build(root):
    ann = [json.loads(line) for line in hf(REPO_ANN, 'highlight_val_release.jsonl').read_text(encoding='utf-8').splitlines() if line.strip()]
    by_vid = {}
    for a in ann:
        by_vid.setdefault(a['vid'], []).append(a)

    r = rng(NAME)
    picked = sorted(r.sample(sorted(by_vid), N_VIDEOS))
    local = {}
    for vid in picked:
        path = try_hf(REPO_VIDEO, f'{vid[0].lower()}/{vid}.mp4')
        if path:
            local[vid] = path
    queries = sorted((a for vid in local for a in by_vid[vid]), key=lambda a: a['qid'])

    curated = load_curated('qvh_zh.json')
    if curated is None or any(str(a['qid']) not in curated for a in queries):
        todo = {str(a['qid']): {'en': a['query'], 'zh': (curated or {}).get(str(a['qid']), '')} for a in queries}
        CURATED.mkdir(parents=True, exist_ok=True)
        (CURATED / 'qvh_zh.todo.json').write_text(json.dumps(todo, ensure_ascii=False, indent=1), encoding='utf-8')
        sys.exit('缺少校对后的中文查询，已写出 _downloads/curated/qvh_zh.todo.json')

    out = reset_dir(root / 'files' / NAME)
    for vid, path in local.items():
        # 视频较大，优先建硬链接，省一份磁盘空间；跨盘等情况退回复制
        try:
            os.link(path, out / f'{vid}.mp4')
        except OSError:
            shutil.copyfile(path, out / f'{vid}.mp4')

    result = [{
        'id': f"{NAME}-{a['qid']}",
        'subset': NAME,
        'query': curated[str(a['qid'])],
        'relevant': [{'file': f"files/{NAME}/{a['vid']}.mp4", 'start': float(s), 'end': float(e)} for s, e in a['relevant_windows']],
    } for a in queries]

    meta = {
        'name': NAME,
        'modality': 'video',
        'description': '150 秒的 Vlog / 新闻视频片段（带音轨，口播较多），按中文描述找视频中的时刻；相关项带时间段。',
        'source': f'https://huggingface.co/datasets/{REPO_ANN}（标注）与 https://huggingface.co/datasets/{REPO_VIDEO}（视频），QVHighlights 验证集',
        'license': 'CC BY-NC-SA 4.0；视频源自 YouTube，归原作者，仅限非商业研究，只在本机评测，不得分发。',
        'query_lang': 'zh',
        'query_origin': 'mt-reviewed',
        'count_files': len(local),
        'count_queries': len(result),
        'dropped': sorted(set(picked) - set(local)),
    }
    return meta, result
