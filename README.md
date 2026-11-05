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
CPAL capture                            Post-process
  ↓                                       ↓
PCM buffer                              Chunk (5 min / 1000 tokens)
  ↓                                       ↓
whisper.cpp streaming (whisper-rs)      Screenshot ↔ speech matching (±30 s)
  ↓                                       ↓
Speaker diarization (pyannote add-on)   LLM summary — one call, structured JSON
  ↓                                       ↓
meetings.db                    →  transcript.json · summary.json
                                  actions.json · decisions.json
(parallel, separate tool)              ↓
F8 screenshot → gallery.db            Ingest into RAG
```

## Features

- **Two STT engines**: `provider = "local"` runs whisper.cpp in-process (default); `provider = "wanai"` routes audio to the WanAI Runtime gateway (`/asr`, model managed centrally by rs-aiprivaderCraft) and silently falls back to local until the gateway ships M4. The exe keeps whisper.cpp baked in either way (a few MB); what the switch removes is the 465 MB model download/management burden.

- **Floating mini panel** (132×96, same size as PointToFix Craft's standby bar): status light, elapsed time, start/stop, always-on-top, draggable.
- **Live subtitle bar** at the bottom of the screen (a second native viewport): last two transcript lines + a recording timer; on-top, font size, opacity and margin are configurable.
- **Local transcription** with whisper.cpp (whisper-rs): 12 s sliding window with tail continuation — no duplicated segments.
- **Speaker naming, the pragmatic way**: diarization only produces anonymous clusters (`SPEAKER_00`…); names come from a 10-second manual pass (sample utterances, known-name shortcuts), with **voiceprint matching** pre-filling names across meetings (192/512-dim embeddings stored as SQLite BLOBs, cosine brute-force — microseconds at hundreds of speakers).
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
- **Settings GUI**: whisper model path/language, pyannote token, LLM endpoint (any OpenAI-compatible API), subtitle options, chunk parameters, RAG URL — plus a built-in **installer for PointToFix Craft** (direct .exe link, local path, or auto-detected sibling build).
- **i18n**: 中文 / English / Deutsch / 日本語, following the system UI language on first start; switchable in Settings.
- **Headless CLI** for scripts and AI agents (`--start`, `--status`, `--tool-install`, `--tool-launch`), plus a `mrc-postprocess` CLI for re-running the pipeline.

## Requirements

- Windows 10/11 x64
- To build: Rust 2021 + CMake + libclang (`pip install cmake libclang`; `LIBCLANG_PATH` is pre-wired in `.cargo/config.toml` to the pip-provided clang)
- To transcribe: a [whisper.cpp ggml model](https://huggingface.co/ggerganov/whisper.cpp) (`ggml-small.bin` or larger recommended for Chinese)
- Optional: python + `pyannote.audio` for speaker diarization; any OpenAI-compatible API key for the LLM summary

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
- **Local model translation** (`provider = "wanai"`): routes to the WanAI Runtime gateway (`rs-aiprivaderCraft`, `http://127.0.0.1:17800`) whose `/translate` serves a local opus-mt ONNX model - fully offline. Until the gateway ships its M3 translate endpoint, this provider silently degrades to no-translation. `provider = "llm"` remains the cloud alternative.


- **Live subtitle translation** (`[translate]`): `provider = "llm"` reuses the `[llm]` OpenAI-compatible endpoint (zero-code switch once the unified model gateway rs-aiprivaderCraft is up); `"baidu"` uses the Baidu Translate Open API (same appid/secret as ltl-manage); `"none"` disables. LLM mode batches 3-5 lines per call and keeps engineering terms; translations land in `segments.translation`, the second subtitle line, and `transcript.json`.
- **Window capture**: a window-title keyword is resolved to the exact visible rect (DWM frame bounds) and shot through pointtofixcraft's headless `--capture`. Three entry points: CLI `mrc-recorder --shot-window Teams [--shot-window-ocr]`; the 📷 button on the panel while recording; `auto_window_secs` periodic auto-shots. Occluded/minimized windows capture whatever is on screen in that region (inherent to region capture); per-window content capture is planned.

## Contracts with sibling tools

- **PointToFix Craft**: this tool opens `%APPDATA%\pointtofix\gallery.db` **read-only** (`shots` table: `created_at` unix ms, `file_path` relative to `gallery\`, plus its OCR text); images are copied into the package.
- **Local RAG service**: `POST {base_url}/ingest` with `{meeting_id, docs:[{id,type,text,metadata}]}`; idempotent per document id (`m3_chunk_000`, `m3_action_00`, …).

## License

[MIT](LICENSE) © 2026 chenghao.wan
