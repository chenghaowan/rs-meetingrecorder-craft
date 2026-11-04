//! 转写切块:5 分钟时间窗 / 1000 token 上限,边界对齐句子(不切断一句话)。

use crate::config::PostCfg;
use crate::db::Segment;
use crate::tokens::approx_tokens;

#[derive(Debug, Clone)]
pub struct Chunk {
    pub id: usize,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    pub speakers: Vec<String>,
}

pub fn chunk_segments(segs: &[Segment], cfg: &PostCfg) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut cur: Vec<&Segment> = Vec::new();
    let mut cur_tokens = 0usize;

    let flush = |cur: &mut Vec<&Segment>, chunks: &mut Vec<Chunk>| {
        if cur.is_empty() {
            return;
        }
        let start_ms = cur.first().unwrap().start_ms;
        let end_ms = cur.last().unwrap().end_ms;
        let mut speakers: Vec<String> = cur
            .iter()
            .filter_map(|s| s.speaker.clone())
            .collect();
        speakers.sort();
        speakers.dedup();
        let text = cur
            .iter()
            .map(|s| {
                let who = s.speaker.clone().unwrap_or_else(|| "未知".into());
                let mmss = fmt_mmss(s.start_ms);
                format!("[{mmss}] {who}: {}", s.text)
            })
            .collect::<Vec<_>>()
            .join("\n");
        chunks.push(Chunk {
            id: chunks.len(),
            start_ms,
            end_ms,
            text,
            speakers,
        });
        cur.clear();
    };

    for s in segs {
        cur.push(s);
        cur_tokens += approx_tokens(&s.text);
        let span = (s.end_ms - cur.first().unwrap().start_ms) as f64 / 1000.0;
        // token 超限或时间窗超限 → 在句边界切
        if cur_tokens >= cfg.chunk_token_limit || span >= cfg.chunk_window_secs {
            flush(&mut cur, &mut chunks);
            cur_tokens = 0;
        }
    }
    flush(&mut cur, &mut chunks);
    chunks
}

/// 会议相对毫秒 → "HH:MM:SS"
pub fn fmt_mmss(rel_ms: i64) -> String {
    let total = rel_ms / 1000;
    format!("{:02}:{:02}:{:02}", total / 3600, (total % 3600) / 60, total % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: i64, text: &str) -> Segment {
        Segment {
            idx: 0,
            start_ms: start,
            end_ms: start + 1000,
            speaker: Some("SPEAKER_00".into()),
            text: text.into(),
        }
    }

    #[test]
    fn splits_by_window() {
        let cfg = PostCfg {
            chunk_window_secs: 300.0,
            chunk_token_limit: 1000,
            ..Default::default()
        };
        let segs: Vec<Segment> = (0..400)
            .map(|i| seg(i * 1000, "这是一句话测试"))
            .collect();
        let chunks = chunk_segments(&segs, &cfg);
        assert!(chunks.len() >= 2);
        // 400s 的内容,5 分钟窗 → 1 块
        let segs: Vec<Segment> = (0..100).map(|i| seg(i * 1000, "短句")).collect();
        assert_eq!(chunk_segments(&segs, &cfg).len(), 1);
    }

    #[test]
    fn splits_by_tokens() {
        let cfg = PostCfg {
            chunk_window_secs: 1e9,
            chunk_token_limit: 10,
            ..Default::default()
        };
        let segs: Vec<Segment> = (0..30).map(|i| seg(i, "这句有十个字整")).collect();
        let chunks = chunk_segments(&segs, &cfg);
        assert!(chunks.len() >= 3);
    }
}
