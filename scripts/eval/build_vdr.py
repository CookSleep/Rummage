"""文本 → 截图 / 文档图像：JinaVDR 的两个中文子集（MTEB 版）。

- Airbnb Synthetic：由房源数据渲染的表格截图（每种语言 1,000 张），中文查询 486 条，按模板生成
  （如“在某街区找某日期段、价格低于某值的房源”）。抽 80 条查询，语料取正例加干扰共 300 张。
- 上海总体规划：《上海市城市总体规划（2017—2035 年）》的 23 页文件图，57 条人工标注查询，全部使用。
- 两个子集各自一个目录；评测时与其他目录一起组成混合库。
"""

import pyarrow.parquet as pq

from common import hf, reset_dir, rng

REPO_AIRBNB = 'mteb/JinaVDRAirbnbSyntheticRetrieval'
REPO_SHANGHAI = 'mteb/JinaVDRShanghaiMasterPlanRetrieval'
NAME_AIRBNB = 'screenshot_airbnb'
NAME_SHANGHAI = 'docimage_shanghai'
N_AIRBNB_QUERIES = 80
N_AIRBNB_FILES = 300


def write_images(corpus_path, keep, out, prefix):
    """把 parquet 中选中的图片写成文件，返回 corpus-id → 文件名。"""
    names = {}
    for batch in pq.ParquetFile(corpus_path).iter_batches(batch_size=64, columns=['id', 'image']):
        for row in batch.to_pylist():
            if row['id'] not in keep:
                continue
            data = row['image']['bytes']
            ext = '.png' if data[:4] == b'\x89PNG' else '.jpg'
            name = f"{prefix}{row['id'].removeprefix('corpus-').removeprefix('test-')}{ext}"
            (out / name).write_bytes(data)
            names[row['id']] = name
    return names


def load(repo, lang_prefix):
    queries = pq.read_table(hf(repo, f'{lang_prefix}queries/test-00000-of-00001.parquet'), columns=['id', 'text']).to_pylist()
    qrels = pq.read_table(hf(repo, f'{lang_prefix}qrels/test-00000-of-00001.parquet')).to_pylist()
    rel = {}
    for r in qrels:
        if r['score'] > 0:
            rel.setdefault(r['query-id'], []).append(r['corpus-id'])
    return [q for q in queries if q['id'] in rel], rel, hf(repo, f'{lang_prefix}corpus/test-00000-of-00001.parquet')


def to_queries(name, picked, rel, names):
    return [{
        'id': f"{name}-{q['id'].removeprefix('query-test-')}",
        'subset': name,
        'query': ' '.join(q['text'].split()),
        'relevant': [{'file': f'files/{name}/{names[c]}'} for c in rel[q['id']]],
    } for q in picked]


def build_airbnb(root):
    queries, rel, corpus_path = load(REPO_AIRBNB, 'zh-')
    r = rng(NAME_AIRBNB)
    picked = sorted(r.sample(queries, N_AIRBNB_QUERIES), key=lambda q: int(q['id'].rsplit('-', 1)[1]))
    pos = {c for q in picked for c in rel[q['id']]}
    corpus_ids = sorted(pq.read_table(corpus_path, columns=['id']).column('id').to_pylist())
    keep = pos | set(r.sample([c for c in corpus_ids if c not in pos], N_AIRBNB_FILES - len(pos)))
    out = reset_dir(root / 'files' / NAME_AIRBNB)
    names = write_images(corpus_path, keep, out, 'listing_')
    result = to_queries(NAME_AIRBNB, picked, rel, names)
    meta = {
        'name': NAME_AIRBNB,
        'modality': 'image',
        'description': '合成的 Airbnb 房源表格截图（中文界面），按街区、日期、价格等条件找截图，考查截图中的文字理解。',
        'source': f'https://huggingface.co/datasets/{REPO_AIRBNB}（JinaVDR，源自 jinaai/airbnb-synthetic-retrieval）',
        'license': 'CC0 1.0（数据集卡片）。',
        'query_lang': 'zh',
        'query_origin': 'template',
        'count_files': len(names),
        'count_queries': len(result),
    }
    return meta, result


def build_shanghai(root):
    queries, rel, corpus_path = load(REPO_SHANGHAI, '')
    out = reset_dir(root / 'files' / NAME_SHANGHAI)
    corpus_ids = pq.read_table(corpus_path, columns=['id']).column('id').to_pylist()
    names = write_images(corpus_path, set(corpus_ids), out, 'page_')
    picked = sorted(queries, key=lambda q: int(q['id'].rsplit('-', 1)[1]))
    result = to_queries(NAME_SHANGHAI, picked, rel, names)
    meta = {
        'name': NAME_SHANGHAI,
        'modality': 'image',
        'description': '《上海市城市总体规划（2017—2035 年）》的页面图片（真实中文公文，含图表），按主题找页。',
        'source': f'https://huggingface.co/datasets/{REPO_SHANGHAI}（JinaVDR，源自 jinaai/shanghai_master_plan_beir）',
        'license': '数据集卡片写 unknown；原文为上海市政府公开文件。只在本机评测，不得分发。',
        'query_lang': 'zh',
        'query_origin': 'human',
        'count_files': len(names),
        'count_queries': len(result),
    }
    return meta, result
