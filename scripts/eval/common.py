"""评测集构建脚本的公共部分：路径、下载缓存、ffmpeg 调用、写出子集结果。

- 所有下载都放在 `Rummage-eval/_downloads/` 中，按来源路径存放，已完整下载的文件直接复用。
  下载先写 `.part`，完成后改名，所以“文件存在”就等于“下载完整”；中断后会从 `.part` 断点续传。
- 每个子集脚本提供 `build(root) -> (meta, queries)`，由 `build_all.py` 汇总写出 `manifest.json`
  与 `queries.jsonl`。格式规范见仓库 `docs/eval/FORMAT.md`。
- 人工整理的查询（校对后的译文、改写的查询）放在 `_downloads/curated/`：它们是由研究用途
  标注派生的内容（部分为 CC BY-NC / CC BY-NC-SA），不进仓库，和下载缓存一起保留。
"""

import http.client
import json
import os
import random
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

# 默认放在仓库的同级目录，可用环境变量覆盖
EVAL_ROOT = Path(os.environ.get('RUMMAGE_EVAL_ROOT', Path(__file__).resolve().parents[2].parent / 'Rummage-eval'))
DOWNLOADS = EVAL_ROOT / '_downloads'
CURATED = DOWNLOADS / 'curated'
FFMPEG = os.environ.get('FFMPEG', 'ffmpeg')
SEED = 20261012


def rng(name):
    # 每个子集独立的固定种子，保证重建时抽样完全一致
    return random.Random(f'{SEED}-{name}')


def download(url, rel):
    """下载到 `_downloads/<rel>`，已存在则直接返回路径。"""
    dst = DOWNLOADS / rel
    if dst.exists():
        return dst
    dst.parent.mkdir(parents=True, exist_ok=True)
    part = dst.with_name(dst.name + '.part')
    for attempt in range(5):
        have = part.stat().st_size if part.exists() else 0
        req = urllib.request.Request(url, headers={'User-Agent': 'rummage-eval/1'})
        if have:
            req.add_header('Range', f'bytes={have}-')
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                # 服务器不支持断点续传时从头下载
                mode = 'ab' if have and resp.status == 206 else 'wb'
                with open(part, mode) as f:
                    shutil.copyfileobj(resp, f, 1 << 20)
            part.replace(dst)
            return dst
        except urllib.error.HTTPError as err:
            if err.code == 416:
                part.replace(dst)
                return dst
            if err.code in (404, 403, 401):
                raise
            print(f'  下载失败（{err}），重试 {attempt + 1}/5：{url}', file=sys.stderr)
        except (urllib.error.URLError, http.client.HTTPException, TimeoutError, ConnectionError) as err:
            print(f'  下载失败（{err}），重试 {attempt + 1}/5：{url}', file=sys.stderr)
        time.sleep(3 * (attempt + 1))
    raise RuntimeError(f'下载失败：{url}')


def hf(repo, path, revision='main'):
    """下载 Hugging Face 数据集仓库中的单个文件；`revision` 为分支或提交。缓存路径不含 `revision`。"""
    url = f'https://huggingface.co/datasets/{repo}/resolve/{revision}/{urllib.request.quote(path)}'
    return download(url, f'hf/{repo}/{path}')


def try_hf(repo, path):
    """源文件已被删除（404 等）时返回 None，由调用方剔除该条目。

    网络中断等临时错误照常抛出：若也当作缺失剔除，抽样结果会随网络状况变化，无法复现。
    """
    try:
        return hf(repo, path)
    except urllib.error.HTTPError as err:
        print(f'  跳过 {repo}/{path}：{err}', file=sys.stderr)
        return None


def ffmpeg(*args):
    subprocess.run([FFMPEG, '-hide_banner', '-loglevel', 'error', '-y', *map(str, args)], check=True)


def reset_dir(path):
    if path.exists():
        shutil.rmtree(path)
    path.mkdir(parents=True)
    return path


def load_curated(name):
    """读取人工整理的 JSON 文件；不存在时返回 None。"""
    path = CURATED / name
    if not path.exists():
        return None
    return json.loads(path.read_text(encoding='utf-8'))


def dir_size(path):
    return sum(p.stat().st_size for p in Path(path).rglob('*') if p.is_file())
