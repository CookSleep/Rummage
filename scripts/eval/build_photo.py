"""文本 → 照片：xFlickrCO 中文子集（MTEB 版 `mteb/XFlickr30kCoT2IRetrieval`）。

- 语料 2,000 张（Flickr30K 测试集 1,000 + COCO 1,000），每张图一条中文描述，查询与图片一一对应。
- 抽 100 条查询，语料取这 100 张正例加 500 张干扰图，共 600 张，按原样写出 JPEG。
- 文件名只用原始编号，不带描述文字，避免文件名关键词检索“作弊”。
"""

import ast

import pyarrow.parquet as pq

from common import hf, reset_dir, rng

REPO = 'mteb/XFlickr30kCoT2IRetrieval'
NAME = 'photo_xflickrco'
N_QUERIES = 100
N_FILES = 600


def build(root):
    queries = pq.read_table(hf(REPO, 'zh-queries/test-00000-of-00001.parquet'), columns=['id', 'text']).to_pylist()
    qrels = pq.read_table(hf(REPO, 'zh-qrels/test-00000-of-00001.parquet')).to_pylist()
    corpus_path = hf(REPO, 'zh-corpus/test-00000-of-00001.parquet')

    rel = {r['query-id']: r['corpus-id'] for r in qrels}
    r = rng(NAME)
    picked = r.sample(sorted(queries, key=lambda q: q['id']), N_QUERIES)
    pos = {rel[q['id']] for q in picked}
    corpus_ids = sorted(pq.read_table(corpus_path, columns=['id']).column('id').to_pylist())
    others = [c for c in corpus_ids if c not in pos]
    keep = pos | set(r.sample(others, N_FILES - len(pos)))

    out = reset_dir(root / 'files' / NAME)
    table = pq.read_table(corpus_path, columns=['id', 'image'])
    for row in table.to_pylist():
        if row['id'] not in keep:
            continue
        data = row['image']['bytes']
        # 原图都是 JPEG；万一不是，按文件头选扩展名
        ext = '.png' if data[:4] == b'\x89PNG' else '.jpg'
        (out / f"{row['id'].removeprefix('corpus-')}{ext}").write_bytes(data)
    names = {p.stem: p.name for p in out.iterdir()}

    result = []
    for q in picked:
        # MTEB 版的查询文本是 Python 列表的字符串形式，例如 "['一个戴着橙色帽子的男人']"
        text = q['text']
        if text.startswith('['):
            text = ast.literal_eval(text)[0]
        cid = rel[q['id']].removeprefix('corpus-')
        result.append({
            'id': f"{NAME}-{q['id'].removeprefix('query-')}",
            'subset': NAME,
            'query': text.strip(),
            'relevant': [{'file': f'files/{NAME}/{names[cid]}'}],
        })

    meta = {
        'name': NAME,
        'modality': 'image',
        'description': '日常照片（Flickr30K / COCO），按中文描述找图。每条查询 1 张正例，其余图片为干扰项。',
        'source': f'https://huggingface.co/datasets/{REPO}（xFlickr&CO，IGLUE 基准）',
        'license': '标注：MTEB 卡片写 CC BY-SA 4.0（原始版 floschne/xflickrco 写 CC BY 4.0）；图片来自 Flickr30K / MS-COCO，各自归原作者，仅限非商业研究，只在本机评测，不得分发。',
        'query_lang': 'zh',
        'query_origin': 'human',
        'count_files': len(names),
        'count_queries': len(result),
    }
    return meta, result
