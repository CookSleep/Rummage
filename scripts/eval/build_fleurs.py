"""文本 → 说话内容：FLEURS 普通话验证集（`google/fleurs` 的 `cmn_hans_cn` dev）。

- 验证集 409 条朗读录音，对应 150 个不同句子（维基百科句子，相邻编号多来自同一篇文章）。每个句子取第一条录音。
- 按句子编号顺序拼接成约 2 分钟一段的“录音文件”（句间留 0.6 秒静音，开头留 1 秒），共约 14 段，
  记录每句的起止秒数；按固定顺序轮流存为 `.mp3`、`.m4a`、`.wav`。
- 抽 50 个句子作查询。查询不照抄原句，改写为“找说到某件事的那段”式的中文描述，保存在
  `_downloads/curated/fleurs_zh.json`（键为句子编号）；缺少时写出 `fleurs_zh.todo.json`。相关项带 start / end。
"""

import io
import json
import subprocess
import sys
import tarfile
import wave

from common import CURATED, FFMPEG, ffmpeg, hf, load_curated, reset_dir, rng

REPO = 'google/fleurs'
NAME = 'speech_fleurs'
N_QUERIES = 50
SR = 16000
TARGET_SECONDS = 120
MAX_SECONDS = 170
GAP = 0.6
LEAD = 1.0
FORMATS = ['.mp3', '.m4a', '.wav']


def decode(data):
    """把 FLEURS 的 32 位浮点 WAV 转成 16 kHz 单声道 16 位 PCM 字节。"""
    proc = subprocess.run(
        [FFMPEG, '-hide_banner', '-loglevel', 'error', '-i', 'pipe:0', '-f', 's16le', '-ac', '1', '-ar', str(SR), 'pipe:1'],
        input=data, capture_output=True, check=True,
    )
    return proc.stdout


def build(root):
    tsv = hf(REPO, 'data/cmn_hans_cn/dev.tsv')
    tar_path = hf(REPO, 'data/cmn_hans_cn/audio/dev.tar.gz')
    first = {}
    for line in tsv.read_text(encoding='utf-8').splitlines():
        cols = line.split('\t')
        first.setdefault(int(cols[0]), {'file': cols[1], 'text': cols[2]})
    sids = sorted(first)

    r = rng(NAME)
    query_sids = sorted(r.sample(sids, N_QUERIES))
    curated = load_curated('fleurs_zh.json')
    if curated is None or any(str(s) not in curated for s in query_sids):
        todo = {str(s): {'text': first[s]['text'], 'zh': (curated or {}).get(str(s), '')} for s in query_sids}
        CURATED.mkdir(parents=True, exist_ok=True)
        (CURATED / 'fleurs_zh.todo.json').write_text(json.dumps(todo, ensure_ascii=False, indent=1), encoding='utf-8')
        sys.exit('缺少改写后的中文查询，已写出 _downloads/curated/fleurs_zh.todo.json')

    wanted = {first[s]['file']: s for s in sids}
    pcm = {}
    with tarfile.open(tar_path) as tar:
        for m in tar:
            name = m.name.rsplit('/', 1)[-1]
            if m.isfile() and name in wanted:
                pcm[wanted[name]] = decode(tar.extractfile(m).read())

    # 按句子编号顺序分组，每组凑够约 2 分钟
    groups, cur, dur = [], [], LEAD
    for s in sids:
        if s not in pcm:
            continue
        sec = len(pcm[s]) / 2 / SR
        if cur and (dur >= TARGET_SECONDS or dur + sec > MAX_SECONDS):
            groups.append(cur)
            cur, dur = [], LEAD
        cur.append(s)
        dur += sec + GAP
    if cur:
        groups.append(cur)

    out = reset_dir(root / 'files' / NAME)
    spans = {}
    silence = lambda sec: b'\x00\x00' * int(sec * SR)
    for i, group in enumerate(groups):
        ext = FORMATS[i % len(FORMATS)]
        fname = f'recording_{i + 1:02d}{ext}'
        buf = bytearray(silence(LEAD))
        for s in group:
            start = len(buf) / 2 / SR
            buf += pcm[s]
            spans[s] = (fname, round(start, 2), round(len(buf) / 2 / SR, 2))
            buf += silence(GAP)
        wav = io.BytesIO()
        with wave.open(wav, 'wb') as w:
            w.setnchannels(1)
            w.setsampwidth(2)
            w.setframerate(SR)
            w.writeframes(bytes(buf))
        if ext == '.wav':
            (out / fname).write_bytes(wav.getvalue())
            continue
        tmp = out / f'_tmp_{i}.wav'
        tmp.write_bytes(wav.getvalue())
        codec = ['-c:a', 'libmp3lame', '-q:a', '4'] if ext == '.mp3' else ['-c:a', 'aac', '-b:a', '64k']
        ffmpeg('-i', tmp, *codec, out / fname)
        tmp.unlink()

    result = []
    for s in query_sids:
        if s not in spans:
            continue
        fname, start, end = spans[s]
        result.append({
            'id': f'{NAME}-{s}',
            'subset': NAME,
            'query': curated[str(s)],
            'relevant': [{'file': f'files/{NAME}/{fname}', 'start': start, 'end': end}],
        })

    meta = {
        'name': NAME,
        'modality': 'audio',
        'description': '普通话朗读（维基百科句子）拼接成的约 2 分钟录音，按“说到某件事的那段”找文件和时间段。',
        'source': f'https://huggingface.co/datasets/{REPO}（FLEURS，cmn_hans_cn 验证集）',
        'license': 'CC BY 4.0。只在本机评测。',
        'query_lang': 'zh',
        'query_origin': 'llm',
        'count_files': len(groups),
        'count_queries': len(result),
        'notes': '查询由编码助手根据原句改写（不照抄原句），已逐条核对语义与原句一致。',
    }
    return meta, result
