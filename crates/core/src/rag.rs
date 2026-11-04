//! ym3dRagCraft 入库客户端:POST {base}/ingest,幂等按 meeting_id + 文档 id。
//!
//! 入库粒度:chunk 级(转写原文)+ 条目级(每条 decision/action/risk 独立向量)。

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::config::RagCfg;
use crate::shots::ShotLink;

pub struct RagClient {
    cfg: RagCfg,
    agent: ureq::Agent,
}

/// 组装入库文档
pub fn build_docs(
    meeting_id: i64,
    title: &str,
    chunks: &[crate::chunk::Chunk],
    summary: Option<&Value>,
    links: &[ShotLink],
) -> Vec<Value> {
    let mut docs: Vec<Value> = chunks
        .iter()
        .map(|c| {
            json!({
                "id": format!("m{meeting_id}_chunk_{:03}", c.id),
                "type": "transcript_chunk",
                "text": c.text,
                "metadata": {
                    "meeting_id": meeting_id,
                    "meeting_title": title,
                    "start_ms": c.start_ms,
                    "end_ms": c.end_ms,
                    "speakers": c.speakers,
                }
            })
        })
        .collect();

    if let Some(s) = summary {
        for (i, d) in s["decisions"].as_array().unwrap_or(&vec![]).iter().enumerate() {
            docs.push(json!({
                "id": format!("m{meeting_id}_decision_{i:02}"),
                "type": "decision",
                "text": format!("会议《{title}》决议: {}", d["item"]),
                "metadata": { "meeting_id": meeting_id, "meeting_title": title, "evidence_time": d["evidence_time"] }
            }));
        }
        for (i, a) in s["actions"].as_array().unwrap_or(&vec![]).iter().enumerate() {
            docs.push(json!({
                "id": format!("m{meeting_id}_action_{i:02}"),
                "type": "action",
                "text": format!("会议《{title}》行动项: {} — 负责人:{} 期限:{}", a["task"], a["owner"], a.get("due").and_then(Value::as_str).unwrap_or("")),
                "metadata": { "meeting_id": meeting_id, "meeting_title": title, "owner": a["owner"], "evidence_time": a.get("evidence_time").cloned().unwrap_or(Value::Null) }
            }));
        }
        for (i, r) in s["risks"].as_array().unwrap_or(&vec![]).iter().enumerate() {
            docs.push(json!({
                "id": format!("m{meeting_id}_risk_{i:02}"),
                "type": "risk",
                "text": format!("会议《{title}》风险: {}", r["risk"]),
                "metadata": { "meeting_id": meeting_id, "meeting_title": title, "evidence_time": r["evidence_time"] }
            }));
        }
        // 总结本体也入库(整会议摘要查询用)
        if !s["summary"].as_str().unwrap_or("").is_empty() {
            docs.push(json!({
                "id": format!("m{meeting_id}_summary"),
                "type": "meeting_summary",
                "text": format!("会议《{title}》总结: {}", s["summary"]),
                "metadata": { "meeting_id": meeting_id, "meeting_title": title }
            }));
        }
    }

    for (i, l) in links.iter().enumerate() {
        if l.context.is_empty() {
            continue;
        }
        docs.push(json!({
            "id": format!("m{meeting_id}_shot_{i:03}"),
            "type": "screenshot",
            "text": format!("[截图 @{}] {}", l.ts, l.context),
            "metadata": { "meeting_id": meeting_id, "meeting_title": title, "image": l.image, "ts": l.ts }
        }));
    }
    docs
}

impl RagClient {
    pub fn new(cfg: &RagCfg) -> Result<Self> {
        let Some(url) = &cfg.base_url else {
            bail!("RAG 未配置: config.toml [rag] base_url 留空即跳过入库");
        };
        if url.is_empty() {
            bail!("RAG 未配置");
        }
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(cfg.timeout_secs))
            .build();
        Ok(Self { cfg: cfg.clone(), agent })
    }

    pub fn ingest(&self, meeting_id: i64, title: &str, docs: &[Value]) -> Result<usize> {
        let url = format!("{}/ingest", self.cfg.base_url.as_deref().unwrap_or("").trim_end_matches('/'));
        let body = json!({
            "meeting_id": meeting_id,
            "meeting_title": title,
            "replace": true, // 幂等:同 meeting 重复入库先清后插(服务端语义)
            "docs": docs,
        });
        let resp = self
            .agent
            .post(&url)
            .send_json(&body)
            .map_err(|e| anyhow::anyhow!("POST {url} 失败: {e}"))?;
        let v: Value = resp.into_json().context("解析 RAG 响应失败")?;
        let n = v["ingested"].as_u64().unwrap_or(docs.len() as u64) as usize;
        Ok(n)
    }
}
