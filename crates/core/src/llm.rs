//! LLM 总结客户端(ureq 同步),双 provider:
//! - "api":OpenAI 兼容端点(Bearer api_key)
//! - "local":llama-server + 本地 GGUF(复用 translate_server 的生命周期管理,离线)
//!
//! 目标:会后一次性结构化总结。response_format 优先 json_schema,
//! 端点不支持(json_schema=false 配置)则退化为 json_object+提示词强约束。
//! 超上下文时由 pipeline 走"两段总结→一次合并"降级,这里只管单次调用。

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::{Config, LlmCfg, LocalTranslateCfg};
use crate::translate_server::{self, LocalServer};

/// 8B 档:Qwen3-8B(中文总结质量好,Q4_K_M 约 4.7 GB;官方仓,hf-mirror 国内可达)
pub const RECOMMENDED_MODEL_FILE: &str = "Qwen3-8B-Q4_K_M.gguf";
pub const RECOMMENDED_MODEL_URL: &str =
    "https://hf-mirror.com/Qwen/Qwen3-8B-GGUF/resolve/main/Qwen3-8B-Q4_K_M.gguf";
pub const RECOMMENDED_MODEL_PAGE: &str = "https://hf-mirror.com/Qwen/Qwen3-8B-GGUF";
/// 4B 档:Qwen3-4B-Instruct-2507(非思考型,Q4_K_M 约 2.3 GB;官方无 GGUF,unsloth 转换)
pub const SMALL_MODEL_FILE: &str = "Qwen3-4B-Instruct-2507-Q4_K_M.gguf";
pub const SMALL_MODEL_URL: &str =
    "https://hf-mirror.com/unsloth/Qwen3-4B-Instruct-2507-GGUF/resolve/main/Qwen3-4B-Instruct-2507-Q4_K_M.gguf";
/// 轻量档:Qwen3-1.7B(核显/CPU,Q4_K_M 约 1.0 GB;unsloth)
pub const TINY_MODEL_FILE: &str = "Qwen3-1.7B-Q4_K_M.gguf";
pub const TINY_MODEL_URL: &str =
    "https://hf-mirror.com/unsloth/Qwen3-1.7B-GGUF/resolve/main/Qwen3-1.7B-Q4_K_M.gguf";
/// llama-server.exe 预编译包(Vulkan)发布页,与字幕翻译共用同一个包
pub const LLAMA_CPP_RELEASES: &str = "https://github.com/ggml-org/llama.cpp/releases";

/// 一档推荐(按独显显存分档;ctx 与显存一起给,KV(f16) 按 144KB/token 估算)
pub struct ModelRec {
    pub file: &'static str,
    pub url: &'static str,
    /// 语言中立的模型名+体积(设置界面直接显示)
    pub label: &'static str,
    /// 推荐上下文窗口(写入 [llm.local].ctx,同时是两段降级阈值)
    pub ctx: u32,
}

/// 分档:>=11GB→8B@32k | >=7.5GB→8B@16k(8GB 卡:4.7+KV2.3≈7GB)
///       | >=5GB→4B@16k(6GB 卡:2.3+2.3≈4.6GB) | 其余(核显/4GB 卡)→1.7B@16k(≈2.9GB)
pub fn recommend_model(dedicated_vram_mb: u64) -> ModelRec {
    match dedicated_vram_mb {
        v if v >= 11_000 => ModelRec {
            file: RECOMMENDED_MODEL_FILE,
            url: RECOMMENDED_MODEL_URL,
            label: "Qwen3-8B Q4_K_M · 4.7 GB",
            ctx: 32768,
        },
        v if v >= 7_500 => ModelRec {
            file: RECOMMENDED_MODEL_FILE,
            url: RECOMMENDED_MODEL_URL,
            label: "Qwen3-8B Q4_K_M · 4.7 GB",
            ctx: 16384,
        },
        v if v >= 5_000 => ModelRec {
            file: SMALL_MODEL_FILE,
            url: SMALL_MODEL_URL,
            label: "Qwen3-4B-Instruct-2507 · 2.3 GB",
            ctx: 16384,
        },
        _ => ModelRec {
            file: TINY_MODEL_FILE,
            url: TINY_MODEL_URL,
            label: "Qwen3-1.7B · 1.0 GB",
            ctx: 16384,
        },
    }
}

/// 本机推荐:按 DXGI 独显显存分档(无独显 → 轻量档)
pub fn recommend_for_machine() -> ModelRec {
    let vram = crate::gpu::discrete().map(|g| g.dedicated_mb).unwrap_or(0);
    recommend_model(vram)
}

/// 本地总结模型目录(data_dir\models\llm)
pub fn models_dir(cfg: &Config) -> PathBuf {
    cfg.data_dir().join("models").join("llm")
}

/// 下载指定档位模型到 data_dir\models\llm\(可重复执行,已存在跳过)
pub fn download_model_file(file: &str, url: &str, cfg: &Config) -> Result<PathBuf> {
    let dir = models_dir(cfg);
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(file);
    if dest.is_file() && dest.metadata().map(|m| m.len()).unwrap_or(0) > 0 {
        return Ok(dest);
    }
    crate::stt::download(url, &dest)?;
    Ok(dest)
}

/// 下载本机推荐模型(设置"下载推荐模型"与外部调用入口)
pub fn install_model(cfg: &Config) -> Result<PathBuf> {
    let rec = recommend_for_machine();
    download_model_file(rec.file, rec.url, cfg)
}

pub struct LlmClient {
    cfg: LlmCfg,
    /// 实际请求端点:api=cfg.base_url;local=http://127.0.0.1:{port}
    base: String,
    /// local provider 的服务进程句柄(drop 即 kill 自启进程;复用外部实例 None)
    _server: Option<LocalServer>,
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
    /// provider="local" 时拉起 llama-server(失败报错);provider="api" 要求 api_key
    pub fn new(cfg: &LlmCfg, data_dir: Option<&std::path::Path>) -> Result<Self> {
        let prov = cfg.provider.trim();
        let (base, server) = match prov {
            "local" => {
                let tl = LocalTranslateCfg {
                    server_exe: cfg.local.server_exe.clone(),
                    model_path: cfg.local.model_path.clone(),
                    port: cfg.local.port,
                    ngl: cfg.local.ngl,
                    ctx: cfg.local.ctx,
                };
                let server = translate_server::ensure(&tl, data_dir)?;
                (server.base_url.clone(), Some(server))
            }
            "" | "api" => {
                if cfg.api_key.is_empty() {
                    bail!("LLM 未配置: 请在 config.toml [llm] 填 api_key,或 provider=\"local\" 用本地模型");
                }
                (cfg.base_url.clone(), None)
            }
            other => bail!("未知 LLM provider: {other}(api/local)"),
        };
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(cfg.timeout_secs))
            .build();
        Ok(Self { cfg: cfg.clone(), base, _server: server, agent })
    }

    pub fn cfg(&self) -> &LlmCfg {
        &self.cfg
    }

    /// llm_meta 用:实际生效的 provider
    pub fn provider(&self) -> &'static str {
        if self._server.is_some() { "local" } else { "api" }
    }

    /// 发一次 chat 请求并要求返回 JSON 对象(已做 ``` 剥壳)。
    pub fn chat_json(
        &self,
        system: &str,
        user: &str,
        schema: Option<&Value>,
    ) -> Result<Value> {
        let url = format!("{}/chat/completions", self.base.trim_end_matches('/'));

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
            // 本地 Qwen3 是思考型模型:关掉思考,总结直接出 JSON(模板不认识该参数时被忽略)
            let mut body = json!({
                "model": self.cfg.model,
                "temperature": self.cfg.temperature,
                "messages": [
                    ChatMessage { role: "system", content: system },
                    ChatMessage { role: "user", content: user }
                ],
                "response_format": response_format,
            });
            if self._server.is_some() {
                body["chat_template_kwargs"] = json!({ "enable_thinking": false });
            }

            let mut req = self.agent.post(&url);
            if !self.cfg.api_key.is_empty() {
                req = req.set("Authorization", &format!("Bearer {}", self.cfg.api_key));
            }
            let resp = req.send_json(&body);

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

    #[test]
    fn recommend_tiers_by_vram() {
        // 核显/无独显/4GB 卡 → 1.7B;6GB 独显 → 4B;8GB → 8B@16k;12GB → 8B@32k
        let tier = |mb| recommend_model(mb);
        assert_eq!(tier(0).file, TINY_MODEL_FILE);
        assert_eq!(tier(4096).file, TINY_MODEL_FILE);
        assert_eq!(tier(6144).file, SMALL_MODEL_FILE);
        assert_eq!(tier(8192).file, RECOMMENDED_MODEL_FILE);
        assert_eq!(tier(8192).ctx, 16384);
        assert_eq!(tier(12288).file, RECOMMENDED_MODEL_FILE);
        assert_eq!(tier(12288).ctx, 32768);
    }
}
