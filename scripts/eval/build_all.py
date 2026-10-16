"""从 `_downloads` 缓存重建评测集 `Rummage-eval/`（格式见 `docs/eval/FORMAT.md`）。

用法（在仓库根目录）：
    .cache/venv/Scripts/python.exe scripts/eval/build_all.py            # 重建全部子集
    .cache/venv/Scripts/python.exe scripts/eval/build_all.py video_vatex # 只重建指定子集
    .cache/venv/Scripts/python.exe scripts/eval/build_all.py doc_mmlongbench # 可选子集，只在指定时构建

- 缺少的源文件会自动下载到 `_downloads/`，已有的直接复用；`_downloads/` 不会被删除。
- 只重建部分子集时，`queries.jsonl` 与 `manifest.json` 中其他子集的条目保持不变。
- 依赖：pyarrow、python-docx、reportlab（装在 `.cache/venv/`），PATH 中的 ffmpeg，以及本机的 Edge（`screenshot_long`）。
"""

import json
import sys

import build_audiocaps
import build_docs
import build_fleurs
import build_long
import build_mmlongbench
import build_photo
import build_qvh
import build_vatex
import build_vdr
from common import EVAL_ROOT, dir_size

FORMAT_VERSION = 1
BUILDERS = {
    build_photo.NAME: build_photo.build,
    build_vdr.NAME_AIRBNB: build_vdr.build_airbnb,
    build_vdr.NAME_SHANGHAI: build_vdr.build_shanghai,
    build_long.NAME: build_long.build,
    build_docs.NAME: build_docs.build,
    build_audiocaps.NAME: build_audiocaps.build,
    build_fleurs.NAME: build_fleurs.build,
    build_vatex.NAME: build_vatex.build,
    build_qvh.NAME: build_qvh.build,
}
# 可选子集：不在默认的“全部子集”中，只在命令行指定时构建
OPTIONAL = {
    build_mmlongbench.NAME: build_mmlongbench.build,
}


def main():
    names = sys.argv[1:] or list(BUILDERS)
    builders = BUILDERS | OPTIONAL
    unknown = [n for n in names if n not in builders]
    if unknown:
        sys.exit(f'未知子集：{unknown}，可选：{list(builders)}')

    manifest_path = EVAL_ROOT / 'manifest.json'
    queries_path = EVAL_ROOT / 'queries.jsonl'
    subsets = {}
    queries = {}
    if manifest_path.exists():
        subsets = {s['name']: s for s in json.loads(manifest_path.read_text(encoding='utf-8'))['subsets']}
    if queries_path.exists():
        for line in queries_path.read_text(encoding='utf-8').splitlines():
            q = json.loads(line)
            queries.setdefault(q['subset'], []).append(q)

    for name in names:
        print(f'== {name}', flush=True)
        meta, qs = builders[name](EVAL_ROOT)
        subsets[name] = meta
        queries[name] = qs
        size = dir_size(EVAL_ROOT / 'files' / name) / 1e6
        print(f'   {meta["count_files"]} 个文件，{meta["count_queries"]} 条查询，{size:.1f} MB', flush=True)

    # 按固定顺序写出，便于对比差异
    order = [n for n in builders if n in subsets]
    manifest = {'format_version': FORMAT_VERSION, 'subsets': [subsets[n] for n in order]}
    manifest_path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + '\n', encoding='utf-8', newline='\n')
    with open(queries_path, 'w', encoding='utf-8', newline='\n') as f:
        for n in order:
            for q in queries.get(n, []):
                f.write(json.dumps(q, ensure_ascii=False) + '\n')
    total = sum(len(queries.get(n, [])) for n in order)
    print(f'完成：{len(order)} 个子集，{total} 条查询 → {EVAL_ROOT}')


if __name__ == '__main__':
    main()
