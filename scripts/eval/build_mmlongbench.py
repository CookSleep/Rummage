"""英文问题 → 长 PDF：MMLongBench-Doc（NeurIPS 2024 Datasets and Benchmarks），可选子集，默认不构建。

- 来源：PDF 取自 Hugging Face `yubo2333/MMLongBench-Doc` 的 `documents/`（钉在 `REVISION`，135 份真实长 PDF：研究报告、学术论文、
  教程幻灯片、手册、宣传册、财报、行政文件，平均约 50 页）；问题取 MMLongBench-Doc-V2（arXiv 2608.03397，
  `VectifyAI/MMLongBench-Doc-V2` 的 `data/samples.json`，钉在 `V2_COMMIT`）修正后的标注：1,071 条，134 份文档。
- 协议：原仓库（GitHub `mayubo2333/MMLongBench-Doc`）与 V2 标注为 Apache-2.0；HF 数据集未标注协议，PDF 版权归原作者。
  只在本机评测，不得分发。
- 转换：去掉不可回答的问题（答案以 `Not answerable` 开头），其余全部使用，不抽样；查询用英文原问题，不翻译。
  相关文件就是问题对应的那份 PDF，文件级相关，不标页码。没有问题的那份 PDF 也放进语料，作干扰项。
- 文件名：原文件名多含标题（如 `PH_2016.06.08_Economy-Final.pdf`），会让文件名检索直接命中，因此按原文件名排序后
  改名为 `doc_001.pdf` 等（硬链接到 `_downloads/` 中的原文件）。
- 分组：FORMAT.md 没有标签字段，每条查询的证据来源（`evidence_sources`）、证据页（`evidence_pages`）与文档类型
  （`doc_type`）以及原文件名另存为 `tags/doc_mmlongbench.json`（评测集根目录下，评测程序不读），供按组统计。
- 用到内嵌图片的评测：证据含 Figure / Chart 的问题要看图才能回答。
"""

import ast
import json
import os

from common import download, hf, reset_dir

REPO = 'yubo2333/MMLongBench-Doc'
REVISION = '2ff6aa9237fc777b6627dc57a486e9225ac5fb86'
V2_REPO = 'VectifyAI/MMLongBench-Doc-V2'
V2_COMMIT = '3aba3c6831a432ee882f763435f9ebe92ab75ed9'
NAME = 'doc_mmlongbench'


def doc_list():
    """HF 仓库 `documents/` 中的 PDF 文件名；列表缓存在 `_downloads/` 中。"""
    path = download(f'https://huggingface.co/api/datasets/{REPO}/tree/{REVISION}/documents', f'hf/{REPO}/documents.tree.json')
    return sorted(e['path'].removeprefix('documents/') for e in json.loads(path.read_text(encoding='utf-8')))


def build(root):
    samples_path = download(
        f'https://raw.githubusercontent.com/{V2_REPO}/{V2_COMMIT}/data/samples.json',
        f'github/{V2_REPO}/{V2_COMMIT}/data/samples.json',
    )
    samples = json.loads(samples_path.read_text(encoding='utf-8'))
    docs = doc_list()
    names = {d: f'doc_{i + 1:03d}.pdf' for i, d in enumerate(docs)}
    out = reset_dir(root / 'files' / NAME)
    for d in docs:
        # 硬链接到下载缓存，不占双份空间（与 build_qvh 相同）
        os.link(hf(REPO, f'documents/{d}', REVISION), out / names[d])

    result = []
    tags = {}
    for i, s in enumerate(samples):
        if s['answer'].strip().lower().startswith('not answerable'):
            continue
        qid = f'{NAME}-{i}'
        result.append({
            'id': qid,
            'subset': NAME,
            'query': ' '.join(s['question'].split()),
            'relevant': [{'file': f"files/{NAME}/{names[s['doc_id']]}"}],
        })
        tags[qid] = {
            'doc_id': s['doc_id'],
            'doc_type': s['doc_type'],
            'evidence_sources': ast.literal_eval(s['evidence_sources']),
            'evidence_pages': ast.literal_eval(s['evidence_pages']),
        }
    (root / 'tags').mkdir(exist_ok=True)
    (root / 'tags' / f'{NAME}.json').write_text(json.dumps(tags, ensure_ascii=False, indent=1) + '\n', encoding='utf-8', newline='\n')

    meta = {
        'name': NAME,
        'modality': 'document',
        'description': '英文真实长 PDF（研究报告、论文、教程幻灯片、手册、宣传册、财报等，平均约 50 页），查询是针对文档内容的英文问句，多数要看图表、表格或版面才能回答；考查内嵌图片对找到文档的帮助。',
        'source': f'https://huggingface.co/datasets/{REPO}（MMLongBench-Doc，NeurIPS 2024）；问题用 https://github.com/{V2_REPO} 修正后的标注（arXiv 2608.03397）',
        'license': '原仓库与 V2 标注为 Apache-2.0；HF 数据集未标注协议，PDF 版权归原作者。只在本机评测，不得分发。',
        'query_lang': 'en',
        'query_origin': 'human',
        'count_files': len(names),
        'count_queries': len(result),
        'notes': '可选子集，build_all.py 默认不构建。每条查询的证据来源、证据页与文档类型在 tags/doc_mmlongbench.json。',
    }
    return meta, result
