//! OpenAI 兼容 LLM 客户端(ureq 同步)。
//!
//! 目标:会后一次性结构化总结。response_format 优先 json_schema,
//! 端点不支持(json_schema=false 配置)则退化为 json_object+提示词强约束。
//! 超上下文时由 pipeline 走"两段总结→一次合并"降级,这里只管单次调用。

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::LlmCfg;

pub struct LlmClient {
    cfg: LlmCfg,
    agent: ureq::Agent,
}

#[derive(Debug, Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMsg,
}

#[derive(Debug, Deserialize)]
struct ChatMsg {
    content: Option<String>,
}

impl LlmClient {
    pub fn new(cfg: &LlmCfg) -> Result<Self> {
        if cfg.api_key.is_empty() {
            bail!("LLM 未配置: 请在 config.toml [llm] 填 api_key");
        }
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(cfg.timeout_secs))
            .build();
        Ok(Self { cfg: cfg.clone(), agent })
    }

    pub fn cfg(&self) -> &LlmCfg {
        &self.cfg
    }

    /// 发一次 chat 请求并要求返回 JSON 对象(已做 ``` 剥壳)。
    pub fn chat_json(
        &self,
        system: &str,
        user: &str,
        schema: Option<&Value>,
    ) -> Result<Value> {
        let url = format!("{}/chat/completions", self.cfg.base_url.trim_end_matches('/'));

        let mut response_format = if self.cfg.json_schema {
            schema.map(|s| {
                json!({
                    "type": "json_schema",
                    "json_schema": { "name": "meeting_summary", "strict": false, "schema": s }
                })
            })
        } else {
            None
        }
        .unwrap_or_else(|| json!({ "type": "json_object" }));

        // 部分端点(ollama 老版)连 json_object 都不认 → 由重试兜底降级
        let mut attempt_strip_rf = false;
        let mut last_err: Option<String> = None;

        for attempt in 0..=self.cfg.max_retries {
            let body = json!({
                "model": self.cfg.model,
                "temperature": self.cfg.temperature,
                "messages": [
                    ChatMessage { role: "system", content: system },
                    ChatMessage { role: "user", content: user }
                ],
                "response_format": response_format,
            });

            let resp = self
                .agent
                .post(&url)
                .set("Authorization", &format!("Bearer {}", self.cfg.api_key))
                .send_json(&body);

            match resp {
                Ok(r) => {
                    let parsed: ChatResponse = r.into_json().context("解析 LLM 响应失败")?;
                    let content = parsed
                        .choices
                        .first()
                        .and_then(|c| c.message.content.clone())
                        .unwrap_or_default();
                    return extract_json(&content).map_err(|e| {
                        anyhow!("LLM 返回非 JSON(第{}次): {e}", attempt + 1)
                    });
                }
                Err(ureq::Error::Status(code, r)) => {
                    let text = r.into_string().unwrap_or_default();
                    // 4xx 且像是不认识 response_format:降级重试
                    if !attempt_strip_rf
                        && (code == 400 || code == 422)
                        && (text.contains("response_format")
                            || text.contains("json_schema")
                            || text.contains("json_object"))
                    {
                        attempt_strip_rf = true;
                        response_format = Value::Null;
                        continue;
                    }
                    last_err = Some(format!("HTTP {code}: {}", truncate(&text, 500)));
                }
                Err(e) => {
                    last_err = Some(e.to_string());
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1500 * (attempt as u64 + 1)));
        }
        bail!("LLM 请求失败: {}", last_err.unwrap_or_default())
    }
}

/// 剥 ```json 围栏、找首个平衡 JSON 对象
pub fn extract_json(content: &str) -> Result<Value> {
    let t = content.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .unwrap_or(t)
        .trim();
    let t = t.strip_suffix("```").unwrap_or(t).trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return Ok(v);
    }
    // 找第一个 '{' 起的平衡括号
    let bytes = t.as_bytes();
    if let Some(start) = t.find('{') {
        let mut depth = 0i32;
        let mut in_str = false;
        let mut esc = false;
        for (i, b) in bytes.iter().enumerate().skip(start) {
            let c = *b as char;
            if esc {
                esc = false;
                continue;
            }
            match c {
                '\\' if in_str => esc = true,
                '"' => in_str = !in_str,
                '{' if !in_str => depth += 1,
                '}' if !in_str => {
                    depth -= 1;
                    if depth == 0 {
                        return serde_json::from_str(&t[start..=i])
                            .context("JSON 平衡段解析失败");
                    }
                }
                _ => {}
            }
        }
    }
    bail!("没有找到 JSON 对象: {}", truncate(t, 200))
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// 总结用的 JSON Schema(供 json_schema 模式;字段与会议包 summary.json 一致)
pub fn summary_schema() -> Value {
    json!({
        "type": "object",
        "required": ["summary", "decisions", "actions", "risks"],
        "properties": {
            "summary": { "type": "string" },
            "decisions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["item", "evidence_time"],
                    "properties": {
                        "item": { "type": "string" },
                        "evidence_time": { "type": "string" }
                    }
                }
            },
            "actions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["owner", "task"],
                    "properties": {
                        "owner": { "type": "string" },
                        "task": { "type": "string" },
                        "due": { "type": "string" },
                        "evidence_time": { "type": "string" }
                    }
                }
            },
            "risks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["risk"],
                    "properties": {
                        "risk": { "type": "string" },
                        "evidence_time": { "type": "string" }
                    }
                }
            }
        }
    })
}

pub const SUMMARY_SYSTEM: &str = r#"你是工业/产品开发会议的记录整理专家。输入是带时间戳和说话人的会议转写(可能附带截图上下文)。输出严格的 JSON,不要输出任何其他文字:
{
 "summary": "整场会议的中文总结,300-600字,按议题分段叙述",
 "decisions": [{"item": "决议内容", "evidence_time": "HH:MM:SS"}],
 "actions":   [{"owner": "执行人", "task": "任务内容", "due": "期限或空字符串", "evidence_time": "HH:MM:SS"}],
 "risks":     [{"risk": "风险描述", "evidence_time": "HH:MM:SS"}]
}
要求:
- evidence_time 必须来自转写中真实出现的时间戳,引用支持该条目的原话位置
- 只提取明确说出的决议/行动/风险,不要推测;没有则输出空数组
- owner 用转写中出现的名字,不要编造
- 行动项写清做什么+产出物(如"更新DFM并发出")"#;

pub const MERGE_SYSTEM: &str = r#"你是会议记录整理专家。输入是同一场会议按前后两半分别生成的两份 JSON 总结。把它们合并为一份,去重(语义相同的条目保留信息更全的),按原文时间排序,输出与前两份完全相同的 JSON 结构,不要输出任何其他文字。"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fenced_json() {
        let v = extract_json("好的,以下是结果:\n```json\n{\"a\":1}\n```").unwrap();
        assert_eq!(v["a"], 1);
        let v = extract_json("前缀 {\"b\":{\"c\":2}} 后缀").unwrap();
        assert_eq!(v["b"]["c"], 2);
    }
}
