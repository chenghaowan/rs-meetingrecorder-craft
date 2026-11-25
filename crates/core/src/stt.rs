//! 本地 STT:SenseVoice-small(sherpa-rs,进程内 ONNX)。
//!
//! 模型由本项目自己管理(不再依赖外部推理服务):
//! - 目录:`[stt].model_dir` → `data_dir/models/sensevoice` → `exe旁/models/sensevoice`
//!   → `cwd/models/sensevoice`,需含 `model.int8.onnx` + `tokens.txt`
//! - 缺失时运行 `mrc-recorder --fetch-stt` 从 hf-mirror 下载(约 230 MB)
//!
//! 引擎随进程退出释放;识别在专用阻塞线程里跑,不占 UI 线程。

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::config::{Config, SttCfg};

const MODEL_FILE: &str = "model.int8.onnx";
const TOKENS_FILE: &str = "tokens.txt";
/// csukuangfj 的 sherpa-onnx 转换版 SenseVoice(中/英/日/韩/粤)
const HF_REPO: &str = "csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17";
const HF_MIRROR: &str = "https://hf-mirror.com";

/// 已落库确定的转写段(会议相对毫秒,由调用方加窗口起点)
#[derive(Debug, Clone)]
pub struct Segment {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

/// 进程内 SenseVoice 识别器
pub struct LocalAsr {
    recognizer: Mutex<sherpa_rs::sense_voice::SenseVoiceRecognizer>,
    pub model_dir: PathBuf,
}

impl LocalAsr {
    /// provider=none 返回 None;否则解析模型目录并加载(首次加载约数百毫秒)
    pub fn from_config(cfg: &Config) -> Result<Option<Self>> {
        let stt = &cfg.stt;
        if stt.provider.trim().eq_ignore_ascii_case("none") || stt.provider.trim().is_empty() {
            return Ok(None);
        }
        let dir = resolve_dir(cfg).ok_or_else(|| {
            anyhow::anyhow!(
                "未找到 SenseVoice 模型({MODEL_FILE} + {TOKENS_FILE});请运行 mrc-recorder --fetch-stt"
            )
        })?;
        Ok(Some(Self::new(&dir, &stt.language, stt.num_threads)?))
    }

    pub fn new(dir: &Path, language: &str, num_threads: u32) -> Result<Self> {
        let model = dir.join(MODEL_FILE);
        let tokens = dir.join(TOKENS_FILE);
        if !model.is_file() || !tokens.is_file() {
            bail!("模型目录不完整: {}", dir.display());
        }
        let language = if language.trim().is_empty() { "auto" } else { language.trim() };
        let recognizer = sherpa_rs::sense_voice::SenseVoiceRecognizer::new(
            sherpa_rs::sense_voice::SenseVoiceConfig {
                model: model.display().to_string(),
                tokens: tokens.display().to_string(),
                language: language.to_string(),
                use_itn: true,
                num_threads: Some(num_threads.max(1) as i32),
                ..Default::default()
            },
        )
        .map_err(|e| anyhow::anyhow!("SenseVoice 加载失败: {e}"))?;
        Ok(Self { recognizer: Mutex::new(recognizer), model_dir: dir.to_path_buf() })
    }

    /// 一段音频 → 文本。当前整窗作为一段返回(时间轴 0..dur),
    /// 由 transcribe::Streaming 的"已确定点"逻辑负责切分落库。
    pub fn transcribe(&self, sample_rate: u32, samples: &[f32]) -> Result<Vec<Segment>> {
        if samples.is_empty() {
            return Ok(vec![]);
        }
        let mut r = self.recognizer.lock().map_err(|_| anyhow::anyhow!("stt 锁中毒"))?;
        let out = r.transcribe(sample_rate, samples);
        let text = out.text.trim().to_string();
        if text.is_empty() {
            return Ok(vec![]);
        }
        let dur_ms = (samples.len() as f64 / sample_rate.max(1) as f64 * 1000.0) as i64;
        Ok(vec![Segment { start_ms: 0, end_ms: dur_ms, text }])
    }
}

/// 候选目录(按优先级)
pub fn candidates(cfg: &Config) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = cfg.stt.model_dir.clone() {
        v.push(p);
    }
    v.push(cfg.data_dir().join("models").join("sensevoice"));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("models").join("sensevoice"));
        }
    }
    v.push(PathBuf::from("models").join("sensevoice"));
    v
}

/// 找到含完整模型文件的目录
pub fn resolve_dir(cfg: &Config) -> Option<PathBuf> {
    candidates(cfg).into_iter().find(|d| {
        d.join(MODEL_FILE).is_file() && d.join(TOKENS_FILE).is_file()
    })
}

/// 下载模型到 data_dir/models/sensevoice(可重复执行,已存在的文件跳过)
pub fn install_model(cfg: &Config) -> Result<PathBuf> {
    let dir = cfg.data_dir().join("models").join("sensevoice");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("创建模型目录失败: {}", dir.display()))?;
    for file in [MODEL_FILE, TOKENS_FILE] {
        let dest = dir.join(file);
        if dest.is_file() && dest.metadata().map(|m| m.len()).unwrap_or(0) > 0 {
            eprintln!("[stt] 已存在,跳过: {}", dest.display());
            continue;
        }
        let url = format!("{HF_MIRROR}/{HF_REPO}/resolve/main/{file}");
        download(&url, &dest)?;
    }
    eprintln!("[stt] SenseVoice 模型就绪: {}", dir.display());
    Ok(dir)
}

fn download(url: &str, dest: &Path) -> Result<()> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(120))
        .build();
    eprintln!("[stt] 下载 {url}");
    let resp = agent.get(url).call().with_context(|| format!("下载失败: {url}"))?;
    let total: u64 = resp
        .header("content-length")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let part = dest.with_extension("part");
    let mut file = File::create(&part)
        .with_context(|| format!("创建临时文件失败: {}", part.display()))?;
    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; 1 << 20];
    let mut received: u64 = 0;
    let mut next_report: u64 = 16 << 20;
    loop {
        let n = reader.read(&mut buf).context("读取响应失败")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).context("写入模型文件失败")?;
        received += n as u64;
        if received >= next_report {
            let pct = if total > 0 { format!("{:.0}%", received as f64 * 100.0 / total as f64) } else { "…".into() };
            eprintln!("[stt] {pct} ({:.1} MiB)", received as f64 / 1048576.0);
            next_report = received + (16 << 20);
        }
    }
    file.flush().ok();
    if total > 0 && received != total {
        let _ = std::fs::remove_file(&part);
        bail!("下载不完整:{received} / {total} 字节");
    }
    std::fs::rename(&part, dest)
        .with_context(|| format!("落盘失败: {}", dest.display()))?;
    Ok(())
}

/// 供 UI/CLI 显示:当前生效目录或"未就绪"
pub fn status(cfg: &Config) -> Result<String, String> {
    match resolve_dir(cfg) {
        Some(d) => Ok(d.display().to_string()),
        None => Err("未找到模型(运行 mrc-recorder --fetch-stt 下载)".into()),
    }
}

/// 便捷:provider 是否启用
pub fn enabled(cfg: &SttCfg) -> bool {
    !cfg.provider.trim().eq_ignore_ascii_case("none") && !cfg.provider.trim().is_empty()
}
