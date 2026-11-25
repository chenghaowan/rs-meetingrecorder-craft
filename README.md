<div align="center">

# rs-meetingrecorder-craft

**A local-first meeting recorder for Windows — record, transcribe, name the speakers, and let an LLM write the minutes. Written in Rust.**

Record a meeting, see live subtitles at the bottom of your screen, press one button when it's over — and get a structured meeting package: transcript with timestamps, decisions, action items, risks, and every screenshot you took, linked to what was being said.

[![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11%20x64-0078D6?logo=windows&logoColor=white)](#requirements)
[![Rust](https://img.shields.io/badge/rust-2021-DEA584?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Version](https://img.shields.io/badge/version-0.1.0-2ea44f)](#)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

[中文说明](README.zh-CN.md)

</div>

---

rs-meetingrecorder-craft is the "meeting notes" half of a local toolchain: it deliberately does **not** do online meetings or video calls. Screenshots come from the sibling tool [PointToFix Craft](https://github.com/chenghaowan/pointtofixcraft) (shared hotkey philosophy: **F8 = screenshot, F9 = start/end meeting**), and full-text/vector search over the meeting history is handled by a local RAG service. Everything runs on your machine; no audio ever leaves it unless you point the LLM summary at a cloud endpoint.

## The pipeline

```
(during the meeting)                    (after the meeting)
CPAL mic + WASAPI loopback              Post-process
  ↓                                       ↓
PCM buffers (mic / remote)              Chunk (5 min / 1000 tokens)
  ↓                                       ↓
Local SenseVoice + silero-VAD           Screenshot ↔ speech matching (±30 s)
(in-process, sentence-level)              ↓
  ↓                                     LLM summary — one call, structured JSON
Speaker diarization (local ONNX)          ↓
  ↓                                     transcript.json · summary.json
meetings.db                    →        actions.json · decisions.json
(parallel, separate tool)                 ↓
F8 screenshot → gallery.db            Ingest into RAG
```

## Features

- **Four local layers, zero gateway**: capture (mic + WASAPI loopback) → STT (SenseVoice in-process ONNX) → speakers (pyannote-segmentation + CAM++ ONNX, all CPU) → translation (llama.cpp Vulkan + Hy-MT3, optional). Everything runs on this machine.
- **Dual-path capture**: the microphone records the room (`source='mic'`) while a WASAPI loopback tap on the output device catches what meeting apps like Teams play (`source='remote'`) — remote voices and in-room voices land as separate, parallel transcribed streams (two archived WAVs).
- **One STT engine, fully local**: in-process SenseVoice-small via sherpa-onnx (CPU ONNX — several times faster than realtime, punctuation included, zh/en/ja/ko/yue), cut into natural sentences by embedded silero-VAD (~630 KB shipped inside the binary). Models are auto-downloaded once (~230 MB via hf-mirror) and managed by this app itself.

- **Floating mini panel** (132×96, same size as PointToFix Craft's standby bar): status light, elapsed time, start/stop, always-on-top, draggable.
- **Live subtitle bar** at the bottom of the screen (a second native viewport): last two transcript lines + a recording timer; on-top, font size, opacity and margin are configurable.
- **Streaming transcription**: silero-VAD cuts speech into natural sentences (low latency, no fixed-window wait); a fixed-window fallback covers the rare no-VAD case. Failed decodes are retried without dropping audio.
- **Speaker naming, the pragmatic way**: diarization (pyannote segmentation + CAM++ voiceprints, local ONNX, ~34 MB auto-downloaded) only produces anonymous clusters (`SPEAKER_00`…), refined by language cues (short-ack inheritance, turn constraints, vocative exclusion); names come from a 10-second manual pass (sample utterances, known-name shortcuts), with **voiceprint matching** pre-filling names across meetings (embeddings stored as SQLite BLOBs, cosine brute-force; cross-model centroids never compared).
- **LLM minutes in one call**: long-context model summarizes the whole transcript at once and must return structured JSON (summary / decisions / actions / risks, each with an `evidence_time`); oversized transcripts automatically fall back to two halves + a merge pass.
- **Meeting package** — plain files, no lock-in:
  ```
  meeting_20261105/
  ├── transcript.json      # segments + speakers + chunk index
  ├── summary.json         # LLM summary (JSON, never Markdown)
  ├── actions.json  decisions.json  risks.json
  ├── screenshots.json     # {"image","ts","context"} per screenshot
  ├── screenshots/         # img001.png… (copied snapshots)
  └── meta.json
  ```
- **Settings GUI**: STT language, diarization toggle, LLM endpoint (any OpenAI-compatible API), subtitle options, chunk parameters, RAG URL — plus a built-in **installer for PointToFix Craft** (direct .exe link, local path, or auto-detected sibling build).
- **i18n**: 中文 / English / Deutsch / 日本語, following the system UI language on first start; switchable in Settings.
- **Headless CLI** for scripts and AI agents (`--start`, `--status`, `--tool-install`, `--tool-launch`, `--fetch-models`), plus a `mrc-postprocess` CLI for re-running the pipeline.

## Requirements

- Windows 10/11 x64
- To build: Rust 2021 for the app itself (sherpa-rs binds prebuilt binaries; libclang via `pip install libclang` is needed by bindgen — see `.cargo/config.toml` comments)
- To transcribe/diarize: run `mrc-recorder --fetch-models` once (SenseVoice + diarization models download automatically via hf-mirror; meetings also auto-download when missing)
- Optional: `llama-b*-bin-win-vulkan-x64.zip` (llama.cpp release) + a Hy-MT3 GGUF under `%APPDATA%\ltl-meetingrecord\` for offline GPU translation; any OpenAI-compatible API key for the LLM summary

## Build from source

```bash
git clone https://github.com/chenghaowan/rs-meetingrecorder-craft.git
cd rs-meetingrecorder-craft
cargo build --release
# artifacts: target/release/mrc-recorder.exe (GUI + headless CLI)
#           target/release/mrc-postprocess.exe (post-processing CLI)
```

## Quick start

```bash
mrc-postprocess init-config        # writes %APPDATA%\ltl-meetingrecord\config.toml (fill [llm].api_key …)
mrc-recorder                       # GUI; F9 starts/ends a meeting, subtitles appear while recording
mrc-recorder --start --title "DFM review"   # headless: record until Enter, then auto post-process
```

After a meeting ends the pipeline runs by itself: diarization → naming prompt (if needed) → chunks → screenshot matching → LLM summary → package under `%APPDATA%\ltl-meetingrecord\packages\`.

## CLI reference

```bash
mrc-recorder --start [--title X]   # headless recording; JSON receipt on stdout (exit 0/1/2)
mrc-recorder --status              # last 10 meetings + screenshot tool location (JSON)
mrc-recorder --tool-install [src]  # install/update PointToFix Craft (URL or local path)
mrc-recorder --tool-launch         # launch the screenshot tool
mrc-recorder --fetch-models        # download the local SenseVoice model (~230 MB, hf-mirror)

mrc-recorder --transcribe <wav>        # headless offline transcription, JSON segments out
mrc-postprocess list               # meeting list
mrc-postprocess run last           # (re-)run post-processing; interactive naming
mrc-postprocess run 3 --no-llm     # rebuild package without LLM
mrc-postprocess speak 3 SPEAKER_00 # sample utterances (to identify a speaker)
mrc-postprocess name 3 SPEAKER_00 张三
mrc-postprocess rag 3              # retry RAG ingestion
```

An AI-agent skill (`.agents/skills/meetingrecord`) documents the database/package contracts for automated retrieval.

## Live translation & window capture
- **Live subtitle translation** (`[translate]`): `provider = "local"` runs a local llama.cpp Vulkan server with a Hy-MT3 GGUF (offline, GPU, auto-started on 127.0.0.1:17810, official Hy-MT prompt template); `"llm"` reuses the `[llm]` OpenAI-compatible endpoint; `"baidu"` uses the Baidu Translate Open API; `"none"` disables. Translations land in `segments.translation`, the second subtitle line, and `transcript.json`.
- **Window capture**: a window-title keyword is resolved to the exact visible rect (DWM frame bounds) and shot through pointtofixcraft's headless `--capture`. Three entry points: CLI `mrc-recorder --shot-window Teams [--shot-window-ocr]`; the 📷 button on the panel while recording; `auto_window_secs` periodic auto-shots. Occluded/minimized windows capture whatever is on screen in that region (inherent to region capture); per-window content capture is planned.

## Contracts with sibling tools

- **PointToFix Craft**: this tool opens `%APPDATA%\pointtofix\gallery.db` **read-only** (`shots` table: `created_at` unix ms, `file_path` relative to `gallery\`, plus its OCR text); images are copied into the package.
- **Local RAG service**: `POST {base_url}/ingest` with `{meeting_id, docs:[{id,type,text,metadata}]}`; idempotent per document id (`m3_chunk_000`, `m3_action_00`, …).

## License

[MIT](LICENSE) © 2026 chenghao.wan
