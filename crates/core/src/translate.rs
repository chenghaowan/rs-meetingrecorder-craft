//! 字幕实时翻译,三 provider:
//! - "local":llama-server + Hy-MT3(本机 Vulkan GPU,官方模板逐段直翻,离线)
//! - "llm":复用 [llm] 的 OpenAI 兼容端点。编号批量翻译,一次调用翻 3~5 段,术语保留。
//! - "baidu":移植 ltl-manage utils/translator.py 的百度翻译开放 API
//!   (appid+secret MD5 签名,兼容经典/AI 两种响应格式,进程内缓存)。
//!
//! 源语言一律自动识别:Hy-MT3 官方模板只给目标语言;LLM 提示词让模型自己认源语言;
//! 百度 API from=auto。只有目标语言 [translate].to_lang 需要配置。
//!
//! 各 provider 共用一层文本缓存(键 = provider|from|to|text)。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;

use crate::config::{LlmCfg, TranslateCfg};
use crate::translate_server::{self, LocalServer};

const CACHE_MAX: usize = 2000;

pub struct Translator {
    cfg: TranslateCfg,
    llm: Option<LlmCfg>,
    /// local provider 的服务进程句柄(drop 即 kill 自启进程)
    server: Option<LocalServer>,
    agent: ureq::Agent,
    cache: Mutex<HashMap<String, String>>,
}

impl Translator {
    pub fn new(cfg: &TranslateCfg, llm: &LlmCfg, data_dir: Option<&Path>) -> Result<Self> {
        let mut server = None;
        match cfg.provider.as_str() {
            "none" | "" => bail!("翻译未启用"),
            "local" => {
                // 就绪即持有句柄;失败即禁用(调用方降级)
                server = Some(translate_server::ensure(&cfg.local, data_dir)?);
            }
            "llm" => {
                if llm.api_key.is_empty() {
                    bail!("provider=llm 但 [llm] api_key 未配置");
                }
            }
            "baidu" => {
                if cfg.appid.is_empty() || cfg.secret.is_empty() {
                    bail!("provider=baidu 但 [translate] appid/secret 未配置");
                }
            }
            other => bail!("未知翻译 provider: {other}(local/llm/baidu/none)"),
        }
        Ok(Self {
            cfg: cfg.clone(),
            llm: Some(llm.clone()),
            server,
            agent: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(30))
                .build(),
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn enabled(&self) -> bool {
        !self.cfg.provider.is_empty() && self.cfg.provider != "none"
    }

    /// 批量翻译:返回顺序与输入一一对应;单条失败为 None(不阻塞整批)。
    pub fn translate_batch(&self, texts: &[String]) -> Vec<Option<String>> {
        if !self.enabled() || texts.is_empty() {
            return vec![None; texts.len()];
        }
        // 1) 缓存命中
        let mut out: Vec<Option<String>> = vec![None; texts.len()];
        let mut pending: Vec<usize> = Vec::new();
        {
            let cache = self.cache.lock().expect("翻译缓存锁");
            for (i, t) in texts.iter().enumerate() {
                if t.trim().is_empty() {
                    continue;
                }
                if let Some(hit) = cache.get(&self.cache_key(t)) {
                    out[i] = Some(hit.clone());
                } else {
                    pending.push(i);
                }
            }
        }
        if pending.is_empty() {
            return out;
        }

        // 2) 未命中的按 provider 翻
        let res = match self.cfg.provider.as_str() {
            "local" => self.translate_batch_local(texts, &pending),
            "baidu" => self.translate_batch_baidu(texts, &pending),
            _ => self.translate_batch_llm(texts, &pending),
        };
        if let Ok(values) = res {
            {
                let mut cache = self.cache.lock().expect("翻译缓存锁");
                if cache.len() > CACHE_MAX {
                    cache.clear();
                }
                for (i, v) in pending.iter().zip(&values) {
                    if let Some(v) = v {
                        cache.insert(self.cache_key(&texts[*i]), v.clone());
                    }
                }
            }
            for (i, v) in pending.into_iter().zip(values) {
                out[i] = v;
            }
        }
        out
    }

    fn cache_key(&self, text: &str) -> String {
        format!("{}|{}|{}|{}", self.cfg.provider, self.cfg.from_lang, self.cfg.to_lang, text)
    }

    // ---------- local provider:Hy-MT3 逐段直翻(官方模板) ----------

    fn translate_batch_local(
        &self,
        texts: &[String],
        idxs: &[usize],
    ) -> Result<Vec<Option<String>>> {
        let server = self
            .server
            .as_ref()
            .ok_or_else(|| anyhow!("local 翻译服务未就绪"))?;
        let target = lang_name(&self.cfg.to_lang);
        let mut values: Vec<Option<String>> = vec![None; idxs.len()];
        for (k, i) in idxs.iter().enumerate() {
            match translate_server::translate_one(&server.base_url, &self.agent, &texts[*i], target) {
                Ok(v) => values[k] = Some(v),
                Err(e) => {
                    eprintln!("[translate] local 失败: {e}");
                    break; // 连续失败放弃本批,下个 tick 再试
                }
            }
        }
        Ok(values)
    }

    // ---------- llm provider:编号批量,一次调用 ----------

    fn translate_batch_llm(
        &self,
        texts: &[String],
        idxs: &[usize],
    ) -> Result<Vec<Option<String>>> {
        let llm = self.llm.as_ref().ok_or_else(|| anyhow!("llm 配置缺失"))?;
        // 按批大小切块,每块一次调用
        let mut values: Vec<Option<String>> = vec![None; idxs.len()];
        for chunk in idxs.chunks(self.cfg.batch_size.max(1)) {
            let numbered: String = chunk
                .iter()
                .enumerate()
                .map(|(k, i)| format!("{}. {}", k + 1, texts[*i].replace('\n', " ")))
                .collect::<Vec<_>>()
                .join("\n");
            let system = format!(
                "你是专业翻译。自动识别每行源语言,把用户给出的编号行逐行翻译成{}。工业/工程术语(如 DFM、CAE、缩水、偷胶、拔模)按行业惯例翻译或保留。只输出相同编号的译文,格式为 \"N. 译文\",不要任何解释。",
                lang_name(&self.cfg.to_lang)
            );
            match chat_text(llm, &self.agent, &system, &numbered) {
                Ok(reply) => {
                    for (k, &si) in chunk.iter().enumerate() {
                        let _ = si;
                        let want = format!("{}.", k + 1);
                        for line in reply.lines() {
                            let t = line.trim();
                            if t.starts_with(&want) {
                                let v = t[want.len()..].trim().to_string();
                                if !v.is_empty() {
                                    values[k] = Some(v);
                                }
                                break;
                            }
                        }
                    }
                }
                Err(e) => eprintln!("[translate] llm 批次失败: {e}"),
            }
        }
        Ok(values)
    }

    // ---------- baidu provider:移植 ltl-manage translator.py ----------

    fn translate_batch_baidu(
        &self,
        texts: &[String],
        idxs: &[usize],
    ) -> Result<Vec<Option<String>>> {
        let mut values: Vec<Option<String>> = vec![None; idxs.len()];
        for (k, i) in idxs.iter().enumerate() {
            match self.baidu_one(&texts[*i]) {
                Ok(v) => values[k] = Some(v),
                Err(e) => {
                    eprintln!("[translate] baidu 失败: {e}");
                    break; // 连续失败直接放弃本批,下个 tick 再试
                }
            }
            // 百度免费版 QPS=1,段间稍作间隔
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }
        Ok(values)
    }

    fn baidu_one(&self, text: &str) -> Result<String> {
        let salt = chrono::Utc::now().timestamp_millis().to_string();
        let sign = format!("{:x}", md5::compute(format!("{}{}{}{}", self.cfg.appid, text, salt, self.cfg.secret)));
        let url = if self.cfg.api_url.is_empty() {
            "https://fanyi-api.baidu.com/api/trans/vip/translate".to_string()
        } else {
            self.cfg.api_url.clone()
        };
        let resp = self
            .agent
            .get(&url)
            .query("q", text)
            .query("from", &self.cfg.from_lang)
            .query("to", &self.cfg.to_lang)
            .query("appid", &self.cfg.appid)
            .query("salt", &salt)
            .query("sign", &sign)
            .call()
            .map_err(|e| anyhow!("请求失败: {e}"))?;
        let data: serde_json::Value = resp.into_json().context("百度响应解析失败")?;
        // 经典接口: {"trans_result":[{"dst":...}]}
        if let Some(dst) = data["trans_result"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|o| o["dst"].as_str())
        {
            return Ok(dst.to_string());
        }
        // AI 文本翻译接口: {"error_code":"0","data":{"result":...}} / {"data":"..."}
        let code = data["error_code"].as_str().unwrap_or("");
        if code.is_empty() || code == "0" || code == "52000" {
            if let Some(r) = data["data"]["result"].as_str() {
                return Ok(r.to_string());
            }
            if let Some(r) = data["data"].as_str() {
                return Ok(r.to_string());
            }
        }
        bail!(
            "百度翻译失败(code={}): {}",
            if code.is_empty() { "unknown" } else { code },
            data["error_msg"].as_str().unwrap_or("?")
        )
    }
}

/// 简单的同步 chat 调用,返回纯文本(不走 json_schema;给翻译用)
pub fn chat_text(llm: &LlmCfg, agent: &ureq::Agent, system: &str, user: &str) -> Result<String> {
    let url = format!("{}/chat/completions", llm.base_url.trim_end_matches('/'));
    let body = json!({
        "model": llm.model,
        "temperature": 0.1,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ]
    });
    let resp = agent
        .post(&url)
        .set("Authorization", &format!("Bearer {}", llm.api_key))
        .send_json(&body)
        .map_err(|e| anyhow!("LLM 请求失败: {e}"))?;
    let v: serde_json::Value = resp.into_json().context("LLM 响应解析失败")?;
    v["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| anyhow!("LLM 响应缺 content"))
}

fn lang_name(code: &str) -> &'static str {
    match code {
        "zh" => "简体中文",
        "en" => "英文",
        "de" => "德文",
        "ja" => "日文",
        _ => "英文",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TranslateCfg;

    #[test]
    fn rejects_missing_config() {
        assert!(Translator::new(
            &TranslateCfg {
                provider: "none".into(),
                ..Default::default()
            },
            &Default::default(),
            None,
        )
        .is_err());
        assert!(Translator::new(
            &TranslateCfg {
                provider: "baidu".into(), // 缺 appid/secret
                ..Default::default()
            },
            &Default::default(),
            None,
        )
        .is_err());
        assert!(Translator::new(
            &TranslateCfg {
                provider: "llm".into(), // [llm] 无 key
                ..Default::default()
            },
            &Default::default(),
            None,
        )
        .is_err());
    }

    #[test]
    fn cache_key_includes_langs() {
        let cfg = TranslateCfg {
            provider: "baidu".into(),
            from_lang: "zh".into(),
            to_lang: "en".into(),
            appid: "a".into(),
            secret: "s".into(),
            ..Default::default()
        };
        let t = Translator::new(&cfg, &Default::default(), None).unwrap();
        assert!(t.cache_key("你好").starts_with("baidu|zh|en|"));
    }
}
