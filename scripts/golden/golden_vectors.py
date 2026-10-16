"""计算黄金向量：llama.cpp b11461 CPU、GGUF Q8_0（主模型 + mmproj）、Flash Attention 显式关闭。

读取 assets/golden/manifest.json，先核对样本文件哈希，再用 llama_embed.LlamaEmbedder 逐条计算 768 维向量并做数值检查
（无 NaN / Inf、范数与 1 相差不超过 1e-6），写入 assets/golden/vectors-gguf-q8-<主模型 SHA-256 前 16 位>.json。
- 文本已在清单中加好任务前缀；图像、音频、视频窗口不加前缀。
- 图像 140 档视觉 token；视频帧按清单中的 vision_tokens_per_image。
- 只加载纯 CPU 预编译包，出现非 CPU 设备即中止。

用法（在仓库根目录）：
  .cache\\venv\\Scripts\\python.exe -I scripts\\golden\\golden_vectors.py           # 重新计算并写入
  .cache\\venv\\Scripts\\python.exe -I scripts\\golden\\golden_vectors.py --check   # 只复核，不写文件

--check 重新计算后与已提交的向量逐条比较，余弦低于 0.9999 或数值检查不过时以非零状态退出。
"""

from __future__ import annotations

import hashlib
import json
import sys
import time
from pathlib import Path

import numpy as np

# -I 模式不把脚本目录加入 sys.path，这里手动加上以导入同目录的模块。
sys.path.insert(0, str(Path(__file__).resolve().parent))
import llama_embed as le  # noqa: E402

QUANT = "Q8_0"
IMAGE_TOKENS = 140
GGUF_SOURCE = "ggml-org/embeddinggemma-2-GGUF@bfcd298762cc34d0357ece5ebdd31791a3a374d8"
# 同一运行库、同一后端重算时应几乎逐位相同，门槛比跨后端比较严得多。
CHECK_MIN_COS = 0.9999


def sha256(path: Path) -> str:
  h = hashlib.sha256()
  with open(path, "rb") as f:
    for chunk in iter(lambda: f.read(1 << 20), b""):
      h.update(chunk)
  return h.hexdigest()


def main() -> None:
  sys.stdout.reconfigure(encoding="utf-8", errors="replace")
  manifest_path = le.GOLDEN_DIR / "manifest.json"
  manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
  for rel, digest in manifest["file_sha256"].items():
    if sha256(le.GOLDEN_DIR / rel) != digest:
      raise SystemExit(f"样本 {rel} 与清单中的哈希不一致：改动样本后要同步更新清单中的哈希。")

  model, mmproj = le.model_path("text", QUANT), le.model_path("mmproj", QUANT)
  model_sha = sha256(model)
  path = le.GOLDEN_DIR / f"vectors-gguf-q8-{model_sha[:16]}.json"
  t0 = time.perf_counter()
  emb = le.LlamaEmbedder(model, mmproj, le.DEFAULT_THREADS, "off")
  init_s = time.perf_counter() - t0
  vectors, bad = [], []
  try:
    for s in le.golden_parts(manifest, IMAGE_TOKENS):
      t1 = time.perf_counter()
      vec, st = emb.embed(s["parts"])
      nan_inf = int((~np.isfinite(vec)).sum())
      norm = float(np.linalg.norm(vec.astype(np.float64)))
      print(f"{s['id']:22s} {s['modality']:8s} 范数 {norm:.7f}  序列 {st['seq_len']:4d}  {time.perf_counter() - t1:6.2f} 秒",
            flush=True)
      if vec.shape != (768,) or nan_inf or not np.isfinite(st["raw_norm"]) or abs(norm - 1) > 1e-6:
        bad.append(s["id"])
      vectors.append({"id": s["id"], "modality": s["modality"], "norm": round(norm, 7), "seq_len": st["seq_len"],
                      "vector": [float(f"{v:.9g}") for v in vec]})
  finally:
    emb.close()
  if bad:
    raise SystemExit(f"数值检查未通过：{bad}")

  if "--check" in sys.argv:
    old = json.loads(path.read_text(encoding="utf-8"))
    if old["manifest_sha256"] != sha256(manifest_path):
      raise SystemExit("manifest.json 与生成黄金向量时的版本不同，需要重新计算黄金向量。")
    ref = {v["id"]: v for v in old["vectors"]}
    # 每个模态记录 [最小余弦, 对应编号, 逐元素最大绝对差]。
    worst: dict[str, list] = {}
    for v in vectors:
      r = ref[v["id"]]["vector"]
      a, b = np.asarray(v["vector"], np.float64), np.asarray(r, np.float64)
      cos = float(a @ b / (np.linalg.norm(a) * np.linalg.norm(b)))
      diff = float(np.abs(a - b).max())
      w = worst.setdefault(v["modality"], [2.0, "", 0.0])
      if cos < w[0]:
        w[0], w[1] = cos, v["id"]
      w[2] = max(w[2], diff)
    for m, (cos, sid, diff) in worst.items():
      print(f"{m:8s} 最小余弦 {cos:.9f}（{sid}）  逐元素最大绝对差 {diff:.3g}")
    if len(ref) != len(vectors) or min(w[0] for w in worst.values()) < CHECK_MIN_COS:
      raise SystemExit(f"复核未通过：条数 {len(vectors)} / {len(ref)}，或最小余弦低于 {CHECK_MIN_COS}。")
    print(f"复核通过：{len(vectors)} 条，逐条余弦均不低于 {CHECK_MIN_COS}。")
    return

  pkg = le.LLAMA_DIR.parent / f"llama-{le.TAG}-bin-win-cpu-x64.zip"
  out = {
      "format": "每条向量是 768 个 float32（以 9 位有效数字的十进制写出，可无损还原为 float32），已 L2 归一化。",
      "runtime": f"llama.cpp {le.TAG}（官方预编译包 {pkg.name}，ctypes 调 llama.dll + mtmd.dll）",
      "runtime_package_sha256": sha256(pkg),
      "runtime_dll_sha256": {n: sha256(le.LLAMA_DIR / n) for n in ("llama.dll", "mtmd.dll", "ggml.dll", "ggml-base.dll")},
      "gguf_source": GGUF_SOURCE,
      "model_file": model.name,
      "model_sha256": model_sha,
      "mmproj_file": mmproj.name,
      "mmproj_sha256": sha256(mmproj),
      "backend": {"text": "cpu", "vision": "cpu", "audio": "cpu"},
      "devices": [d["name"] for d in emb.devices],
      "precision": "权重 Q8_0（主模型与 mmproj）；ggml CPU 后端把矩阵乘的激活按权重的 vec_dot 类型量化为 Q8_0，其余运算 F32；不使用 F16",
      "flash_attn": "off（llama 上下文与 mtmd 均显式设为 DISABLED）",
      "pooling": "mean，非因果注意力，整段输入一次编码",
      "threads": le.DEFAULT_THREADS,
      "max_tokens": le.MAX_TOKENS,
      "vision_tokens_per_image": IMAGE_TOKENS,
      "video_vision_tokens_per_image": manifest["videos"][0]["vision_tokens_per_image"],
      "prefix_scheme": manifest["prefix_scheme"],
      "manifest_sha256": sha256(manifest_path),
      "engine_init_seconds": round(init_s, 2),
      "vectors": vectors,
  }
  # 固定 LF 换行，与仓库存储一致。
  path.write_bytes((json.dumps(out, ensure_ascii=False, separators=(",", ":")) + "\n").encode("utf-8"))
  print(f"共 {len(vectors)} 条向量，已写入 {path.name}")


if __name__ == "__main__":
  main()
