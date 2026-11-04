//! meetings.db:SQLite(WAL),时间一律 unix 毫秒 INTEGER。
//! 录音线程与后处理各自持有独立连接,WAL 允许并发。

use std::path::Path;

use anyhow::{Context, Result};
use chrono::{Local, TimeZone};
use rusqlite::Connection;

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// unix 毫秒 → 本地 "2026-11-04 15:30:12"
pub fn fmt_local(ms: i64) -> String {
    Local.timestamp_millis_opt(ms).unwrap().format("%F %T").to_string()
}

/// unix 毫秒 → 本地 "20261104"
pub fn fmt_date(ms: i64) -> String {
    Local.timestamp_millis_opt(ms).unwrap().format("%Y%m%d").to_string()
}

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path)
        .with_context(|| format!("打开数据库失败: {}", path.display()))?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA foreign_keys=ON;
         PRAGMA busy_timeout=5000;",
    )?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meetings (
           id           INTEGER PRIMARY KEY AUTOINCREMENT,
           title        TEXT NOT NULL DEFAULT '',
           started_at   INTEGER NOT NULL,
           ended_at     INTEGER,
           status       TEXT NOT NULL DEFAULT 'recording',
           audio_path   TEXT,
           package_dir  TEXT
         );

         CREATE TABLE IF NOT EXISTS segments (
           id         INTEGER PRIMARY KEY AUTOINCREMENT,
           meeting_id INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
           idx        INTEGER NOT NULL,
           start_ms   INTEGER NOT NULL,
           end_ms     INTEGER NOT NULL,
           speaker    TEXT,
           text       TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_segments_meeting ON segments(meeting_id, start_ms);

         CREATE TABLE IF NOT EXISTS speakers (
           meeting_id    INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
           speaker_key   TEXT NOT NULL,
           name          TEXT,
           proposed_name TEXT,
           similarity    REAL,
           PRIMARY KEY (meeting_id, speaker_key)
         );

         CREATE TABLE IF NOT EXISTS post_process (
           meeting_id       INTEGER PRIMARY KEY REFERENCES meetings(id) ON DELETE CASCADE,
           summary_json     TEXT,
           actions_json     TEXT,
           decisions_json   TEXT,
           risks_json       TEXT,
           screenshot_json  TEXT,
           llm_meta_json    TEXT,
           created_at       INTEGER
         );

         CREATE TABLE IF NOT EXISTS voiceprints (
           id         INTEGER PRIMARY KEY AUTOINCREMENT,
           name       TEXT NOT NULL UNIQUE,
           dim        INTEGER NOT NULL,
           centroid   BLOB NOT NULL,
           created_at INTEGER NOT NULL,
           updated_at INTEGER NOT NULL
         );

         CREATE TABLE IF NOT EXISTS voiceprint_samples (
           id           INTEGER PRIMARY KEY AUTOINCREMENT,
           voiceprint_id INTEGER NOT NULL REFERENCES voiceprints(id) ON DELETE CASCADE,
           embedding    BLOB NOT NULL,
           meeting_id   INTEGER,
           weight       REAL NOT NULL DEFAULT 1.0
         );
         CREATE INDEX IF NOT EXISTS idx_vp_samples ON voiceprint_samples(voiceprint_id);",
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Meeting {
    pub id: i64,
    pub title: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub status: String,
    pub audio_path: Option<String>,
    pub package_dir: Option<String>,
}

impl Meeting {
    pub fn duration_ms(&self) -> Option<i64> {
        self.ended_at.map(|e| e - self.started_at)
    }
}

pub fn insert_meeting(conn: &Connection, title: &str, started_at: i64) -> Result<i64> {
    conn.execute(
        "INSERT INTO meetings(title, started_at, status) VALUES (?1, ?2, 'recording')",
        rusqlite::params![title, started_at],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn finish_meeting(conn: &Connection, id: i64, ended_at: i64, audio_path: &str) -> Result<()> {
    conn.execute(
        "UPDATE meetings SET ended_at=?2, audio_path=?3, status='processing' WHERE id=?1",
        rusqlite::params![id, ended_at, audio_path],
    )?;
    Ok(())
}

pub fn set_status(conn: &Connection, id: i64, status: &str) -> Result<()> {
    conn.execute(
        "UPDATE meetings SET status=?2 WHERE id=?1",
        rusqlite::params![id, status],
    )?;
    Ok(())
}

pub fn set_package_dir(conn: &Connection, id: i64, dir: &str) -> Result<()> {
    conn.execute(
        "UPDATE meetings SET package_dir=?2 WHERE id=?1",
        rusqlite::params![id, dir],
    )?;
    Ok(())
}

pub fn get_meeting(conn: &Connection, id: i64) -> Result<Meeting> {
    conn.query_row(
        "SELECT id, title, started_at, ended_at, status, audio_path, package_dir
         FROM meetings WHERE id=?1",
        [id],
        |r| {
            Ok(Meeting {
                id: r.get(0)?,
                title: r.get(1)?,
                started_at: r.get(2)?,
                ended_at: r.get(3)?,
                status: r.get(4)?,
                audio_path: r.get(5)?,
                package_dir: r.get(6)?,
            })
        },
    )
    .context("会议不存在")
}

/// latest = 状态非 recording 的最新会议(后处理的对象)
pub fn latest_meeting(conn: &Connection) -> Result<Option<Meeting>> {
    let mut st = conn.prepare(
        "SELECT id, title, started_at, ended_at, status, audio_path, package_dir
         FROM meetings WHERE status != 'recording' ORDER BY started_at DESC LIMIT 1",
    )?;
    let mut rows = st.query([])?;
    Ok(rows.next()?.map(|r| Meeting {
        id: r.get(0).unwrap(),
        title: r.get(1).unwrap(),
        started_at: r.get(2).unwrap(),
        ended_at: r.get(3).unwrap(),
        status: r.get(4).unwrap(),
        audio_path: r.get(5).unwrap(),
        package_dir: r.get(6).unwrap(),
    }))
}

pub fn list_meetings(conn: &Connection) -> Result<Vec<Meeting>> {
    let mut st = conn.prepare(
        "SELECT id, title, started_at, ended_at, status, audio_path, package_dir
         FROM meetings ORDER BY started_at DESC",
    )?;
    let rows = st.query_map([], |r| {
        Ok(Meeting {
            id: r.get(0)?,
            title: r.get(1)?,
            started_at: r.get(2)?,
            ended_at: r.get(3)?,
            status: r.get(4)?,
            audio_path: r.get(5)?,
            package_dir: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn insert_segment(
    conn: &Connection,
    meeting_id: i64,
    idx: i64,
    start_ms: i64,
    end_ms: i64,
    speaker: Option<&str>,
    text: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO segments(meeting_id, idx, start_ms, end_ms, speaker, text)
         VALUES (?1,?2,?3,?4,?5,?6)",
        rusqlite::params![meeting_id, idx, start_ms, end_ms, speaker, text],
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Segment {
    pub idx: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker: Option<String>,
    pub text: String,
}

pub fn load_segments(conn: &Connection, meeting_id: i64) -> Result<Vec<Segment>> {
    let mut st = conn.prepare(
        "SELECT idx, start_ms, end_ms, speaker, text
         FROM segments WHERE meeting_id=?1 ORDER BY start_ms",
    )?;
    let rows = st.query_map([meeting_id], |r| {
        Ok(Segment {
            idx: r.get(0)?,
            start_ms: r.get(1)?,
            end_ms: r.get(2)?,
            speaker: r.get(3)?,
            text: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// 带显示名的段落(speaker 列 = 簇键;展示一律用这个:
/// 名字已命名 → 名字,未命名 → SPEAKER_xx 原键)
pub fn load_segments_named(conn: &Connection, meeting_id: i64) -> Result<Vec<Segment>> {
    let mut st = conn.prepare(
        "SELECT s.idx, s.start_ms, s.end_ms,
                COALESCE(sp.name, s.speaker) AS speaker, s.text
         FROM segments s
         LEFT JOIN speakers sp
           ON sp.meeting_id = s.meeting_id AND sp.speaker_key = s.speaker
         WHERE s.meeting_id=?1 ORDER BY s.start_ms",
    )?;
    let rows = st.query_map([meeting_id], |r| {
        Ok(Segment {
            idx: r.get(0)?,
            start_ms: r.get(1)?,
            end_ms: r.get(2)?,
            speaker: r.get(3)?,
            text: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn upsert_speaker(
    conn: &Connection,
    meeting_id: i64,
    key: &str,
    name: Option<&str>,
    proposed: Option<&str>,
    similarity: Option<f64>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO speakers(meeting_id, speaker_key, name, proposed_name, similarity)
         VALUES (?1,?2,?3,?4,?5)
         ON CONFLICT(meeting_id, speaker_key) DO UPDATE SET
           name=COALESCE(excluded.name, name),
           proposed_name=COALESCE(excluded.proposed_name, proposed_name),
           similarity=COALESCE(excluded.similarity, similarity)",
        rusqlite::params![meeting_id, key, name, proposed, similarity],
    )?;
    Ok(())
}

pub fn load_speakers(
    conn: &Connection,
    meeting_id: i64,
) -> Result<Vec<(String, Option<String>, Option<String>, Option<f64>)>> {
    let mut st = conn.prepare(
        "SELECT speaker_key, name, proposed_name, similarity
         FROM speakers WHERE meeting_id=?1 ORDER BY speaker_key",
    )?;
    let rows = st.query_map([meeting_id], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

