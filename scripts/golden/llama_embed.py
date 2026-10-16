"""黄金向量的参考实现：用 llama.cpp 纯 CPU 预编译包（ctypes 调 llama.dll + mtmd.dll）计算 EmbeddingGemma 2 向量。

运行库：`scripts/fetch-llama.ps1` 取出的 llama.cpp b11461 Windows CPU x64 预编译包
（.cache/third-party/llama.cpp/b11461/win-cpu-x64/），签名以同一标签的 include/llama.h、ggml-backend.h、
tools/mtmd/mtmd.h 为准。模型：ggml-org/embeddinggemma-2-GGUF 的主模型与 mmproj，
放在 src-tauri/dev-data/models/embeddinggemma-2-gguf/。

输入构造仿照参考项目 DigUp（MIT）的 `Sources/LlamaRuntime/LlamaEmbedder.swift`：
- 文本：`llama_tokenize(add_special, parse_special)` 得到 `<bos> 文本 <eos>`；
- 图像 / 音频 / 视频窗口：文本中用 mtmd 的媒体标记占位，`mtmd_tokenize` 展开为
  `<bos> … <|image> 软 token <image|> … <|audio> 软 token <audio|> … <eos>`，媒体块用 `mtmd_encode_chunk`
  编码后把各行作为嵌入输入；
- 整个输入作为一个序列，非因果、一次 `llama_process(ENCODE)`，平均池化后取 `llama_get_embeddings_seq`，
  再用 f64 做 L2 归一化。

预处理：
- 图像用 Pillow 解码，`to_rgb` 转 RGB（带透明通道的图合成到白底，同 transformers `convert_to_rgb`），
  按 `target_size`（transformers Gemma 4 `get_aspect_ratio_preserving_size`）以 PIL 双三次缩放后，把 RGB 字节交给
  `mtmd_bitmap_init`。尺寸已是 48 的倍数，mtmd 不会再缩放；软 token 数与推算不符时报错。
- 音频用 `read_wav` 读成 [-1, 1] 的 float32，交给 `mtmd_bitmap_init_from_audio`，梅尔谱由 mtmd 计算。
- 视频窗口按清单中 `inputs_av` / `inputs_v` 的交错顺序，段与段之间不加空格。

精度：llama.cpp 与 mtmd 的 Flash Attention 默认是 AUTO，CPU 上会启用并把 K、V 与掩码转成 F16，
所以两边都显式关闭。CPU 上矩阵乘的激活按权重的 vec_dot 类型量化（Q8_0 权重 → 激活 Q8_0），这是 ggml CPU 后端的固有行为。

只用 CPU：目录中只有 CPU 后端，`n_gpu_layers = 0`、模型设备列表只放 CPU 设备、mtmd `use_gpu = false`；
加载后枚举全部设备，出现非 CPU 设备即中止。

本模块供 golden_vectors.py 与 dump_golden_inputs.py 导入，不单独运行。
"""

from __future__ import annotations

import ctypes
import io
import os
import wave
from pathlib import Path

import numpy as np
from PIL import Image

REPO_ROOT = Path(__file__).resolve().parents[2]
GOLDEN_DIR = REPO_ROOT / "assets" / "golden"
TAG = "b11461"
LLAMA_DIR = REPO_ROOT / ".cache" / "third-party" / "llama.cpp" / TAG / "win-cpu-x64"
MODEL_DIR = REPO_ROOT / "src-tauri" / "dev-data" / "models" / "embeddinggemma-2-gguf"
# i7-12700H 的性能核数。
DEFAULT_THREADS = 6
# 一个输入必须放进一个微批（非因果编码不能拆批）。与 DigUp 相同取 2048，足够放下 1120 档图像或黄金样本的视频窗口。
MAX_TOKENS = 2048

# llama.h / ggml.h 中的枚举
LLAMA_POOLING_TYPE_MEAN = 1
LLAMA_ATTENTION_TYPE_NON_CAUSAL = 1
LLAMA_FLASH_ATTN = {"auto": -1, "off": 0}
LLAMA_PROCESS_TYPE_ENCODE = 0
GGML_BACKEND_DEVICE_TYPE_CPU = 0
MTMD_INPUT_CHUNK_TYPE_TEXT = 0

vp, cp, i32, u32, f32, b, sz = (ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int32, ctypes.c_uint32, ctypes.c_float,
                                ctypes.c_bool, ctypes.c_size_t)


class ModelParams(ctypes.Structure):
  """include/llama.h `struct llama_model_params`（b11461）。"""
  _fields_ = [("devices", vp), ("tensor_buft_overrides", vp), ("n_gpu_layers", i32), ("split_mode", i32),
              ("load_mode", i32), ("lazy_mode", i32), ("main_gpu", i32), ("tensor_split", vp),
              ("progress_callback", vp), ("progress_callback_user_data", vp), ("kv_overrides", vp),
              ("vocab_only", b), ("check_tensors", b), ("use_extra_bufts", b), ("no_host", b), ("no_alloc", b),
              ("load_mtp", b)]


class ContextParams(ctypes.Structure):
  """include/llama.h `struct llama_context_params`（b11461）。"""
  _fields_ = [("n_ctx", u32), ("n_batch", u32), ("n_ubatch", u32), ("n_seq_max", u32), ("n_rs_seq", u32),
              ("n_outputs_max", u32), ("n_outputs_max_per_seq", u32), ("n_threads", i32), ("n_threads_batch", i32),
              ("ctx_type", i32), ("rope_scaling_type", i32), ("pooling_type", i32), ("attention_type", i32),
              ("flash_attn_type", i32), ("rope_freq_base", f32), ("rope_freq_scale", f32), ("yarn_ext_factor", f32),
              ("yarn_attn_factor", f32), ("yarn_beta_fast", f32), ("yarn_beta_slow", f32), ("yarn_orig_ctx", u32),
              ("defrag_thold", f32), ("cb_eval", vp), ("cb_eval_user_data", vp), ("type_k", i32), ("type_v", i32),
              ("abort_callback", vp), ("abort_callback_data", vp), ("embeddings", b), ("offload_kqv", b),
              ("no_perf", b), ("op_offload", b), ("swa_full", b), ("kv_unified", b), ("samplers", vp),
              ("n_samplers", sz), ("ctx_other", vp)]


class MtmdParams(ctypes.Structure):
  """tools/mtmd/mtmd.h `struct mtmd_context_params`（b11461）。"""
  _fields_ = [("use_gpu", b), ("device", vp), ("print_timings", b), ("n_threads", ctypes.c_int),
              ("image_marker", cp), ("media_marker", cp), ("flash_attn_type", i32), ("warmup", b),
              ("image_min_tokens", ctypes.c_int), ("image_max_tokens", ctypes.c_int), ("cb_eval", vp),
              ("cb_eval_user_data", vp), ("batch_max_tokens", i32), ("progress_callback", vp),
              ("progress_callback_user_data", vp)]


class MtmdInputText(ctypes.Structure):
  _fields_ = [("text", cp), ("text_len", sz), ("add_special", b), ("parse_special", b)]


class LlamaEmbd(ctypes.Structure):
  _fields_ = [("data", ctypes.POINTER(f32)), ("n_rows", sz), ("n_embd", sz)]


LOG_CB = ctypes.CFUNCTYPE(None, ctypes.c_int, ctypes.c_char_p, ctypes.c_void_p)
# 保存 INFO 及以上日志（DEBUG 每次推理上千行），出错时附在错误信息中。
LOGS: list[str] = []
_last_level = [2]


@LOG_CB
def _log(level, text, _user):
  # GGML_LOG_LEVEL_CONT（5）沿用上一条的级别。
  if level != 5:
    _last_level[0] = level
  if text and _last_level[0] >= 2:
    LOGS.append(text.decode("utf-8", "replace"))


_libs: dict[str, ctypes.CDLL] = {}


def libs() -> dict[str, ctypes.CDLL]:
  if _libs:
    return _libs
  # ggml.dll 加载 ggml-cpu-*.dll 时也要能找到 libomp.dll，所以把目录加入进程的 DLL 搜索路径。
  os.add_dll_directory(str(LLAMA_DIR))
  ggml = ctypes.CDLL(str(LLAMA_DIR / "ggml.dll"))
  # 设备查询函数在 ggml-base.dll，后端加载函数在 ggml.dll。
  base = ctypes.CDLL(str(LLAMA_DIR / "ggml-base.dll"))
  llama = ctypes.CDLL(str(LLAMA_DIR / "llama.dll"))
  mtmd = ctypes.CDLL(str(LLAMA_DIR / "mtmd.dll"))
  sigs = {
      ggml: {
          "ggml_backend_load_all_from_path": (None, [cp]),
          "ggml_backend_dev_count": (sz, []),
          "ggml_backend_dev_get": (vp, [sz]),
      },
      base: {
          "ggml_backend_dev_name": (cp, [vp]),
          "ggml_backend_dev_description": (cp, [vp]),
          "ggml_backend_dev_type": (ctypes.c_int, [vp]),
      },
      llama: {
          "llama_log_set": (None, [LOG_CB, vp]),
          "llama_backend_init": (None, []),
          "llama_model_default_params": (ModelParams, []),
          "llama_context_default_params": (ContextParams, []),
          "llama_model_load_from_file": (vp, [cp, ModelParams]),
          "llama_model_free": (None, [vp]),
          "llama_init_from_model": (vp, [vp, ContextParams]),
          "llama_free": (None, [vp]),
          "llama_model_get_vocab": (vp, [vp]),
          "llama_model_n_embd_inp": (i32, [vp]),
          "llama_model_n_embd_out": (i32, [vp]),
          "llama_tokenize": (i32, [vp, cp, i32, ctypes.POINTER(i32), i32, b, b]),
          "llama_batch_ext_init": (vp, [vp]),
          "llama_batch_ext_free": (None, [vp]),
          "llama_batch_ext_clear": (None, [vp]),
          "llama_batch_ext_add_token": (i32, [vp, i32, i32]),
          "llama_batch_ext_add_embd": (i32, [vp, i32, LlamaEmbd]),
          "llama_batch_ext_set_output_embd": (b, [vp, i32, b]),
          "llama_batch_ext_set_pos": (b, [vp, i32, ctypes.POINTER(i32)]),
          "llama_process": (i32, [vp, ctypes.c_int, vp]),
          "llama_get_embeddings_seq": (ctypes.POINTER(f32), [vp, i32]),
      },
      mtmd: {
          "mtmd_log_set": (None, [LOG_CB, vp]),
          "mtmd_default_marker": (cp, []),
          "mtmd_context_params_default": (MtmdParams, []),
          "mtmd_init_from_file": (vp, [cp, vp, MtmdParams]),
          "mtmd_free": (None, [vp]),
          "mtmd_support_vision": (b, [vp]),
          "mtmd_support_audio": (b, [vp]),
          "mtmd_get_audio_sample_rate": (ctypes.c_int, [vp]),
          "mtmd_bitmap_init": (vp, [u32, u32, ctypes.c_char_p]),
          "mtmd_bitmap_init_from_audio": (vp, [sz, ctypes.POINTER(f32)]),
          "mtmd_bitmap_free": (None, [vp]),
          "mtmd_input_chunks_init": (vp, []),
          "mtmd_input_chunks_size": (sz, [vp]),
          "mtmd_input_chunks_get": (vp, [vp, sz]),
          "mtmd_input_chunks_free": (None, [vp]),
          "mtmd_input_chunk_get_type": (ctypes.c_int, [vp]),
          "mtmd_input_chunk_get_tokens_text": (ctypes.POINTER(i32), [vp, ctypes.POINTER(sz)]),
          "mtmd_input_chunk_get_n_tokens": (sz, [vp]),
          "mtmd_tokenize": (i32, [vp, vp, ctypes.POINTER(MtmdInputText), ctypes.POINTER(vp), sz]),
          "mtmd_encode_chunk": (i32, [vp, vp]),
          "mtmd_get_output_embd": (ctypes.POINTER(f32), [vp]),
      },
  }
  for lib, fns in sigs.items():
    for name, (res, args) in fns.items():
      fn = getattr(lib, name)
      fn.restype = res
      fn.argtypes = args
  llama.llama_log_set(_log, None)
  mtmd.mtmd_log_set(_log, None)
  # 预编译包按 GGML_BACKEND_DL 构建：后端是独立 DLL，需显式加载；目录里只有 CPU 后端。路径按 UTF-8 传入。
  ggml.ggml_backend_load_all_from_path(str(LLAMA_DIR).encode("utf-8"))
  llama.llama_backend_init()
  _libs.update(ggml=ggml, base=base, llama=llama, mtmd=mtmd)
  return _libs


def devices() -> list[dict]:
  g, base = libs()["ggml"], libs()["base"]
  out = []
  for k in range(g.ggml_backend_dev_count()):
    d = g.ggml_backend_dev_get(k)
    out.append({"name": base.ggml_backend_dev_name(d).decode(), "description": base.ggml_backend_dev_description(d).decode(),
                "type": base.ggml_backend_dev_type(d), "ptr": d})
  return out


def model_path(kind: str, quant: str) -> Path:
  name = f"embeddinggemma-2-{quant}.gguf" if kind == "text" else f"mmproj-embeddinggemma-2-{quant}.gguf"
  return MODEL_DIR / name


def read_wav(data: bytes) -> np.ndarray:
  with wave.open(io.BytesIO(data)) as w:
    if w.getframerate() != 16000 or w.getnchannels() != 1 or w.getsampwidth() != 2:
      raise ValueError("只支持 16 kHz 单声道 16 位 PCM WAV（黄金样本均为此格式）")
    return np.frombuffer(w.readframes(w.getnframes()), dtype="<i2").astype(np.float32) / 32768.0


def to_rgb(img: Image.Image) -> Image.Image:
  if img.mode == "RGB":
    return img
  rgba = img.convert("RGBA")
  bg = Image.new("RGBA", rgba.size, (255, 255, 255))
  return Image.alpha_composite(bg, rgba).convert("RGB")


def target_size(h: int, w: int, max_soft_tokens: int, patch: int = 16, pool: int = 3) -> tuple[int, int]:
  """Gemma4 `get_aspect_ratio_preserving_size`。"""
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


class LlamaEmbedder:
  def __init__(self, text_model: Path, mmproj: Path | None, threads: int = DEFAULT_THREADS, flash_attn: str = "off"):
    L = libs()
    self.L = L
    llama, mtmd = L["llama"], L["mtmd"]
    devs = devices()
    bad = [d for d in devs if d["type"] != GGML_BACKEND_DEVICE_TYPE_CPU]
    if bad or not devs:
      raise RuntimeError(f"发现非 CPU 设备或没有设备，已中止：{[(d['name'], d['type']) for d in devs]}")
    self.devices = [{k: v for k, v in d.items() if k != "ptr"} for d in devs]
    cpu_devs = (vp * (len(devs) + 1))(*[d["ptr"] for d in devs], None)

    mp = llama.llama_model_default_params()
    mp.n_gpu_layers = 0
    mp.devices = ctypes.cast(cpu_devs, vp)
    self._devs = cpu_devs
    self.model = llama.llama_model_load_from_file(str(text_model).encode("utf-8"), mp)
    if not self.model:
      raise RuntimeError(f"无法加载 {text_model.name}：{''.join(LOGS[-5:])}")
    cp_ = llama.llama_context_default_params()
    cp_.n_ctx = cp_.n_batch = cp_.n_ubatch = MAX_TOKENS
    cp_.n_seq_max = 1
    cp_.n_threads = cp_.n_threads_batch = threads
    cp_.embeddings = True
    cp_.pooling_type = LLAMA_POOLING_TYPE_MEAN
    cp_.attention_type = LLAMA_ATTENTION_TYPE_NON_CAUSAL
    cp_.flash_attn_type = LLAMA_FLASH_ATTN[flash_attn]
    cp_.offload_kqv = False
    cp_.op_offload = False
    self.ctx = llama.llama_init_from_model(self.model, cp_)
    if not self.ctx:
      raise RuntimeError(f"无法创建上下文：{''.join(LOGS[-5:])}")
    self.vocab = llama.llama_model_get_vocab(self.model)
    self.n_embd_inp = llama.llama_model_n_embd_inp(self.model)
    self.n_embd_out = llama.llama_model_n_embd_out(self.model)
    self.batch = llama.llama_batch_ext_init(self.ctx)
    self.media = None
    if mmproj is not None:
      mp2 = mtmd.mtmd_context_params_default()
      mp2.use_gpu = False
      mp2.device = None
      mp2.print_timings = False
      mp2.n_threads = threads
      mp2.flash_attn_type = LLAMA_FLASH_ATTN[flash_attn]
      mp2.warmup = True
      # 图像在调用方已缩放到最终尺寸，mtmd 不再缩放（见模块说明）；上限取模型允许的最大值 1120。
      mp2.image_min_tokens = 1
      mp2.image_max_tokens = 1120
      self.media = mtmd.mtmd_init_from_file(str(mmproj).encode("utf-8"), self.model, mp2)
      if not self.media:
        raise RuntimeError(f"无法加载 {mmproj.name}：{''.join(LOGS[-5:])}")
      self.marker = mtmd.mtmd_default_marker()
      if not (mtmd.mtmd_support_vision(self.media) and mtmd.mtmd_support_audio(self.media)):
        raise RuntimeError("mmproj 不同时支持图像与音频")
      if mtmd.mtmd_get_audio_sample_rate(self.media) != 16000:
        raise RuntimeError("mtmd 的音频采样率不是 16 kHz")

  def close(self) -> None:
    llama, mtmd = self.L["llama"], self.L["mtmd"]
    if self.batch:
      llama.llama_batch_ext_free(self.batch)
    if self.media:
      mtmd.mtmd_free(self.media)
    if self.ctx:
      llama.llama_free(self.ctx)
    if self.model:
      llama.llama_model_free(self.model)
    self.batch = self.media = self.ctx = self.model = None

  # ---------- 输入 ----------

  def tokenize(self, text: str) -> list[int]:
    llama = self.L["llama"]
    data = text.encode("utf-8")
    buf = (i32 * (len(data) + 8))()
    n = llama.llama_tokenize(self.vocab, data, len(data), buf, len(buf), True, True)
    if n < 0:
      raise RuntimeError(f"分词失败（{n}）")
    return list(buf[:n])

  def bitmap(self, part: tuple) -> tuple[int, int]:
    """返回 (mtmd 位图, 按预处理推算的软 token 数；音频为 -1，由 mtmd 决定)。"""
    mtmd = self.L["mtmd"]
    if part[0] == "image":
      img = to_rgb(Image.open(io.BytesIO(part[1])))
      th, tw = target_size(img.height, img.width, part[2])
      if (th, tw) != (img.height, img.width):
        img = img.resize((tw, th), Image.BICUBIC)
      bm = mtmd.mtmd_bitmap_init(tw, th, img.tobytes())
      return bm, (th // 48) * (tw // 48)
    wav = np.ascontiguousarray(part[1], np.float32)
    bm = mtmd.mtmd_bitmap_init_from_audio(len(wav), wav.ctypes.data_as(ctypes.POINTER(f32)))
    return bm, -1

  def _place(self, idx: int, pos: int) -> None:
    llama = self.L["llama"]
    if idx < 0:
      raise RuntimeError(f"批已满或输入无效（{idx}）")
    p = (i32 * 4)(pos, 0, 0, 0)
    if not (llama.llama_batch_ext_set_pos(self.batch, idx, p) and llama.llama_batch_ext_set_output_embd(self.batch, idx, True)):
      raise RuntimeError(f"无法设置第 {idx} 个 token")

  # ---------- 推理 ----------

  def embed(self, parts: list[tuple]) -> tuple[np.ndarray, dict]:
    """parts 为 ("text", 字符串) / ("image", 图像文件字节, 视觉 token 档位) / ("audio", 波形) 列表。

    返回 (768 维向量, {"seq_len": 序列长度, "raw_norm": 归一化前的范数})。
    """
    llama, mtmd = self.L["llama"], self.L["mtmd"]
    llama.llama_batch_ext_clear(self.batch)
    pos = 0
    if all(p[0] == "text" for p in parts):
      tokens = self.tokenize("".join(p[1] for p in parts))
      if len(tokens) > MAX_TOKENS:
        raise RuntimeError(f"输入 {len(tokens)} 个 token，超过 {MAX_TOKENS}")
      for t in tokens:
        self._place(llama.llama_batch_ext_add_token(self.batch, 0, t), pos)
        pos += 1
    else:
      bms, text, expected, got = [], [], [], []
      for p in parts:
        if p[0] == "text":
          text.append(p[1])
          continue
        bm, n = self.bitmap(p)
        if not bm:
          raise RuntimeError("无法创建 mtmd 位图")
        bms.append(bm)
        expected.append(n)
        text.append(self.marker.decode())
      chunks = mtmd.mtmd_input_chunks_init()
      try:
        data = "".join(text).encode("utf-8")
        it = MtmdInputText(data, len(data), True, True)
        arr = (vp * len(bms))(*bms)
        rc = mtmd.mtmd_tokenize(self.media, chunks, ctypes.byref(it), arr, len(bms))
        if rc != 0:
          raise RuntimeError(f"mtmd_tokenize 失败（{rc}）")
        for k in range(mtmd.mtmd_input_chunks_size(chunks)):
          ch = mtmd.mtmd_input_chunks_get(chunks, k)
          if mtmd.mtmd_input_chunk_get_type(ch) == MTMD_INPUT_CHUNK_TYPE_TEXT:
            n = sz()
            toks = mtmd.mtmd_input_chunk_get_tokens_text(ch, ctypes.byref(n))
            for t in toks[:n.value]:
              self._place(llama.llama_batch_ext_add_token(self.batch, 0, t), pos)
              pos += 1
            continue
          if mtmd.mtmd_encode_chunk(self.media, ch) != 0:
            raise RuntimeError("mtmd_encode_chunk 失败")
          rows = mtmd.mtmd_get_output_embd(self.media)
          n = mtmd.mtmd_input_chunk_get_n_tokens(ch)
          got.append(int(n))
          if pos + n > MAX_TOKENS:
            raise RuntimeError(f"输入超过 {MAX_TOKENS} 个 token")
          base = ctypes.addressof(rows.contents)
          for r in range(n):
            # llama_batch_ext_add_embd 复制该行数据（DigUp 注释“copied by the batch”）。
            e = LlamaEmbd(ctypes.cast(base + r * self.n_embd_inp * 4, ctypes.POINTER(f32)), 1, self.n_embd_inp)
            self._place(llama.llama_batch_ext_add_embd(self.batch, 0, e), pos)
            pos += 1
      finally:
        mtmd.mtmd_input_chunks_free(chunks)
        for bm in bms:
          mtmd.mtmd_bitmap_free(bm)
      exp = [e for e in expected if e >= 0]
      img_got = [g for g, e in zip(got, expected) if e >= 0]
      if exp != img_got:
        raise RuntimeError(f"图像软 token 数 {img_got} 与按预处理推算的 {exp} 不符（mtmd 可能又缩放了图像）")
    rc = llama.llama_process(self.ctx, LLAMA_PROCESS_TYPE_ENCODE, self.batch)
    if rc != 0:
      raise RuntimeError(f"llama_process 失败（{rc}）")
    pooled = llama.llama_get_embeddings_seq(self.ctx, 0)
    if not pooled:
      raise RuntimeError("没有池化后的向量")
    raw = np.ctypeslib.as_array(pooled, (self.n_embd_out,)).astype(np.float64)
    norm = float(np.linalg.norm(raw))
    vec = (raw / norm).astype(np.float32) if norm > 0 else raw.astype(np.float32)
    return vec, {"seq_len": pos, "raw_norm": norm}


def golden_parts(manifest: dict, image_tokens: int) -> list[dict]:
  """把黄金样本展开为 embed 的输入。单张图片按 image_tokens 档，视频帧按清单中的 vision_tokens_per_image。"""
  read = lambda rel: (GOLDEN_DIR / rel).read_bytes()  # noqa: E731
  out = []
  for t in manifest["texts"]:
    out.append({"id": t["id"], "modality": "text", "parts": [("text", t["input"])]})
  for im in manifest["images"]:
    out.append({"id": im["id"], "modality": "image", "parts": [("image", read(im["file"]), image_tokens)]})
  for a in manifest["audios"]:
    out.append({"id": a["id"], "modality": "audio", "parts": [("audio", read_wav(read(a["file"])))]})
  for v in manifest["videos"]:
    for variant in ("av", "v"):
      parts = []
      for p in v[f"inputs_{variant}"]:
        if p["type"] == "text":
          parts.append(("text", p["text"]))
        elif p["type"] == "image":
          parts.append(("image", read(p["file"]), v["vision_tokens_per_image"]))
        else:
          parts.append(("audio", read_wav(read(p["file"]))))
      out.append({"id": f"{v['id']}_{variant}", "modality": f"video_{variant}", "parts": parts})
  return out
