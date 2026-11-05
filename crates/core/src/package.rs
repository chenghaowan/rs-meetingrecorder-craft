//! 会议包生成:meeting_YYYYMMDD/ 目录,JSON 全落盘,图片复制快照。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use serde_json::{json, Value};

use crate::chunk::{fmt_mmss, Chunk};
use crate::db::{self, Meeting, Segment};
use crate::shots::ShotLink;

pub struct PackageInput<'a> {
    pub meeting: &'a Meeting,
    pub speakers: &'a [(String, Option<String>, Option<String>, Option<f64>)],
    pub segments: &'a [Segment],
    pub chunks: &'a [Chunk],
    pub links: &'a [ShotLink],
    pub summary: Option<&'a Value>,
    pub llm_meta: Option<&'a Value>,
}

pub fn create_dir(packages_root: &Path, started_at: i64) -> Result<PathBuf> {
    std::fs::create_dir_all(packages_root)?;
    let base = format!("meeting_{}", db::fmt_date(started_at));
    let mut dir = packages_root.join(&base);
    let mut n = 1;
    while dir.exists() {
        n += 1;
        dir = packages_root.join(format!("{base}_{n}"));
    }
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn write_all(inp: &PackageInput, dir: &Path, shot_files: &[(PathBuf, PathBuf)]) -> Result<()> {
    // transcript.json
    let transcript = json!({
        "meeting": {
            "id": inp.meeting.id,
            "title": inp.meeting.title,
            "started_at": db::fmt_local(inp.meeting.started_at),
            "duration_min": inp.meeting.duration_ms().map(|d| d / 60000),
        },
        "speakers": inp.speakers.iter().map(|(k, name, prop, sim)| json!({
            "key": k, "name": name, "proposed": prop, "similarity": sim,
        })).collect::<Vec<_>>(),
        "segments": inp.segments.iter().map(|s| json!({
            "idx": s.idx,
            "start": fmt_mmss(s.start_ms),
            "end": fmt_mmss(s.end_ms),
            "start_ms": s.start_ms,
            "end_ms": s.end_ms,
            "speaker": s.speaker,
            "text": s.text,
            "translation": s.translation,
        })).collect::<Vec<_>>(),
        "chunks": inp.chunks.iter().map(|c| json!({
            "id": format!("chunk{:03}", c.id),
            "start": fmt_mmss(c.start_ms),
            "end": fmt_mmss(c.end_ms),
            "speakers": c.speakers,
        })).collect::<Vec<_>>(),
    });
    write_json(&dir.join("transcript.json"), &transcript)?;

    // summary.json / actions.json / decisions.json
    match inp.summary {
        Some(s) => {
            write_json(&dir.join("summary.json"), s)?;
            write_json(&dir.join("actions.json"), &json!(s["actions"]))?;
            write_json(&dir.join("decisions.json"), &json!(s["decisions"]))?;
            write_json(&dir.join("risks.json"), &json!(s["risks"]))?;
        }
        None => {
            // 无 LLM:留空骨架,保持结构可消费
            let empty: Vec<Value> = vec![];
            write_json(&dir.join("summary.json"), &json!({
                "summary": "", "decisions": empty, "actions": empty, "risks": empty,
            }))?;
            write_json(&dir.join("actions.json"), &json!(empty))?;
            write_json(&dir.join("decisions.json"), &json!(empty))?;
            write_json(&dir.join("risks.json"), &json!(empty))?;
        }
    }

    // screenshots.json + 图片复制快照
    std::fs::create_dir_all(dir.join("screenshots"))?;
    let shots_json: Vec<Value> = inp
        .links
        .iter()
        .map(|l| {
            json!({
                "image": l.image,
                "ts": l.ts,
                "context": l.context,
            })
        })
        .collect();
    write_json(&dir.join("screenshots.json"), &json!(shots_json))?;
    for (src, dst) in shot_files {
        std::fs::copy(src, dst)
            .with_context(|| format!("复制截图失败: {} → {}", src.display(), dst.display()))?;
    }

    // meta.json
    let mut timings: BTreeMap<String, Value> = BTreeMap::new();
    if let Some(m) = inp.llm_meta {
        timings.insert("llm".into(), m.clone());
    }
    write_json(
        &dir.join("meta.json"),
        &json!({
            "format_version": 1,
            "meeting_id": inp.meeting.id,
            "title": inp.meeting.title,
            "started_at": db::fmt_local(inp.meeting.started_at),
            "segments": inp.segments.len(),
            "chunks": inp.chunks.len(),
            "screenshots": inp.links.len(),
            "has_llm_summary": inp.summary.is_some(),
            "timings": timings,
        }),
    )?;
    Ok(())
}

fn write_json(path: &Path, v: &Value) -> Result<()> {
    let text = serde_json::to_string_pretty(v)?;
    std::fs::write(path, text).with_context(|| format!("写文件失败: {}", path.display()))
}
