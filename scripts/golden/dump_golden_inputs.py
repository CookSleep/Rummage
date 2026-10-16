"""导出黄金样本预处理后的输入（与 llama_embed.py 完全相同的像素和波形），供 rummage-media 的测试 golden_pixels_match_pil
（src/lib.rs）逐字节比较 PNG 的解码与缩放结果。

每条样本一个目录 <id>/：
- parts.json：有序片段列表，{"text": ...}、{"image": "0.rgb", "w": 宽, "h": 高}、{"audio": "1.f32"}；
- 二进制文件：图像是 llama_embed.LlamaEmbedder.bitmap() 交给 mtmd_bitmap_init 的 RGB 字节（按行，每像素 3 字节），
  音频是交给 mtmd_bitmap_init_from_audio 的小端 float32 采样。文件名前缀是片段序号。
图像档位与黄金向量一致：图像 140，视频帧按清单中的 vision_tokens_per_image。
输出到 .cache/golden/pil-inputs/，即上述测试读取的位置。

用法（在仓库根目录）：
  .cache\\venv\\Scripts\\python.exe -I scripts\\golden\\dump_golden_inputs.py
"""

from __future__ import annotations

import io
import json
import shutil
import sys
from pathlib import Path

import numpy as np
from PIL import Image

# -I 模式不把脚本目录加入 sys.path，这里手动加上以导入同目录的模块。
sys.path.insert(0, str(Path(__file__).resolve().parent))
import llama_embed as le  # noqa: E402

OUT_DIR = le.REPO_ROOT / ".cache" / "golden" / "pil-inputs"


def main() -> None:
  sys.stdout.reconfigure(encoding="utf-8", errors="replace")
  manifest = json.loads((le.GOLDEN_DIR / "manifest.json").read_text(encoding="utf-8"))
  samples = le.golden_parts(manifest, 140)
  if OUT_DIR.exists():
    shutil.rmtree(OUT_DIR)
  for s in samples:
    d = OUT_DIR / s["id"]
    d.mkdir(parents=True)
    parts = []
    for k, p in enumerate(s["parts"]):
      if p[0] == "text":
        parts.append({"text": p[1]})
        continue
      if p[0] == "image":
        # 与 LlamaEmbedder.bitmap() 相同的步骤。
        img = le.to_rgb(Image.open(io.BytesIO(p[1])))
        th, tw = le.target_size(img.height, img.width, p[2])
        if (th, tw) != (img.height, img.width):
          img = img.resize((tw, th), Image.BICUBIC)
        (d / f"{k}.rgb").write_bytes(img.tobytes())
        parts.append({"image": f"{k}.rgb", "w": tw, "h": th})
        continue
      wav = np.ascontiguousarray(p[1], np.float32).astype("<f4")
      (d / f"{k}.f32").write_bytes(wav.tobytes())
      parts.append({"audio": f"{k}.f32"})
    (d / "parts.json").write_text(json.dumps(parts, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"{s['id']:16s} {s['modality']:8s} {len(parts)} 个片段")
  print(f"共 {len(samples)} 条，已写入 {OUT_DIR}")


if __name__ == "__main__":
  main()
