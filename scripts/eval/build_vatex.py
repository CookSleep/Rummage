"""文本 → 视频片段：VATEX 中英双语检索版（`vnahata/vatex-multilingual-retrieval`）。

- 993 段已按标注区间裁好的约 10 秒短视频（VATEX 验证集，源自 YouTube），每段 1 条人工撰写的中文描述。
- 视频分 3 个 parquet 分片，只用第一个分片（331 段）。从中抽 100 段作语料，其中 60 段作查询正例。
- 实测这些片段只有视频轨、没有音轨（重新编码时去掉了），所以只考查画面。
"""

import pyarrow.parquet as pq

from common import hf, reset_dir, rng

REPO = 'vnahata/vatex-multilingual-retrieval'
NAME = 'video_vatex'
N_FILES = 100
N_QUERIES = 60


def build(root):
    captions = {c['video_id']: c['text'] for c in pq.read_table(hf(REPO, 'zh/test-00000-of-00001.parquet')).to_pylist()}
    shard = hf(REPO, 'videos/test-00000-of-00003.parquet')
    ids = sorted(pq.read_table(shard, columns=['id']).column('id').to_pylist())
    r = rng(NAME)
    keep = set(r.sample([i for i in ids if i in captions], N_FILES))
    query_ids = sorted(r.sample(sorted(keep), N_QUERIES))

    out = reset_dir(root / 'files' / NAME)
    names = {}
    for batch in pq.ParquetFile(shard).iter_batches(batch_size=16):
        for row in batch.to_pylist():
            if row['id'] in keep:
                names[row['id']] = f"{row['id']}.mp4"
                (out / names[row['id']]).write_bytes(row['video']['bytes'])

    result = [{
        'id': f'{NAME}-{vid}',
        'subset': NAME,
        'query': captions[vid].strip(),
        'relevant': [{'file': f'files/{NAME}/{names[vid]}'}],
    } for vid in query_ids if vid in names]

    meta = {
        'name': NAME,
        'modality': 'video',
        'description': '约 10 秒的生活 / 运动 / 动作短视频（无音轨），按中文描述找视频；每条查询 1 个正例。',
        'source': f'https://huggingface.co/datasets/{REPO}（VATEX 验证集，第一个视频分片）',
        'license': '描述 CC BY 4.0；视频源自 YouTube，归原作者，数据集卡片标为 other（作者无权再分发视频），只在本机评测，不得分发。',
        'query_lang': 'zh',
        'query_origin': 'human',
        'count_files': len(names),
        'count_queries': len(result),
    }
    return meta, result
