#!/usr/bin/env python3
"""说话人分离外挂(pyannote)。

用法: python diarize.py <wav> <out.json>
环境: HF_TOKEN(pyannote 模型需要;首次使用需在 HF 上同意
       pyannote/speaker-diarization-3.1 与 pyannote/embedding 的条款)

输出 JSON:
{
  "segments":  [{"start": 12.3, "end": 18.9, "speaker": "SPEAKER_00"}, ...],
  "embeddings": {"SPEAKER_00": [0.012, ...], ...}   # 每簇一条质心向量
}

依赖: pip install pyannote.audio torch torchaudio
"""

import json
import os
import sys


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: diarize.py <wav> <out.json>", file=sys.stderr)
        return 2
    wav_path, out_path = sys.argv[1], sys.argv[2]

    try:
        from pyannote.audio import Pipeline
        from pyannote.audio import Inference
        import numpy as np
    except ImportError:
        print("缺少依赖: pip install pyannote.audio torch torchaudio", file=sys.stderr)
        return 3

    token = os.environ.get("HF_TOKEN")

    try:
        pipeline = Pipeline.from_pretrained(
            "pyannote/speaker-diarization-3.1",
            use_auth_token=token,
        )
    except Exception as e:  # noqa: BLE001
        print(f"加载 diarization 模型失败: {e}", file=sys.stderr)
        return 4

    diarization = pipeline(wav_path)

    segments = []
    turns_by_spk: dict[str, list[tuple[float, float]]] = {}
    for turn, _, speaker in diarization.itertracks(yield_label=True):
        segments.append({
            "start": round(turn.start, 3),
            "end": round(turn.end, 3),
            "speaker": speaker,
        })
        turns_by_spk.setdefault(speaker, []).append((turn.start, turn.end))

    # 每簇质心 embedding:对每段转写区间整窗取向量再平均
    embeddings: dict[str, list[float]] = {}
    try:
        inference = Inference("pyannote/embedding", use_auth_token=token)
        from pyannote.audio.core.io import Audio
        import torch  # noqa: F401

        audio_file = {"audio": wav_path}
        wave, sr = Audio()(audio_file)
        for spk, turns in turns_by_spk.items():
            vecs = []
            for (s, e) in turns:
                i0, i1 = int(s * sr), min(int(e * sr), wave.shape[1])
                if i1 - i0 < sr // 2:  # <0.5s 的片段跳过(太短不稳)
                    continue
                chunk = wave[:, i0:i1].numpy()
                v = np.asarray(inference({"waveform": chunk, "sample_rate": sr}))
                vecs.append(v.reshape(-1))
            if vecs:
                embeddings[spk] = np.mean(vecs, axis=0).astype(float).tolist()
    except Exception as e:  # noqa: BLE001
        # embedding 失败不致命:分离结果照常输出,声纹功能退化为手工命名
        print(f"embedding 提取失败(忽略): {e}", file=sys.stderr)

    result = {"segments": segments, "embeddings": embeddings}
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(result, f, ensure_ascii=False)
    print(f"ok: {len(segments)} turns, {len(embeddings)} speakers")
    return 0


if __name__ == "__main__":
    sys.exit(main())
