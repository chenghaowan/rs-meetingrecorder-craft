//! 截图消费:只读打开 pointtofixcraft 的 gallery.db,
//! 按 created_at ∈ [meeting 起,止] 筛选,±窗口取发言做 context。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::config::ShotsCfg;
use crate::db::Segment;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShotLink {
    /// 会议包内文件名(如 img001.png)
    pub image: String,
    /// 截图时刻(会议相对) "HH:MM:SS"
    pub ts: String,
    /// 相对会议开始毫秒
    pub rel_ms: i64,
    /// 截图时刻附近(±context_window)的发言拼接
    pub context: String,
}

fn default_db() -> PathBuf {
    std::env::var("APPDATA")
        .map(|p| PathBuf::from(p).join("pointtofix").join("gallery.db"))
        .unwrap_or_else(|_| PathBuf::from("gallery.db"))
}

fn default_root() -> PathBuf {
    std::env::var("APPDATA")
        .map(|p| PathBuf::from(p).join("pointtofix").join("gallery"))
        .unwrap_or_else(|_| PathBuf::from("gallery"))
}

#[derive(Debug, Clone)]
pub struct Shot {
    pub created_at: i64,
    /// gallery 根目录下的相对路径('/' 分隔)
    pub file_rel: String,
    pub ocr_text: String,
}

/// 只读连接(WAL 下安全,不干扰 pointtofixcraft 写入)
pub fn open_readonly(cfg: &ShotsCfg) -> Result<Connection> {
    let path = cfg.db_path.clone().unwrap_or_else(default_db);
    if !path.exists() {
        anyhow::bail!("gallery.db 不存在: {}", path.display());
    }
    Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("只读打开失败: {}", path.display()))
}

pub fn root(cfg: &ShotsCfg) -> PathBuf {
    cfg.root.clone().unwrap_or_else(default_root)
}

/// 取会议时段内的截图(只挑已存在的图片文件)
pub fn meeting_shots(conn: &Connection, cfg: &ShotsCfg, from_ms: i64, to_ms: i64) -> Result<Vec<Shot>> {
    let mut st = conn.prepare_cached(
        "SELECT created_at, file_path, ocr_text FROM shots
         WHERE created_at >= ?1 AND created_at <= ?2 ORDER BY created_at",
    )?;
    let rows = st.query_map(rusqlite::params![from_ms, to_ms], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let root = root(cfg);
    let mut out = Vec::new();
    for row in rows {
        let (created_at, file_rel, ocr_text) = row?;
        let abs = root.join(file_rel.replace('/', "\\"));
        if abs.exists() {
            out.push(Shot { created_at, file_rel, ocr_text });
        }
    }
    Ok(out)
}

/// 每张截图 → 上下文(±window 内发言拼接,截 200 字)
pub fn match_contexts(
    shots: &[Shot],
    segs: &[Segment],
    meeting_started_ms: i64,
    window_secs: f64,
) -> Vec<ShotLink> {
    let win_ms = (window_secs * 1000.0) as i64;
    let mut links = Vec::new();
    for (i, shot) in shots.iter().enumerate() {
        let rel = shot.created_at - meeting_started_ms;
        let (lo, hi) = (rel - win_ms, rel + win_ms);
        let mut parts: Vec<String> = Vec::new();
        let mut len = 0usize;
        for s in segs {
            if s.end_ms >= lo && s.start_ms <= hi {
                let who = s.speaker.clone().unwrap_or_else(|| "未知".into());
                let line = format!("{who}: {}", s.text);
                if len + line.chars().count() > 200 {
                    break;
                }
                len += line.chars().count();
                parts.push(line);
            }
        }
        let ext = Path::new(&shot.file_rel)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "png".into());
        links.push(ShotLink {
            image: format!("img{:03}.{ext}", i + 1),
            ts: crate::chunk::fmt_mmss(rel.max(0)),
            rel_ms: rel,
            context: parts.join("\n"),
        });
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Segment;

    #[test]
    fn context_window() {
        let seg = Segment {
            idx: 0,
            start_ms: 920_000,
            end_ms: 940_000,
            speaker: Some("Frank".into()),
            text: "这里有缩水风险,建议增加偷胶".into(),
        };
        let shot = Shot {
            created_at: 923_000,
            file_rel: "a/b.png".into(),
            ocr_text: String::new(),
        };
        let links = match_contexts(&[shot], &[seg], 0, 30.0);
        assert_eq!(links[0].image, "img001.png");
        assert!(links[0].context.contains("缩水"));
    }
}
