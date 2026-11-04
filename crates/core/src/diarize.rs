//! 说话人分离(pyannote 子进程)+ 声纹库(cosine 匹配)。
//!
//! diarize.py 负责:整段 WAV → 说话人分段 + 每簇一条 192/512 维 embedding。
//! Rust 侧只做:spawn、解析 JSON、写 speakers 表(名暂空)、拿 embedding
//! 去声纹库比对预填名。声纹就是 f32 向量存 BLOB,余弦暴力扫,百人库微秒级。

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use rusqlite::Connection;
use serde::Deserialize;

use crate::config::Config;
use crate::db;

/// diarize.py 输出结构
#[derive(Debug, Deserialize)]
pub struct DiarizeResult {
    pub segments: Vec<DiarSegment>,
    #[serde(default)]
    pub embeddings: std::collections::BTreeMap<String, Vec<f32>>,
}

#[derive(Debug, Deserialize)]
pub struct DiarSegment {
    pub start: f64,
    pub end: f64,
    pub speaker: String,
}

pub fn script_path(cfg: &Config) -> Option<std::path::PathBuf> {
    if let Some(p) = &cfg.diarize.script {
        return p.exists().then_some(p.clone());
    }
    let mut candidates = vec![];
    if let Ok(exe) = std::env::current_exe() {
        candidates.push(exe.parent().unwrap().join("scripts").join("diarize.py"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("scripts").join("diarize.py"));
        candidates.push(cwd.join("diarize.py"));
    }
    candidates.into_iter().find(|p| p.exists())
}

/// 跑 diarization。失败返回 Err(调用方降级:继续无说话人流程)。
pub fn run(cfg: &Config, wav: &Path) -> Result<DiarizeResult> {
    let script = script_path(cfg).ok_or_else(|| anyhow!("找不到 diarize.py"))?;
    let python = cfg
        .diarize
        .python
        .clone()
        .unwrap_or_else(|| "python".to_string());
    let out_json = wav.with_extension("diar.json");

    let mut cmd = Command::new(&python);
    cmd.arg(&script)
        .arg(wav)
        .arg(&out_json)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(tok) = &cfg.diarize.hf_token {
        cmd.env("HF_TOKEN", tok);
    }
    let output = cmd
        .output()
        .with_context(|| format!("启动 python 失败: {python}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let _ = std::fs::remove_file(&out_json);
        bail!("diarize.py 失败: {}", err.lines().last().unwrap_or(""));
    }
    let text = std::fs::read_to_string(&out_json).context("读 diarize 输出失败")?;
    let _ = std::fs::remove_file(&out_json);
    serde_json::from_str(&text).context("解析 diarize 输出失败")
}

/// 把 diarization 段按时间重叠合并进 segments(整段替换 speaker;
/// whisper 段与 diar 段取重叠最大者)。
pub fn merge_into_segments(
    conn: &Connection,
    meeting_id: i64,
    diar: &DiarizeResult,
    meeting_started_ms: i64,
) -> Result<usize> {
    let mut updated = 0;
    for seg in &db::load_segments(conn, meeting_id)? {
        let abs0 = meeting_started_ms + seg.start_ms;
        let abs1 = meeting_started_ms + seg.end_ms;
        let mut best: Option<(&str, f64)> = None;
        for d in &diar.segments {
            let d0 = meeting_started_ms + (d.start * 1000.0) as i64;
            let d1 = meeting_started_ms + (d.end * 1000.0) as i64;
            let overlap = (abs1.min(d1) - abs0.max(d0)).max(0) as f64;
            if overlap > 0.0 && best.map(|(_, o)| overlap > o).unwrap_or(true) {
                best = Some((&d.speaker, overlap));
            }
        }
        if let Some((spk, _)) = best {
            conn.execute(
                "UPDATE segments SET speaker=?1 WHERE meeting_id=?2 AND idx=?3",
                rusqlite::params![spk, meeting_id, seg.idx],
            )?;
            updated += 1;
        }
    }
    // speakers 表登记簇
    let mut keys: Vec<&str> = diar
        .segments
        .iter()
        .map(|s| s.speaker.as_str())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    for k in keys {
        db::upsert_speaker(conn, meeting_id, k, None, None, None)?;
    }
    Ok(updated)
}

// ---------- 声纹库 ----------

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na <= 1e-9 || nb <= 1e-9 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

pub fn encode_vec(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

pub fn decode_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

pub struct VpMatch {
    pub name: String,
    pub similarity: f32,
}

/// 拿一条 embedding 与声纹库全体质心比对,返回最高分。
pub fn match_voiceprint(conn: &Connection, emb: &[f32]) -> Result<Option<VpMatch>> {
    let mut st = conn.prepare("SELECT name, centroid FROM voiceprints")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Vec<u8>>(1)?,
        ))
    })?;
    let mut best: Option<VpMatch> = None;
    for row in rows {
        let (name, blob) = row?;
        let c = decode_vec(&blob);
        let sim = cosine(emb, &c);
        if best.as_ref().map(|b| sim > b.similarity).unwrap_or(true) {
            best = Some(VpMatch { name, similarity: sim });
        }
    }
    Ok(best)
}

/// 人工确认命名后:把簇 embedding 注册进声纹库(新库插入/已有人并样本+重算质心)。
pub fn register_voiceprint(
    conn: &Connection,
    name: &str,
    emb: &[f32],
    meeting_id: i64,
    max_samples: usize,
) -> Result<()> {
    let now = db::now_ms();
    let blob = encode_vec(emb);
    let existing: Option<(i64, i64, Vec<u8>)> = conn
        .query_row(
            "SELECT id, dim, centroid FROM voiceprints WHERE name=?1",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    match existing {
        Some((id, dim, old_centroid)) => {
            conn.execute(
                "INSERT INTO voiceprint_samples(voiceprint_id, embedding, meeting_id)
                 VALUES (?1,?2,?3)",
                rusqlite::params![id, blob, meeting_id],
            )?;
            // 质心 = 旧质心与旧样本数 + 新样本的增量均值
            let n: i64 = conn.query_row(
                "SELECT COUNT(*) FROM voiceprint_samples WHERE voiceprint_id=?1",
                [id],
                |r| r.get(0),
            )?;
            let old = decode_vec(&old_centroid);
            if dim as usize == emb.len() && !old.is_empty() {
                let mut c = old;
                for (i, v) in emb.iter().enumerate() {
                    c[i] = c[i] + (v - c[i]) / (n as f32);
                }
                conn.execute(
                    "UPDATE voiceprints SET centroid=?2, updated_at=?3 WHERE id=?1",
                    rusqlite::params![id, encode_vec(&c), now],
                )?;
            }
            // 淘汰最旧样本
            conn.execute(
                "DELETE FROM voiceprint_samples WHERE voiceprint_id=?1 AND id NOT IN (
                   SELECT id FROM voiceprint_samples WHERE voiceprint_id=?1
                   ORDER BY id DESC LIMIT ?2)",
                rusqlite::params![id, max_samples],
            )?;
        }
        None => {
            conn.execute(
                "INSERT INTO voiceprints(name, dim, centroid, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?4)",
                rusqlite::params![name, emb.len() as i64, blob, now],
            )?;
            let id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO voiceprint_samples(voiceprint_id, embedding, meeting_id)
                 VALUES (?1,?2,?3)",
                rusqlite::params![id, blob, meeting_id],
            )?;
        }
    }
    Ok(())
}

/// 序列化 embedding 为 base64(JSON 传输用)
pub fn emb_to_b64(v: &[f32]) -> String {
    base64::engine::general_purpose::STANDARD.encode(encode_vec(v))
}

pub fn emb_from_b64(s: &str) -> Result<Vec<f32>> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(s)
        .context("embedding base64 解码失败")?;
    Ok(decode_vec(&raw))
}
