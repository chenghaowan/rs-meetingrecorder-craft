//! 说话人分离(sherpa-onnx ONNX,进程内)+ 声纹库(cosine 匹配)。
//!
//! 双流 diarization:现场(mic)与远端(remote)各跑一遍 pyannote 分段 ONNX +
//! CAM++ 声纹嵌入 ONNX + 阈值聚类,全部 CPU 本地;跨流用簇质心余弦合并
//! 同一说话人的簇。输出保持旧契约:segments + 每簇质心 embedding,
//! 供声纹预填与注册。embedding 带模型标识,跨模型质心不比对。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::Connection;

use crate::config::Config;
use crate::db;
use crate::transcribe::load_wav_16k;

/// pyannote segmentation(sherpa-onnx 转换版,~6 MB)
const SEG_REPO: &str = "csukuangfj/sherpa-onnx-pyannote-segmentation-3-0";
const SEG_FILE: &str = "model.onnx";
/// 3D-Speaker CAM++ 中文声纹嵌入(~28 MB)
const EMB_REPO: &str = "csukuangfj/speaker-embedding-models";
const EMB_FILE: &str = "3dspeaker_speech_campplus_sv_zh-cn_16k-common.onnx";
/// 嵌入模型标识(存 voiceprints.embedding_model,跨模型质心不比对)
pub const EMBEDDING_MODEL_ID: &str = "3dspeaker-campplus-sv-zhcn";

const HF_MIRROR: &str = "https://hf-mirror.com";

/// diarization 输出结构(契约与旧 Python 版一致)
#[derive(Debug)]
pub struct DiarizeResult {
    pub segments: Vec<DiarSegment>,
    pub embeddings: BTreeMap<String, Vec<f32>>,
}

#[derive(Debug)]
pub struct DiarSegment {
    pub start: f64,
    pub end: f64,
    pub speaker: String,
}

fn diarize_dir(cfg: &Config) -> PathBuf {
    cfg.data_dir().join("models").join("diarize")
}

/// 模型目录候选优先级之外的固定位置:data_dir\models\diarize
pub fn resolve_models(cfg: &Config) -> Option<(PathBuf, PathBuf)> {
    let dir = diarize_dir(cfg);
    let seg = dir.join(SEG_FILE);
    let emb = dir.join(EMB_FILE);
    (seg.is_file() && emb.is_file()).then_some((seg, emb))
}

/// 确保模型就绪(缺失自动经 hf-mirror 下载;可重复执行,已存在跳过)
pub fn ensure_models(cfg: &Config) -> Result<(PathBuf, PathBuf)> {
    if let Some(p) = resolve_models(cfg) {
        return Ok(p);
    }
    let dir = diarize_dir(cfg);
    std::fs::create_dir_all(&dir)?;
    let seg = dir.join(SEG_FILE);
    let emb = dir.join(EMB_FILE);
    for (dest, repo, file) in [
        (&seg, SEG_REPO, SEG_FILE),
        (&emb, EMB_REPO, EMB_FILE),
    ] {
        if !(dest.is_file() && dest.metadata().map(|m| m.len()).unwrap_or(0) > 0) {
            let url = format!("{HF_MIRROR}/{repo}/resolve/main/{file}");
            eprintln!("[diarize] 下载 {url}");
            crate::stt::download(&url, dest)?;
        }
    }
    eprintln!("[diarize] 模型就绪: {}", dir.display());
    Ok((seg, emb))
}

/// 相对路径(audio\xxx.wav)按 data_dir 解析;绝对路径原样
fn resolve_audio(cfg: &Config, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cfg.data_dir().join(p)
    }
}

/// 双流 diarization:现场与远端各跑一遍 → 跨流质心匹配统一簇号。
/// 任意一路失败跳过该路;两路都失败返回 Err(调用方降级)。
pub fn run_dual(
    cfg: &Config,
    mic_wav: Option<&str>,
    remote_wav: Option<&str>,
) -> Result<DiarizeResult> {
    let (seg_m, emb_m) = ensure_models(cfg)?;
    let mut merged_segments: Vec<DiarSegment> = Vec::new();
    let mut merged_emb: BTreeMap<String, Vec<f32>> = BTreeMap::new();
    for (label, path_str) in [("mic", mic_wav), ("remote", remote_wav)] {
        let Some(path_str) = path_str else { continue };
        let path = resolve_audio(cfg, path_str);
        if !path.is_file() {
            continue;
        }
        let samples = match load_wav_16k(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[diarize] {label} 读音频失败: {e:#}");
                continue;
            }
        };
        if samples.len() < 16000 {
            continue;
        }
        let part = match run_samples(&seg_m, &emb_m, cfg, samples) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[diarize] {label} 分离失败: {e:#}");
                continue;
            }
        };
        // 本地簇键 → 全局簇键:mic 直接保留;remote 质心余弦匹配,不够像则开新簇
        let used: Vec<String> = merged_emb.keys().cloned().collect();
        let mut map: BTreeMap<String, String> = BTreeMap::new();
        for (local_key, emb) in &part.embeddings {
            let global = if label == "mic" || merged_emb.is_empty() {
                local_key.clone()
            } else {
                match best_match(&merged_emb, emb) {
                    Some((k, sim)) if sim >= cfg.diarize.cross_match => k,
                    _ => next_free_key(&used),
                }
            };
            if !merged_emb.contains_key(&global) {
                merged_emb.insert(global.clone(), emb.clone());
            }
            map.insert(local_key.clone(), global);
        }
        for seg in part.segments {
            if let Some(g) = map.get(&seg.speaker) {
                merged_segments.push(DiarSegment {
                    start: seg.start,
                    end: seg.end,
                    speaker: g.clone(),
                });
            }
        }
    }
    if merged_segments.is_empty() {
        bail!("无可分离语音(两路均失败或太短)");
    }
    Ok(DiarizeResult { segments: merged_segments, embeddings: merged_emb })
}

fn best_match(pool: &BTreeMap<String, Vec<f32>>, emb: &[f32]) -> Option<(String, f32)> {
    pool.iter()
        .map(|(k, v)| (k.clone(), cosine(emb, v)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

fn next_free_key(used: &[String]) -> String {
    let mut i = 0;
    loop {
        let k = format!("SPEAKER_{i:02}");
        if !used.contains(&k) {
            return k;
        }
        i += 1;
    }
}

/// 单路样本 → 分段 + 每簇质心 embedding。
/// 簇数未知:num_clusters 传 0 = 阈值聚类(sherpa-rs 对 None 硬编码 4,必须显式覆盖)。
fn run_samples(
    seg_model: &Path,
    emb_model: &Path,
    cfg: &Config,
    samples: Vec<f32>,
) -> Result<DiarizeResult> {
    let dur_s = samples.len() as f64 / 16000.0;
    if dur_s < 1.0 {
        bail!("音频太短({dur_s:.1}s)");
    }
    let mut d = sherpa_rs::diarize::Diarize::new(
        seg_model,
        emb_model,
        sherpa_rs::diarize::DiarizeConfig {
            num_clusters: Some(0),
            threshold: Some(cfg.diarize.threshold),
            min_duration_on: Some(0.0),
            min_duration_off: Some(0.35),
            provider: None,
            debug: false,
        },
    )
    .map_err(|e| anyhow!("diarization 初始化失败: {e}"))?;
    // compute 拿走所有权;质心提取还要按簇切样本,克隆一份(90min ≈ 345 MB)
    let samples_for_emb = samples.clone();
    let segs = d
        .compute(samples, None)
        .map_err(|e| anyhow!("diarization 失败: {e}"))?;
    if segs.is_empty() {
        bail!("未检出说话人段");
    }

    // 按簇收集时间段
    let mut by_cluster: BTreeMap<i32, Vec<(f64, f64)>> = BTreeMap::new();
    let mut segments = Vec::with_capacity(segs.len());
    for s in &segs {
        segments.push(DiarSegment {
            start: s.start as f64,
            end: s.end as f64,
            speaker: format!("SPEAKER_{:02}", s.speaker),
        });
        by_cluster.entry(s.speaker).or_default().push((s.start as f64, s.end as f64));
    }

    // 每簇质心:取最长 ≤3 段(各截 30s)拼样本 → 嵌入 → 均值
    let mut extractor = sherpa_rs::speaker_id::EmbeddingExtractor::new(
        sherpa_rs::speaker_id::ExtractorConfig {
            model: emb_model.display().to_string(),
            num_threads: Some(2),
            ..Default::default()
        },
    )
    .map_err(|e| anyhow!("声纹嵌入器加载失败: {e}"))?;
    let mut embeddings = BTreeMap::new();
    for (spk, mut ranges) in by_cluster {
        ranges.sort_by(|a, b| (b.1 - b.0).total_cmp(&(a.1 - a.0)));
        ranges.truncate(3);
        let mut buf: Vec<f32> = Vec::new();
        for (s, e) in ranges {
            let s0 = ((s * 16000.0) as usize).min(samples_for_emb.len());
            let e0 = ((e * 16000.0) as usize).min(samples_for_emb.len()).max(s0);
            // 单段上限 30s(嵌入模型按固定窗聚合,足够)
            let cap = (s0 + 30 * 16000).min(e0);
            buf.extend_from_slice(&samples_for_emb[s0..cap]);
        }
        if buf.len() < 8000 {
            continue; // <0.5s 的簇不提取,声纹预填自然跳过
        }
        match extractor.compute_speaker_embedding(buf, 16000) {
            Ok(v) => {
                embeddings.insert(format!("SPEAKER_{spk:02}"), v);
            }
            Err(e) => eprintln!("[diarize] SPEAKER_{spk:02} 质心提取失败: {e:#}"),
        }
    }
    Ok(DiarizeResult { segments, embeddings })
}

/// 把 diarization 段按时间重叠合并进 segments(整段替换 speaker;
/// 转写段与 diar 段取重叠最大者)。
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
/// 只比同 embedding 模型的质心(旧 pyannote 质心维度/空间不同,不参与)。
pub fn match_voiceprint(
    conn: &Connection,
    emb: &[f32],
    model_id: &str,
) -> Result<Option<VpMatch>> {
    let mut st = conn.prepare(
        "SELECT name, centroid FROM voiceprints WHERE embedding_model = ?1",
    )?;
    let rows = st.query_map([model_id], |r| {
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
    model_id: &str,
) -> Result<()> {
    let now = db::now_ms();
    let blob = encode_vec(emb);
    let existing: Option<(i64, i64, Vec<u8>)> = conn
        .query_row(
            "SELECT id, dim, centroid FROM voiceprints WHERE name=?1 AND embedding_model=?2",
            rusqlite::params![name, model_id],
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
                "INSERT INTO voiceprints(name, dim, centroid, embedding_model, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?5)",
                rusqlite::params![name, emb.len() as i64, blob, model_id, now],
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
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(encode_vec(v))
}

pub fn emb_from_b64(s: &str) -> Result<Vec<f32>> {
    use base64::Engine;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(s)
        .context("embedding base64 解码失败")?;
    Ok(decode_vec(&raw))
}
