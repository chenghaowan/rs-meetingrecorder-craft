//! 后处理流水线总控:diarize → 声纹预填 → 命名(可交互) → chunk → 截图匹配
//! → LLM 总结(命名之后跑,产物直接带真名) → 会议包 → RAG 入库。
//!
//! 每步幂等可重跑;命名缺失且非交互时以 PipelineStop::NamingNeeded 返回,
//! 由 CLI/托盘提示后继续。

use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::chunk::{self, Chunk};
use crate::config::Config;
use crate::db::{self, Segment};
use crate::diarize;
use crate::llm::{self, LlmClient};
use crate::package::{self, PackageInput};
use crate::rag::{self, RagClient};
use crate::shots;
use crate::tokens::approx_tokens;
use crate::translate::Translator;

pub enum PipelineStop {
    /// 有说话人簇未命名(非交互模式),带未命名 key 列表
    NamingNeeded(Vec<String>),
}

pub struct Report {
    pub meeting_id: i64,
    pub package_dir: Option<String>,
    pub segments: usize,
    pub chunks: usize,
    pub screenshots: usize,
    pub diarized: bool,
    pub named: usize,
    pub llm_used: bool,
    pub llm_halves: bool,
    pub rag_ingested: Option<usize>,
    pub rag_error: Option<String>,
    pub elapsed_ms: u64,
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "会议#{} 后处理完成: {} 段 / {} chunk / {} 截图 | diarize={} 命名={} LLM={}{}\n  会议包: {}",
            self.meeting_id,
            self.segments,
            self.chunks,
            self.screenshots,
            if self.diarized { "✓" } else { "—" },
            self.named,
            if self.llm_used { "✓" } else { "—" },
            if self.llm_halves { "(两段降级)" } else { "" },
            self.package_dir.as_deref().unwrap_or("(未生成)"),
        )?;
        if let Some(n) = self.rag_ingested {
            write!(f, "\n  RAG 入库: {n} 条")?;
        }
        if let Some(e) = &self.rag_error {
            write!(f, "\n  RAG 入库失败(可 `postprocess rag {}` 重试): {e}", self.meeting_id)?;
        }
        Ok(())
    }
}

pub fn run(
    cfg: &Config,
    conn: &mut Connection,
    meeting_id: i64,
    interactive: bool,
    skip_llm: bool,
    skip_rag: bool,
) -> std::result::Result<Report, (anyhow::Error, Option<PipelineStop>)> {
    let t0 = Instant::now();
    let inner = || -> Result<(Report, Option<PipelineStop>)> {
        let meeting = db::get_meeting(conn, meeting_id)?;
        let ended_at = meeting
            .ended_at
            .with_context(|| format!("会议#{meeting_id} 还没结束,不能后处理"))?;
        let mut segs = db::load_segments(conn, meeting_id)?;

        // ---- 1. diarization(只在 segments 还没说话人时跑一次) ----
        let mut diar_embeddings: BTreeMap<String, Vec<f32>> = BTreeMap::new();
        let mut diarized = false;
        let has_speaker = segs.iter().any(|s| s.speaker.is_some());
        if !has_speaker && cfg.diarize.enabled {
            if let Some(audio) = meeting.audio_path.clone() {
                match diarize::run(cfg, std::path::Path::new(&audio)) {
                    Ok(d) => {
                        let n = diarize::merge_into_segments(conn, meeting_id, &d, meeting.started_at)?;
                        diar_embeddings = d.embeddings.clone();
                        diarized = true;
                        eprintln!("[pipeline] diarization 完成,{} 段已标注,{} 簇", n, diar_embeddings.len());
                        segs = db::load_segments(conn, meeting_id)?;
                    }
                    Err(e) => {
                        eprintln!("[pipeline] diarization 跳过: {e}");
                    }
                }
            }
        }

        // ---- 2. 声纹比对预填 + 命名 ----
        // speakers 表 = 已登记键 ∪ segments 中出现的键(部分登记/重跑均收敛);
        // 全无键(纯静音)→ 视为单说话人 SPEAKER_00
        {
            let mut keys: Vec<String> = segs
                .iter()
                .filter_map(|s| s.speaker.clone())
                .collect();
            keys.sort();
            keys.dedup();
            if keys.is_empty() && !segs.is_empty() {
                keys.push("SPEAKER_00".into());
                conn.execute(
                    "UPDATE segments SET speaker='SPEAKER_00' WHERE meeting_id=?1 AND speaker IS NULL",
                    [meeting_id],
                )?;
            }
            for k in &keys {
                db::upsert_speaker(conn, meeting_id, k, None, None, None)?;
            }
        }
        let speaker_keys: Vec<String> = db::load_speakers(conn, meeting_id)?
            .into_iter()
            .map(|(k, _, _, _)| k)
            .collect();

        for key in &speaker_keys {
            if let Some(emb) = diar_embeddings.get(key) {
                if let Some(m) = diarize::match_voiceprint(conn, emb)? {
                    let certain = m.similarity >= cfg.post.voiceprint_match;
                    let proposed = if certain || m.similarity >= cfg.post.voiceprint_uncertain {
                        Some(m.name.clone())
                    } else {
                        None
                    };
                    db::upsert_speaker(conn, meeting_id, key, None, proposed.as_deref(), Some(m.similarity as f64))?;
                    if certain {
                        // 高置信直接预填名(仍可改)
                        db::upsert_speaker(conn, meeting_id, key, Some(&m.name), None, None)?;
                    }
                }
            }
        }

        // 命名:交互式提示;非交互时有未命名则中断
        let speakers = db::load_speakers(conn, meeting_id)?;
        let unnamed: Vec<String> = speakers
            .iter()
            .filter(|(_, name, _, _)| name.is_none())
            .map(|(k, _, _, _)| k.clone())
            .collect();
        let mut named_count = speakers.iter().filter(|(_, n, _, _)| n.is_some()).count();
        if !unnamed.is_empty() {
            if interactive && std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                named_count = prompt_naming(conn, meeting_id, &segs, &diar_embeddings, cfg, &unnamed)?;
            } else {
                return Ok((
                    Report {
                        meeting_id, package_dir: None, segments: segs.len(), chunks: 0,
                        screenshots: 0, diarized, named: named_count, llm_used: false,
                        llm_halves: false, rag_ingested: None, rag_error: None,
                        elapsed_ms: t0.elapsed().as_millis() as u64,
                    },
                    Some(PipelineStop::NamingNeeded(unnamed)),
                ));
            }
        }
        // ---- 2.5 翻译补齐:录音时没翻成的(服务挂/离线导入)在这里补 ----
        {
            let untranslated: Vec<i64> = segs
                .iter()
                .filter(|s| s.translation.trim().is_empty())
                .map(|s| s.idx)
                .collect();
            if !untranslated.is_empty() {
                match Translator::new(&cfg.translate, &cfg.llm) {
                    Ok(tr) if tr.enabled() => {
                        let texts: Vec<String> = segs
                            .iter()
                            .filter(|s| s.translation.trim().is_empty())
                            .map(|s| s.text.clone())
                            .collect();
                        let outs = tr.translate_batch(&texts);
                        for (k, o) in outs.iter().enumerate() {
                            if let Some(v) = o {
                                let _ = db::update_translation(conn, untranslated[k], meeting_id, v);
                            }
                        }
                        let done = outs.iter().filter(|o| o.is_some()).count();
                        eprintln!("[pipeline] 翻译补齐 {}/{} 段", done, untranslated.len());
                        segs = db::load_segments(conn, meeting_id)?;
                    }
                    _ => eprintln!("[pipeline] 翻译未启用/未配置,跳过补齐"),
                }
            }
        }

        // 展示段:名字已命名的用名字,未命名的保留簇键(簇键永不覆写)
        let segs_named = db::load_segments_named(conn, meeting_id)?;

        // 确认过的簇 → 声纹注册(有 embedding 才做)
        let speakers = db::load_speakers(conn, meeting_id)?;
        for (key, name, _, _) in &speakers {
            if let (Some(nm), Some(emb)) = (name, diar_embeddings.get(key)) {
                diarize::register_voiceprint(conn, nm, emb, meeting_id, cfg.post.voiceprint_max_samples)?;
            }
        }

        // ---- 3. chunk ----
        let chunks = chunk::chunk_segments(&segs_named, &cfg.post);

        // ---- 4. 截图匹配 ----
        let mut links: Vec<shots::ShotLink> = Vec::new();
        let mut shot_files: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
        let shot_db = shots::open_readonly(&cfg.shots);
        if let Ok(sconn) = shot_db {
            match shots::meeting_shots(&sconn, &cfg.shots, meeting.started_at, ended_at) {
                Ok(list) => {
                    links = shots::match_contexts(&list, &segs_named, meeting.started_at, cfg.shots.context_window_secs);
                    let root = shots::root(&cfg.shots);
                    // list 与 links 同序(match_contexts 按 shots 顺序生成)
                    for (sh, link) in list.iter().zip(&links) {
                        let src = root.join(sh.file_rel.replace('/', "\\"));
                        shot_files.push((src, std::path::PathBuf::from(&link.image)));
                    }
                }
                Err(e) => eprintln!("[pipeline] 截图读取失败: {e}"),
            }
        } else {
            eprintln!("[pipeline] 无 gallery.db,跳过截图关联");
        }
        let screenshots = links.len();

        // ---- 5. LLM 总结(命名后,带截图上下文) ----
        let mut summary: Option<Value> = None;
        let mut llm_meta: Option<Value> = None;
        let mut llm_halves = false;
        if !skip_llm && !segs_named.is_empty() {
            match LlmClient::new(&cfg.llm) {
                Ok(client) => {
                    let mut user = build_transcript_text(&segs_named, &chunks);
                    let shot_block = serde_json::to_string_pretty(&links).unwrap_or_default();
                    if !links.is_empty() {
                        user.push_str(&format!("\n\n## 截图上下文(截图时刻附近的发言)\n{shot_block}"));
                    }
                    let est = approx_tokens(&user);
                    let res = if est > (cfg.llm.context_tokens as f64 * 0.8) as usize {
                        llm_halves = true;
                        summarize_in_halves(&client, &chunks, &links)
                    } else {
                        client
                            .chat_json(llm::SUMMARY_SYSTEM, &user, Some(&llm::summary_schema()))
                            .map(|v| (v, false))
                    };
                    match res {
                        Ok((v, _)) => {
                            summary = Some(v.clone());
                            llm_meta = Some(json!({
                                "model": cfg.llm.model,
                                "estimated_input_tokens": est,
                                "halves": llm_halves,
                                "elapsed_ms": t0.elapsed().as_millis() as u64,
                            }));
                        }
                        Err(e) => eprintln!("[pipeline] LLM 总结失败(会议包 summary 留空): {e}"),
                    }
                }
                Err(e) => eprintln!("[pipeline] LLM 跳过: {e}"),
            }
        }

        // ---- 6. 会议包(重跑复用已有目录,幂等覆盖) ----
        let package_dir: std::path::PathBuf = match &meeting.package_dir {
            Some(d) if std::path::Path::new(d).is_dir() => std::path::PathBuf::from(d),
            _ => match package::create_dir(&cfg.packages_dir(), meeting.started_at) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("[pipeline] 会议包目录创建失败: {e}");
                    return Ok((
                        Report {
                            meeting_id, package_dir: None, segments: segs_named.len(), chunks: chunks.len(),
                            screenshots, diarized, named: named_count, llm_used: summary.is_some(),
                            llm_halves, rag_ingested: None, rag_error: None,
                            elapsed_ms: t0.elapsed().as_millis() as u64,
                        },
                        None,
                    ));
                }
            },
        };
        std::fs::create_dir_all(&package_dir).ok();
        // 截图目标路径 = 包内 screenshots/<img>
        let copy_jobs: Vec<(std::path::PathBuf, std::path::PathBuf)> = shot_files
            .iter()
            .map(|(src, img)| (src.clone(), package_dir.join("screenshots").join(img)))
            .collect();
        let inp = PackageInput {
            meeting: &meeting,
            speakers: &speakers,
            segments: &segs_named,
            chunks: &chunks,
            links: &links,
            summary: summary.as_ref(),
            llm_meta: llm_meta.as_ref(),
        };
        package::write_all(&inp, &package_dir, &copy_jobs)?;
        let package_str = package_dir.to_string_lossy().to_string();
        db::set_package_dir(conn, meeting_id, &package_str)?;

        // ---- 7. RAG 入库 ----
        let mut rag_ingested = None;
        let mut rag_error = None;
        if !skip_rag {
            match RagClient::new(&cfg.rag) {
                Ok(client) => {
                    let docs = rag::build_docs(meeting_id, &meeting.title, &chunks, summary.as_ref(), &links);
                    match client.ingest(meeting_id, &meeting.title, &docs) {
                        Ok(n) => rag_ingested = Some(n),
                        Err(e) => rag_error = Some(e.to_string()),
                    }
                }
                Err(e) => {
                    // 未配置不算错误,静默跳过;已配置但连不上才算
                    if cfg.rag.base_url.as_deref().map(|s| !s.is_empty()).unwrap_or(false) {
                        rag_error = Some(e.to_string());
                    }
                }
            }
        }

        db::set_status(conn, meeting_id, "done")?;
        Ok((
            Report {
                meeting_id,
                package_dir: Some(package_str),
                segments: segs.len(),
                chunks: chunks.len(),
                screenshots,
                diarized,
                named: named_count,
                llm_used: summary.is_some(),
                llm_halves,
                rag_ingested,
                rag_error,
                elapsed_ms: t0.elapsed().as_millis() as u64,
            },
            None,
        ))
    };

    match inner() {
        Ok((r, None)) => Ok(r),
        Ok((_, Some(stop))) => Err((anyhow::anyhow!("需要命名后继续"), Some(stop))),
        Err(e) => Err((e, None)),
    }
}

fn build_transcript_text(segs: &[Segment], chunks: &[Chunk]) -> String {
    let head = format!(
        "## 会议转写(共{}段,{}个时间块)\n",
        segs.len(),
        chunks.len()
    );
    head + &chunks
        .iter()
        .map(|c| format!("### chunk{:03} ({})\n{}", c.id, chunk::fmt_mmss(c.start_ms), c.text))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 超上下文降级:按 chunk 中点切两半,各自总结,再一次合并。
/// 返回 (最终JSON, 是否两段)。失败只返回 Err(由外层留空处理)。
fn summarize_in_halves(client: &LlmClient, chunks: &[Chunk], links: &[shots::ShotLink]) -> Result<(Value, bool)> {
    let mid = chunks.len() / 2;
    let (a, b) = chunks.split_at(mid);
    let part = |cs: &[Chunk]| {
        cs.iter()
            .map(|c| format!("### chunk{:03} ({})\n{}", c.id, chunk::fmt_mmss(c.start_ms), c.text))
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    let ua = part(a);
    let ub = part(b);
    let ja = client.chat_json(llm::SUMMARY_SYSTEM, &ua, Some(&llm::summary_schema()))?;
    let jb = client.chat_json(llm::SUMMARY_SYSTEM, &ub, Some(&llm::summary_schema()))?;
    let merged = client.chat_json(
        llm::MERGE_SYSTEM,
        &format!("前半:\n{}\n\n后半:\n{}", serde_json::to_string_pretty(&ja)?, serde_json::to_string_pretty(&jb)?),
        Some(&llm::summary_schema()),
    )?;
    let _ = links;
    Ok((merged, true))
}

/// 交互式命名(控制台)。返回确认命名数。
fn prompt_naming(
    conn: &Connection,
    meeting_id: i64,
    segs: &[Segment],
    embeddings: &BTreeMap<String, Vec<f32>>,
    cfg: &Config,
    unnamed: &[String],
) -> Result<usize> {
    use std::io::Write as _;
    println!("\n═══ 说话人命名(回车接受预填,- 跳过) ═══");
    let mut done = 0usize;
    for key in unnamed {
        let rows: Vec<&Segment> = segs.iter().filter(|s| s.speaker.as_deref() == Some(key)).collect();
        let dur_ms: i64 = rows.iter().map(|s| s.end_ms - s.start_ms).sum();
        let sample = rows.first().map(|s| s.text.chars().take(40).collect::<String>()).unwrap_or_default();
        let (_, _name, proposed, sim) = db::load_speakers(conn, meeting_id)?
            .into_iter()
            .find(|(k, _, _, _)| k == key)
            .unwrap_or((key.clone(), None, None, None));
        // 历史名单(声纹库里的名字)作为提示
        let known: Vec<String> = {
            let mut st = conn.prepare("SELECT name FROM voiceprints ORDER BY updated_at DESC")?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            rows.flatten().take(8).collect()
        };
        println!(
            "\n{key}: 发言 {:.0}分{:.0}秒 / {} 段\n  样例:「{sample}」",
            (dur_ms / 60000) as f64,
            ((dur_ms % 60000) / 1000) as f64,
            rows.len()
        );
        match (&proposed, &sim) {
            (Some(p), Some(s)) => println!("  声纹预填: {p} (相似度 {s:.2})"),
            (Some(p), _) => println!("  预填: {p}"),
            _ => {}
        }
        if !known.is_empty() {
            println!("  历史名单: {}", known.join(", "));
        }
        print!("  名字> ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();
        let input = line.trim();
        let chosen = if input.is_empty() {
            proposed.clone()
        } else if input == "-" {
            None
        } else {
            Some(input.to_string())
        };
        if let Some(nm) = chosen {
            db::upsert_speaker(conn, meeting_id, key, Some(&nm), None, None)?;
            done += 1;
        }
    }
    let _ = (embeddings, cfg);
    Ok(done)
}
