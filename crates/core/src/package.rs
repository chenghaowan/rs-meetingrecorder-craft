//! 会议包生成:meeting_YYYYMMDD/ 目录,JSON 全落盘,图片复制快照,
//! 另产出人类可读的会议纪要 minutes.md(截图缩图转 webp、base64 内嵌)。

use std::collections::BTreeMap;
use std::io::Cursor;
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
    /// minutes.md 截图 base64 内嵌([shots].embed_minutes_images;Tinta 等阅读器不支持)
    pub embed_images: bool,
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
            "source": s.source,
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

    // minutes.md — 人类可读会议纪要(LLM 总结 + 逐段转写;截图按配置内嵌或文件引用)
    write_minutes_md(inp, &dir.join("minutes.md"), shot_files)?;
    Ok(())
}

/// 内嵌图上限:base64 超过该长度则回退为文件引用(md 不至于撑爆)
const EMBED_MAX_B64: usize = 4_000_000;
/// 内嵌缩图的最长边(逻辑可读性足够,体积可控)
const EMBED_MAX_SIDE: u32 = 1280;

/// 截图 → 缩图(最长边 EMBED_MAX_SIDE)→ webp → base64 data URI。
/// 解码失败返回 Err(调用方回退文件引用)。
fn webp_data_uri(src: &Path) -> Result<String> {
    let img = image::open(src)?;
    let (w, h) = (img.width(), img.height());
    let img = if w.max(h) > EMBED_MAX_SIDE {
        let s = EMBED_MAX_SIDE as f32 / w.max(h) as f32;
        img.resize_exact(
            ((w as f32 * s) as u32).max(1),
            ((h as f32 * s) as u32).max(1),
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let mut out = Cursor::new(Vec::new());
    // image 0.25 的 webp 编码仅支持无损;压缩靠缩图(最长边 1280)
    let enc = image::codecs::webp::WebPEncoder::new_lossless(&mut out);
    enc.encode(rgba.as_raw(), rgba.width(), rgba.height(), image::ExtendedColorType::Rgba8)?;
    let bytes = out.into_inner();
    use base64::Engine;
    Ok(format!(
        "data:image/webp;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// 会议纪要 Markdown:无 LLM 总结时也生成(纪要骨架 + 转写全文 + 截图内嵌)
fn write_minutes_md(inp: &PackageInput, path: &Path, shot_files: &[(PathBuf, PathBuf)]) -> Result<()> {
    let mut md = String::with_capacity(16 << 10);
    let m = inp.meeting;
    md.push_str(&format!("# {}\n\n", m.title));
    md.push_str(&format!("- 开始: {}\n", db::fmt_local(m.started_at)));
    if let Some(ended) = m.ended_at {
        md.push_str(&format!("- 结束: {}\n", db::fmt_local(ended)));
        if let Some(dur) = m.duration_ms() {
            md.push_str(&format!("- 时长: {} 分钟\n", dur / 60000));
        }
    }
    let names: Vec<String> = inp
        .speakers
        .iter()
        .filter_map(|(_, name, _, _)| name.clone())
        .collect();
    if !names.is_empty() {
        md.push_str(&format!("- 参会人: {}\n", names.join(", ")));
    }
    md.push_str(&format!("- 段落: {} · 截图: {}\n", inp.segments.len(), inp.links.len()));

    // ---- 纪要(LLM 总结)----
    md.push_str("\n## 纪要\n\n");
    match inp.summary {
        Some(s) => {
            let text = s["summary"].as_str().unwrap_or("").trim();
            if text.is_empty() {
                md.push_str("(LLM 未产出总结)\n");
            } else {
                md.push_str(text);
                md.push_str("\n");
            }
            let decisions = s["decisions"].as_array();
            let actions = s["actions"].as_array();
            let risks = s["risks"].as_array();
            md.push_str("\n## 决议\n\n");
            match decisions.filter(|a| !a.is_empty()) {
                Some(list) => {
                    for d in list {
                        md.push_str(&format!(
                            "- {}({})\n",
                            d["item"].as_str().unwrap_or(""),
                            d["evidence_time"].as_str().unwrap_or("")
                        ));
                    }
                }
                None => md.push_str("(无)\n"),
            }
            md.push_str("\n## 行动项\n\n");
            match actions.filter(|a| !a.is_empty()) {
                Some(list) => {
                    md.push_str("| 负责人 | 任务 | 期限 | 时间点 |\n|---|---|---|---|\n");
                    for a in list {
                        md.push_str(&format!(
                            "| {} | {} | {} | {} |\n",
                            a["owner"].as_str().unwrap_or(""),
                            a["task"].as_str().unwrap_or(""),
                            a.get("due").and_then(Value::as_str).unwrap_or(""),
                            a.get("evidence_time").and_then(Value::as_str).unwrap_or("")
                        ));
                    }
                }
                None => md.push_str("(无)\n"),
            }
            md.push_str("\n## 风险\n\n");
            match risks.filter(|a| !a.is_empty()) {
                Some(list) => {
                    for r in list {
                        md.push_str(&format!(
                            "- {}({})\n",
                            r["risk"].as_str().unwrap_or(""),
                            r["evidence_time"].as_str().unwrap_or("")
                        ));
                    }
                }
                None => md.push_str("(无)\n"),
            }
        }
        None => md.push_str("(未生成:配置 [llm] 后重跑 `mrc-postprocess run` 可补)\n"),
    }

    // ---- 转写全文 ----
    md.push_str("\n## 转写全文\n\n");
    if inp.segments.is_empty() {
        md.push_str("(无转写内容)\n");
    }
    for s in inp.segments {
        let who = s.speaker.clone().unwrap_or_else(|| "未知".into());
        let mut src_tag = "";
        if s.source.as_deref() == Some("remote") {
            src_tag = "(远端)";
        }
        md.push_str(&format!("**[{}] {}{}**\n\n{}\n", fmt_mmss(s.start_ms), who, src_tag, s.text));
        let trn = s.translation.trim();
        if !trn.is_empty() {
            md.push_str(&format!("> {trn}\n"));
        }
        md.push('\n');
    }

    // ---- 截图(embed_minutes_images=true 时转 webp base64 内嵌,失败/超限回退
    //      文件引用;默认 false 用文件引用——Tinta/Nomo 等阅读器不支持 data URI,
    //      但标准 markdown 图片语法(相对路径)Nomo 均可渲染) ----
    if !inp.links.is_empty() {
        md.push_str("\n## 截图\n\n");
        // 包内文件名 → 包外源路径(shot_files 与 links 无序对应,用文件名索引)
        let src_of: BTreeMap<String, &PathBuf> = shot_files
            .iter()
            .filter_map(|(src, dst)| {
                dst.file_name().map(|n| (n.to_string_lossy().to_string(), src))
            })
            .collect();
        for l in inp.links {
            let ctx = l.context.replace('\n', " ");
            let embedded = if inp.embed_images {
                src_of
                    .get(&l.image)
                    .and_then(|src| webp_data_uri(src).ok())
                    .filter(|uri| uri.len() <= EMBED_MAX_B64)
            } else {
                None
            };
            match embedded {
                Some(uri) => {
                    md.push_str(&format!("![{}]({})\n\n", l.ts, uri));
                    if !ctx.is_empty() {
                        md.push_str(&format!("- **[{}]** {}\n\n", l.ts, ctx));
                    }
                }
                None => {
                    // 相对路径 markdown 图片(会议包自带 screenshots/,整体拷走不失效)
                    md.push_str(&format!("![{}]({})\n\n", l.ts, l.image));
                    if !ctx.is_empty() {
                        md.push_str(&format!("- **[{}]** {}\n\n", l.ts, ctx));
                    }
                }
            }
        }
    }

    std::fs::write(path, md).with_context(|| format!("写文件失败: {}", path.display()))
}

fn write_json(path: &Path, v: &Value) -> Result<()> {
    let text = serde_json::to_string_pretty(v)?;
    std::fs::write(path, text).with_context(|| format!("写文件失败: {}", path.display()))
}
