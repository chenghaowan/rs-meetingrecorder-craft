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

const VAD_FILE: &str = "silero_vad.onnx";
/// silero-vad v5(sherpa-onnx 转换版,MIT,~630 KB):体积小到直接内嵌进二进制,
/// 首次使用落盘 data_dir\models\vad,免去一次网络下载
const VAD_EMBEDDED: &[u8] = include_bytes!("../assets/silero_vad.onnx");

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

/// 下载模型到 data_dir/models/(sensevoice + silero-vad;可重复执行,已存在的文件跳过)
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
    eprintln!("[stt] 模型就绪: {}(VAD 内嵌随包,无需下载)", dir.display());
    Ok(dir)
}

/// 下载文件到指定路径(.part 临时文件 + content-length 校验;diarize 模块复用)
pub(crate) fn download(url: &str, dest: &Path) -> Result<()> {
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

// ---------- silero-vad 切句 ----------

/// VAD 模型目录候选(与 sensevoice 同级:data_dir\models\vad)
fn vad_candidates(cfg: &Config) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = cfg.stt.model_dir.clone() {
        v.push(p.parent().map(|d| d.join("vad")).unwrap_or_else(|| p.clone()));
    }
    v.push(cfg.data_dir().join("models").join("vad"));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("models").join("vad"));
        }
    }
    v.push(PathBuf::from("models").join("vad"));
    v
}

/// 找到含 silero_vad.onnx 的目录
pub fn resolve_vad(cfg: &Config) -> Option<PathBuf> {
    vad_candidates(cfg).into_iter().find(|d| d.join(VAD_FILE).is_file())
}

/// 保证 silero_vad.onnx 就绪:已存在直接用;缺失则把内嵌模型落盘到 data_dir\models\vad。
pub fn ensure_vad(cfg: &Config) -> Result<PathBuf> {
    if let Some(dir) = resolve_vad(cfg) {
        return Ok(dir.join(VAD_FILE));
    }
    let dir = cfg.data_dir().join("models").join("vad");
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(VAD_FILE);
    if !dest.is_file() {
        std::fs::write(&dest, VAD_EMBEDDED)
            .with_context(|| format!("写入内嵌 VAD 模型失败: {}", dest.display()))?;
    }
    Ok(dest)
}

/// VAD 检测窗口(样本数):sherpa-onnx 把单次 AcceptWaveform 内的全部窗口
/// OR 成一个 is_speech,必须逐窗喂,否则段起点会被裁到块尾
const VAD_WINDOW_SIZE: i32 = 512;

/// VAD 切句器:喂 16k mono f32,弹完整语音段。流式、线程内独占使用。
///
/// 时间轴:`next_segment` 返回的起始样本索引相对 VAD 建立起累计,
/// 调用方用"首个喂入块的会议相对毫秒"做原点换算(见 transcribe::Streaming)。
pub struct VadStream {
    vad: sherpa_rs::silero_vad::SileroVad,
    fed: u64,
    consumed: u64,
}

impl VadStream {
    /// 内嵌模型兜底,几乎不会失败
    pub fn from_config(cfg: &Config) -> Result<Option<Self>> {
        let model = ensure_vad(cfg)?;
        Ok(Some(Self::new(&model)?))
    }

    pub fn new(model: &Path) -> Result<Self> {
        if !model.is_file() {
            bail!("VAD 模型缺失: {}", model.display());
        }
        let vad = sherpa_rs::silero_vad::SileroVad::new(
            sherpa_rs::silero_vad::SileroVadConfig {
                model: model.display().to_string(),
                // 切句参数:句尾 0.5s 静音判停;最短 0.25s;最长 20s(长发言强制切,
                // 字幕不拖);默认窗口 512 样本 @16k
                min_silence_duration: 0.5,
                min_speech_duration: 0.25,
                max_speech_duration: 20.0,
                threshold: 0.5,
                sample_rate: crate::capture::TARGET_RATE,
                window_size: VAD_WINDOW_SIZE,
                ..Default::default()
            },
            60.0, // 内部环形缓冲 60s:容纳静音段 + 未弹出语音,内存 ~3.8 MB
        )
        .map_err(|e| anyhow::anyhow!("silero-vad 加载失败: {e}"))?;
        Ok(Self { vad, fed: 0, consumed: 0 })
    }

    /// 喂入一段样本(内部按 512 窗口逐窗喂给引擎)
    pub fn accept(&mut self, samples: Vec<f32>) {
        if samples.is_empty() {
            return;
        }
        self.fed += samples.len() as u64;
        for chunk in samples.chunks(VAD_WINDOW_SIZE as usize) {
            self.vad.accept_waveform(chunk.to_vec());
        }
    }

    /// 取下一段完整语音:返回 (起始样本索引[相对 VAD 建立], 语音样本)。
    pub fn next_segment(&mut self) -> Option<(u64, Vec<f32>)> {
        if self.vad.is_empty() {
            return None;
        }
        let seg = self.vad.front();
        self.vad.pop();
        let len = seg.samples.len() as u64;
        // sherpa-onnx 的 start 是相对整个输入流的绝对样本索引;
        // 语义异常(早于已消费位置)时退化为顺序偏移,保证时间轴单调
        let start = if seg.start >= 0 && (seg.start as u64) >= self.consumed {
            seg.start as u64
        } else {
            self.consumed
        };
        self.consumed = (start + len).max(self.consumed);
        Some((start, seg.samples))
    }

    /// 结束时冲刷:把尾部未判停的语音强制成段弹出(之后照常 next_segment)
    pub fn flush(&mut self) {
        self.vad.flush();
    }
}
