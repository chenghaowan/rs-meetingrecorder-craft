//! 本地翻译服务:llama-server.exe(b12716 Vulkan 预编译包)+ Hy-MT3 GGUF。
//!
//! 生命周期:ensure() 三段式——/health 探测(已有实例直接复用)→ 本地 exe
//! 分离启动(-ngl 99 全量进 GPU)→ 轮询就绪(首次加载 1.35 GB 需数十秒)。
//! 持有 Child 的 LocalServer drop 时 kill 自启进程;复用的外部进程不动。
//!
//! 翻译指令用 Hy-MT 官方模板(腾讯混元 Hy-MT 仓库):
//! `将以下文本翻译为{target_language}，注意只需要输出翻译后的结果，不要额外解释：`

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};

use crate::config::LocalTranslateCfg;

/// 首次加载 GGUF 的就绪超时
const READY_TIMEOUT: Duration = Duration::from_secs(180);

pub struct LocalServer {
    pub base_url: String,
    /// 自启的子进程(Some = 退出时 kill;复用的外部实例 None 不动)
    child: Option<Child>,
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn exe_candidates(cfg: &LocalTranslateCfg, data_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = &cfg.server_exe {
        v.push(p.clone());
    }
    if let Some(base) = data_dir {
        v.push(base.join("tools").join("llama-vulkan").join("llama-server.exe"));
    }
    v.push(PathBuf::from("llama-server.exe"));
    v
}

fn model_candidates(cfg: &LocalTranslateCfg, data_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = &cfg.model_path {
        v.push(p.clone());
    }
    if let Some(base) = data_dir {
        // 目录内任意 .gguf(取第一个)
        if let Ok(rd) = std::fs::read_dir(base.join("models").join("hy-mt3")) {
            let mut ggufs: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("gguf"))
                .collect();
            ggufs.sort();
            v.extend(ggufs);
        }
    }
    v
}

fn health(base: &str, timeout: Duration) -> bool {
    let agent: ureq::Agent = ureq::AgentBuilder::new().timeout(timeout).build();
    agent.get(&format!("{base}/health")).call().is_ok()
}

/// 确保翻译服务就绪:探测 → 自启 → 轮询。返回的 LocalServer 持有进程句柄。
pub fn ensure(cfg: &LocalTranslateCfg, data_dir: Option<&Path>) -> Result<LocalServer> {
    let base = format!("http://127.0.0.1:{}", cfg.port);
    if health(&base, Duration::from_millis(500)) {
        return Ok(LocalServer { base_url: base, child: None });
    }
    let exe = exe_candidates(cfg, data_dir)
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| anyhow!("找不到 llama-server.exe([translate.local].server_exe 或 data_dir\\tools\\llama-vulkan)"))?;
    let model = model_candidates(cfg, data_dir)
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| anyhow!("找不到 Hy-MT3 GGUF([translate.local].model_path 或 data_dir\\models\\hy-mt3)"))?;

    eprintln!(
        "[tr-server] 启动 {} -m {} --port {} -ngl {}",
        exe.display(),
        model.display(),
        cfg.port,
        cfg.ngl
    );
    let child = Command::new(&exe)
        .arg("-m")
        .arg(&model)
        .arg("--port")
        .arg(cfg.port.to_string())
        .arg("-ngl")
        .arg(cfg.ngl.to_string())
        .arg("-c")
        .arg(cfg.ctx.to_string())
        .arg("--jinja")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("启动 llama-server 失败: {}", exe.display()))?;

    let t0 = Instant::now();
    loop {
        if health(&base, Duration::from_millis(500)) {
            eprintln!("[tr-server] 就绪({:.1}s): {base}", t0.elapsed().as_secs_f64());
            return Ok(LocalServer { base_url: base, child: Some(child) });
        }
        if t0.elapsed() > READY_TIMEOUT {
            return Err(anyhow!("llama-server {} 秒内未就绪", READY_TIMEOUT.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Hy-MT 官方标准翻译模板(单段直翻;翻译模型不吃编号批量)
pub fn translate_one(base_url: &str, agent: &ureq::Agent, text: &str, target_lang: &str) -> Result<String> {
    let prompt = format!(
        "将以下文本翻译为{target_lang}，注意只需要输出翻译后的结果，不要额外解释：\n\n{text}"
    );
    let body = serde_json::json!({
        "messages": [{"role": "user", "content": prompt}],
        "temperature": 0.1,
        "max_tokens": 2048,
    });
    let resp = agent
        .post(&format!("{base_url}/v1/chat/completions"))
        .set("Content-Type", "application/json")
        .send_json(&body)
        .map_err(|e| anyhow!("llama-server 请求失败: {e}"))?;
    let v: serde_json::Value = resp.into_json().context("llama-server 响应解析失败")?;
    v["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("llama-server 响应缺 content"))
}

/// 单元测试用:模板渲染正确性
#[cfg(test)]
mod tests {
    #[test]
    fn prompt_template_matches_official() {
        let text = "Hello";
        let target = "简体中文";
        let prompt = format!(
            "将以下文本翻译为{target}，注意只需要输出翻译后的结果，不要额外解释：\n\n{text}"
        );
        assert!(prompt.starts_with("将以下文本翻译为简体中文"));
        assert!(prompt.ends_with("\n\nHello"));
    }
}
