## rs-meetingrecorder-craft 0.1.0

Initial release: the local meeting recorder — record, transcribe, name the speakers, and let an LLM write the minutes. Everything stays on your machine.

- **Floating mini panel (132×96)** — same footprint as PointToFix Craft's standby bar: status light, elapsed timer, start/stop, draggable, always-on-top optional. `F9` starts/ends a meeting (`F8` stays with PointToFix Craft for screenshots).
- **Live subtitle bar** — a second always-on-top viewport pinned to the bottom of the screen while recording: the last two transcript lines plus a recording timer. On-top, font size, background opacity and bottom margin are configurable in Settings.
- **Local streaming transcription** — whisper.cpp via whisper-rs; a 12-second sliding window with tail continuation commits only finalized segments (no duplicates). Uses the default input device, mixed down to 16 kHz mono, with the original-rate WAV archived for later re-processing.
- **Speaker naming without ceremony** — diarization (pyannote add-on, `scripts/diarize.py`) yields anonymous clusters; you name them once in a compact dialog (sample utterance, talk time, known-name shortcuts). Voiceprint embeddings (SQLite BLOBs, cosine brute-force) are registered on confirmation and pre-fill names in future meetings automatically.
- **LLM minutes in one call** — the whole transcript goes to any OpenAI-compatible endpoint in a single request that must return structured JSON: `summary` / `decisions` / `actions` / `risks`, each item carrying an `evidence_time` back into the transcript. Oversized transcripts fall back to two halves + a merge pass automatically.
- **Meeting packages** — plain files under `%APPDATA%\ltl-meetingrecord\packages\meeting_YYYYMMDD\`: `transcript.json`, `summary.json`, `actions.json`, `decisions.json`, `risks.json`, `screenshots.json`, copied `screenshots/`, `meta.json`. Re-running the pipeline is idempotent.
- **Screenshot correlation** — every F8 shot taken during the meeting is matched to the speech within ±30 s (configurable) and copied into the package with its context text; the gallery database is opened read-only.
- **Settings GUI with built-in tool installer** — whisper model path/language, pyannote python+token, LLM endpoint/key/model, subtitle options, chunk parameters, RAG URL, UI language — plus one-click install/update/launch for PointToFix Craft (direct .exe link, local path, or auto-detected sibling build).
- **Headless CLI** — `mrc-recorder --start` (record until Enter, auto post-process, JSON receipt), `--status`, `--tool-install`, `--tool-launch`; `mrc-postprocess list/run/name/speak/package/rag/fixture-shots` for everything after. Exit codes 0/1/2.
- **RAG ingestion** — chunks and every decision/action/risk are pushed to a local RAG service (`POST /ingest`, idempotent per document id); failures never block the package.
- **i18n** — 中文 / English / Deutsch / 日本語, following the system UI language on first start, switchable in Settings.

Portable layout: unzip anywhere and run `mrc-recorder.exe` — no installer, no VC++ redistributable.

| File | Purpose |
| --- | --- |
| `mrc-recorder.exe` | main program (GUI + headless CLI) |
| `mrc-postprocess.exe` | post-processing CLI |
| `scripts/diarize.py` | speaker diarization add-on (needs python + pyannote.audio) |
| `scripts/fetch_whisper.py` | downloads a whisper ggml model (`python scripts\fetch_whisper.py`) |

**The whisper model is not bundled.** Without it the recorder still archives audio; transcription, subtitles and everything downstream need a model (ggml-small recommended, ~465 MB). An LLM API key is optional — without it the package contains transcript + screenshots only. Speaker diarization additionally needs python + `pyannote.audio` and a HuggingFace token.

Requires Windows 10/11 x64. See the [README](https://github.com/chenghaowan/rs-meetingrecorder-craft#readme) for the pipeline, package format, CLI reference and sibling-tool contracts.
