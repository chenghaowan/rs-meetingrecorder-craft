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

rs-meetingrecorder-craft is the "meeting notes" half of a local toolchain: it deliberately does **not** do online meetings or video calls — and it does **not** do RAG; the post-meeting artifact is a human-readable `minutes.md`. Screenshots come from the sibling tool [PointToFix Craft](https://github.com/chenghaowan/pointtofixcraft) (shared hotkey philosophy: **F8 = screenshot, F9 = start/end meeting**). Everything runs on your machine; no audio ever leaves it unless you point the LLM summary at a cloud endpoint.

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
F8 screenshot → gallery.db            minutes.md (the minutes)
```

## Features

- **Four local layers, zero gateway**: capture (mic + WASAPI loopback) → STT (SenseVoice in-process ONNX) → speakers (pyannote-segmentation + CAM++ ONNX, all CPU) → translation (llama.cpp Vulkan + Hy-MT3, optional). Everything runs on this machine.
- **Dual-path capture**: the microphone records the room (`source='mic'`) while a WASAPI loopback tap on the output device catches what meeting apps like Teams play (`source='remote'`) — remote voices and in-room voices land as separate, parallel transcribed streams (two archived WAVs).
- **One STT engine, fully local**: in-process SenseVoice-small via sherpa-onnx (CPU ONNX — several times faster than realtime, punctuation included, zh/en/ja/ko/yue), cut into natural sentences by embedded silero-VAD (~630 KB shipped inside the binary). Models are auto-downloaded once (~230 MB via hf-mirror) and managed by this app itself.

- **Floating mini panel** (132×96, same size as PointToFix Craft's standby bar): status light, elapsed time, start/stop, **✂ one-click shot** (recording only; captures the configured target window only — e.g. Teams — falling back to the full desktop when unset; the shot goes straight into the current meeting and lands in the package after it ends), always-on-top, draggable.
- **Live subtitle bar** at the bottom of the screen (a second native viewport): last two transcript lines + a recording timer; on-top, font size, opacity and margin are configurable.
- **Streaming transcription**: silero-VAD cuts speech into natural sentences (low latency, no fixed-window wait); a fixed-window fallback covers the rare no-VAD case. Failed decodes are retried without dropping audio.
- **Speaker naming, the pragmatic way**: while recording, speakers are **labeled live as A/B/C** (per-segment CAM++ voiceprint clustering; the same person on mic and loopback lands in one cluster, threshold from `[diarize]`) — click the A tag in the transcript list to rename it on the spot (subtitles, list and package all follow). After the meeting the naming window finishes the pass (sample utterances, known-name shortcuts), with language-cue refinement (short-ack inheritance, turn constraints, vocative exclusion); **voiceprint matching** pre-fills names across meetings (live cluster centroids are persisted with the meeting; embeddings stored as SQLite BLOBs, cosine brute-force; cross-model centroids never compared). The no-VAD fallback still uses offline post-meeting diarization (pyannote segmentation → `SPEAKER_00`…).
- **LLM minutes in one call, API or local**: the model summarizes the whole transcript at once and must return structured JSON (summary / decisions / actions / risks, each with an `evidence_time`); oversized transcripts automatically fall back to two halves + a merge pass. `[llm].provider` picks between `"api"` (any OpenAI-compatible endpoint) and `"local"` (llama-server + local GGUF, offline). The local tier is **auto-recommended from your GPU's dedicated VRAM** (DXGI detection, one-click download into `data_dir\models\llm` from Settings): ≥11 GB → Qwen3-8B@32k; ≥7.5 GB → Qwen3-8B@16k; ≥5 GB → Qwen3-4B@16k; integrated graphics / no dGPU → Qwen3-1.7B@16k (runs fine on thin laptops via iGPU or CPU).
- **Meeting package** — plain files, no lock-in:
  ```
  meeting_20261105/
  ├── minutes.md           # the minutes (human-readable: summary/decisions/actions/risks + full transcript + screenshots)
  ├── transcript.json      # segments + speakers + chunk index
  ├── summary.json         # LLM summary (JSON, never Markdown)
  ├── actions.json  decisions.json  risks.json
  ├── screenshots.json     # {"image","ts","context"} per screenshot
  ├── screenshots/         # img001.png… (copied snapshots)
  └── meta.json
  ```
- **Built-in Nomo reader**: the portable build ships a single-file Markdown reader, [Nomo](https://github.com/nomo-md/nomo) (`thirdpart\Nomo\`, redistributed unmodified under AGPL-3.0 as a separate process — does not affect this project's licensing). After a meeting, expand the panel and click **"View in Nomo"** to see minutes and screenshots in one view — screenshots in minutes.md use standard relative-path markdown images (no base64 embedding: Tinta/Nomo and some readers don't support data URIs), so any image-capable Markdown reader works too; the button hides itself when Nomo is absent.
- **Settings GUI** (resizable, scrolling): STT language dropdown (auto/zh/en/ja/ko/yue), diarization toggle, summary engine API/local switch, subtitle options, chunk parameters, subtitle translation (target-language dropdown) — plus a built-in **installer for PointToFix Craft** (direct .exe link, local path, or auto-detected sibling build, with release-page links for both mirrors).
- **i18n**: 中文 / English / Deutsch / 日本語, following the system UI language on first start; switchable in Settings.
- **Headless CLI** for scripts and AI agents (`--start`, `--status`, `--tool-install`, `--tool-launch`, `--fetch-models`), plus a `mrc-postprocess` CLI for re-running the pipeline.

## Requirements

- Windows 10/11 x64
- To build: Rust 2021 for the app itself (sherpa-rs binds prebuilt binaries; libclang via `pip install libclang` is needed by bindgen — see `.cargo/config.toml` comments)
- To transcribe/diarize: run `mrc-recorder --fetch-models` once (SenseVoice + diarization models download automatically via hf-mirror; meetings also auto-download when missing)
- LLM summary (either one): any OpenAI-compatible API key; or a local GGUF — Settings has a one-click "download recommended model" that **picks the tier from your GPU's dedicated VRAM** (≥11GB → Qwen3-8B 4.7GB / ≥7.5GB → 8B@16k / ≥5GB → [Qwen3-4B](https://hf-mirror.com/unsloth/Qwen3-4B-Instruct-2507-GGUF) 2.3GB / iGPU → [Qwen3-1.7B](https://hf-mirror.com/unsloth/Qwen3-1.7B-GGUF) 1.0GB). `[llm.local].server_exe` left empty reuses the translation llama-server ([llama.cpp releases](https://github.com/ggml-org/llama.cpp/releases); falls back to CPU automatically without a GPU)
- Optional: a Hy-MT3 GGUF under `%APPDATA%\ltl-meetingrecord\models\hy-mt3\` for offline GPU translation

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

After a meeting ends the pipeline runs by itself: diarization → naming prompt (if needed) → chunks → screenshot matching → LLM summary → package (with minutes.md) under `%APPDATA%\ltl-meetingrecord\packages\`.

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
mrc-postprocess run 3 --no-llm     # rebuild package without LLM (minutes.md included)
mrc-postprocess speak 3 SPEAKER_00 # sample utterances (to identify a speaker)
mrc-postprocess name 3 SPEAKER_00 张三
```

An AI-agent skill (`.agents/skills/meetingrecord`) documents the database/package contracts for automated retrieval.

## Live translation & window capture
- **Live subtitle translation** (`[translate]`): the source language is **auto-detected** (the Hy-MT3 template never needs one; the Baidu API uses `from=auto`) — only the target language is configured (dropdown). `provider = "local"` runs a local llama.cpp Vulkan server with a Hy-MT3 GGUF (offline, GPU, auto-started on 127.0.0.1:17810, official Hy-MT prompt template); `"api"` (config key still `"llm"`) reuses the `[llm]` OpenAI-compatible endpoint; `"baidu"` uses the Baidu Translate Open API; `"none"` disables. Translations land in `segments.translation`, the second subtitle line, and `transcript.json`.
- **Window capture**: a window-title keyword is resolved to the exact visible rect (DWM frame bounds). **All in-meeting shots (✂ full screen / 📷 window / periodic auto) are "meeting-direct"**: they go through pointtofixcraft's headless CLI with `--out`, writing straight into `data_dir\shots\meeting_<id>\` — **pointtofix's gallery is not touched**; after the meeting they are copied into the package's `screenshots/` and matched to nearby speech by time. The gallery stays read-only and is still consulted for shots you take yourself in pointtofixcraft (F8) — both sources merge by time with continuous numbering. CLI `mrc-recorder --shot-window Teams [--shot-window-ocr]` still uses the gallery (standalone use). Occluded/minimized windows capture whatever is on screen in that region (inherent to region capture); per-window content capture is planned.

## Contracts with sibling tools

- **PointToFix Craft**: this tool only reuses its **headless capture CLI** (`--capture x,y,w,h` / `--capture-full`, with `--out` to write a file without touching its gallery); in-meeting shots no longer write to gallery.db, which stays read-only for linking shots you took in pointtofixcraft yourself. Portable zips on [GitHub Releases](https://github.com/chenghaowan/pointtofixcraft/releases) and [Gitee Releases](https://gitee.com/chenghao_wan/pointtofixcraft/releases).

## License

[MIT](LICENSE) © 2026 chenghao.wan
