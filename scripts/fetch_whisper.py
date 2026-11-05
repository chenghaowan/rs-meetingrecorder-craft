#!/usr/bin/env python3
"""下载 whisper.cpp ggml 模型(转写用)。

用法:
    python scripts/fetch_whisper.py                 # 默认下 ggml-small.bin 到 <仓库>/models
    python scripts/fetch_whisper.py --model base    # tiny/base/small/medium
    python scripts/fetch_whisper.py --target D:\\somewhere\\models

模型放到 exe 同目录的 models\\ 或数据目录 models\\ 下即可被自动发现
(也可在 config.toml [whisper] model_path 指定绝对路径)。
"""

import argparse
import sys
import urllib.request
from pathlib import Path

MODELS = {
    "tiny":   ("ggml-tiny.bin",   77_651_232),
    "base":   ("ggml-base.bin",   147_952_672),
    "small":  ("ggml-small.bin",  487_720_432),
    "medium": ("ggml-medium.bin", 1_532_482_640),
}
BASE_URL = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}"
# 国内直连备选(魔搭镜像,结构相同)
CN_URL = "https://www.modelscope.cn/models/keepitsimple/whisper.cpp/resolve/master/{}"


def human(n: int) -> str:
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024:
            return f"{n:.1f} {unit}"
        n /= 1024
    return f"{n:.1f} TB"


def fetch(url: str, dst: Path, expect: int) -> bool:
    part = dst.with_suffix(".part")
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "mrc-fetch/0.1"})
        with urllib.request.urlopen(req, timeout=60) as resp, open(part, "wb") as f:
            total = int(resp.headers.get("Content-Length") or expect)
            done = 0
            while True:
                chunk = resp.read(1 << 20)
                if not chunk:
                    break
                f.write(chunk)
                done += len(chunk)
                pct = f"{done / total * 100:.0f}%" if total else f"{done}"
                print(f"\r  {pct} ({human(done)})", end="", flush=True)
        print()
    except Exception as e:  # noqa: BLE001
        print(f"\n  下载失败: {e}")
        part.unlink(missing_ok=True)
        return False
    if expect and part.stat().st_size != expect:
        print(f"  大小不符: {part.stat().st_size} != {expect}")
        part.unlink(missing_ok=True)
        return False
    part.replace(dst)
    return True


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", choices=MODELS, default="small")
    ap.add_argument("--target", default="models")
    args = ap.parse_args()

    name, size = MODELS[args.model]
    dst = Path(args.target)
    dst.mkdir(parents=True, exist_ok=True)
    dst = dst / name
    if dst.exists() and (not size or dst.stat().st_size == size):
        print(f"已存在: {dst}")
        return 0

    for url in (BASE_URL.format(name), CN_URL.format(name)):
        print(f"下载 {args.model}: {url}")
        if fetch(url, dst, size):
            print(f"完成: {dst} ({human(dst.stat().st_size)})")
            return 0
        print("  换下一个源…")
    return 1


if __name__ == "__main__":
    sys.exit(main())
