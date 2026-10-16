"""生成 rummage-media 缩放测试的夹具：随机噪声 RGB 原图，以及 Pillow 双三次缩放到 140、1120 档尺寸的结果摘要。

- <名称>.rgb：原图像素，按行存放，每像素 3 字节。
- expected.txt：每行“名称 原宽 原高 档位 目标宽 目标高 FNV-1a-64”，摘要对 Pillow `resize(BICUBIC)` 结果的原始字节计算。
  1120 档的结果约 7.7 MB 一张，原始字节放不进 3 MB 的夹具预算，所以只存摘要。档位为“-”的行只缩放一个方向。
600×600 的原图在 140 档缩小、1120 档放大。

另有解码用的小 PNG（手写编码，PIL 写不出 16 位 RGB PNG）：
- rgb16.png、gray16.png：16 位 RGB 与 16 位灰度，<名称>.expected.rgb 为按高字节换算的 8 位 RGB；
- palette_trns.png：带 tRNS 透明的调色板图；la.png：8 位灰度加透明；
- huge.png：头部声称 100000×100000，数据只有几个字节，用于测试像素上限。

用法（在仓库根目录）：
  .cache\\venv\\Scripts\\python.exe -I crates\\rummage-media\\tests\\fixtures\\gen.py
"""

from __future__ import annotations

import struct
import zlib
from pathlib import Path

import numpy as np
from PIL import Image

OUT = Path(__file__).resolve().parent
SOURCES = [("noise_517x311", 517, 311), ("noise_64x1500", 64, 1500), ("noise_600x600", 600, 600)]
TIERS = [140, 1120]
# 只缩放一个方向（Resample.c 跳过另一趟）。
ONE_AXIS = [("noise_517x311", 517, 144), ("noise_517x311", 240, 311)]


def target_size(h: int, w: int, max_soft_tokens: int, patch: int = 16, pool: int = 3) -> tuple[int, int]:
  """与 scripts/golden/llama_embed.py 的 target_size 相同。"""
  max_patches = max_soft_tokens * pool * pool
  factor = (max_patches * patch * patch / (h * w)) ** 0.5
  side = pool * patch
  th = int(factor * h // side) * side
  tw = int(factor * w // side) * side
  if th == 0 and tw == 0:
    raise ValueError(f"图像 {w}×{h} 太细长，无法缩放")
  max_side = (max_patches // pool ** 2) * side
  if th == 0:
    return side, min(w // h * side, max_side)
  if tw == 0:
    return min(h // w * side, max_side), side
  return th, tw


def fnv1a64(data: bytes) -> int:
  h = 0xCBF29CE484222325
  for b in data:
    h = ((h ^ b) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
  return h


def write_png(name: str, w: int, h: int, depth: int, color_type: int, data: bytes,
              extra: tuple[tuple[bytes, bytes], ...] = (), idat: bytes | None = None) -> None:
  def chunk(kind: bytes, body: bytes) -> bytes:
    return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))

  if idat is None:
    row = len(data) // h
    idat = zlib.compress(b"".join(b"\x00" + data[y * row:(y + 1) * row] for y in range(h)))
  ihdr = struct.pack(">IIBBBBB", w, h, depth, color_type, 0, 0, 0)
  body = chunk(b"IHDR", ihdr) + b"".join(chunk(k, v) for k, v in extra) + chunk(b"IDAT", idat) + chunk(b"IEND", b"")
  (OUT / name).write_bytes(b"\x89PNG\r\n\x1a\n" + body)


def decode_fixtures(rng: np.random.RandomState) -> None:
  w, h = 31, 17
  rgb16 = rng.randint(0, 65536, (h, w, 3)).astype(">u2")
  write_png("rgb16.png", w, h, 16, 2, rgb16.tobytes())
  (OUT / "rgb16.expected.rgb").write_bytes((rgb16 >> 8).astype(np.uint8).tobytes())
  gray16 = rng.randint(0, 65536, (h, w)).astype(">u2")
  write_png("gray16.png", w, h, 16, 0, gray16.tobytes())
  gray8 = (gray16 >> 8).astype(np.uint8)
  (OUT / "gray16.expected.rgb").write_bytes(np.repeat(gray8[..., None], 3, axis=2).tobytes())
  plte = rng.randint(0, 256, (16, 3), dtype=np.uint8)
  trns = np.array([0, 1, 64, 127, 128, 200, 254, 255, 30, 90], np.uint8)
  idx = rng.randint(0, 16, (h, w), dtype=np.uint8)
  write_png("palette_trns.png", w, h, 8, 3, idx.tobytes(), ((b"PLTE", plte.tobytes()), (b"tRNS", trns.tobytes())))
  la = rng.randint(0, 256, (h, w, 2), dtype=np.uint8)
  write_png("la.png", w, h, 8, 4, la.tobytes())
  write_png("huge.png", 100000, 100000, 8, 2, b"", idat=zlib.compress(b"\x00" * 16))


def main() -> None:
  rng = np.random.RandomState(20261013)
  lines = []
  imgs = {}
  for name, w, h in SOURCES:
    px = rng.randint(0, 256, (h, w, 3), dtype=np.uint8)
    (OUT / f"{name}.rgb").write_bytes(px.tobytes())
    img = imgs[name] = Image.fromarray(px, "RGB")
    for tier in TIERS:
      th, tw = target_size(h, w, tier)
      out = img.resize((tw, th), Image.BICUBIC).tobytes()
      lines.append(f"{name} {w} {h} {tier} {tw} {th} {fnv1a64(out):016x}")
      print(lines[-1], flush=True)
  for name, tw, th in ONE_AXIS:
    img = imgs[name]
    out = img.resize((tw, th), Image.BICUBIC).tobytes()
    lines.append(f"{name} {img.width} {img.height} - {tw} {th} {fnv1a64(out):016x}")
    print(lines[-1], flush=True)
  (OUT / "expected.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")
  decode_fixtures(rng)


if __name__ == "__main__":
  main()
