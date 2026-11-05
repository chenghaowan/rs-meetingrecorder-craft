//! whisper-rs 流式转写。
//!
//! 策略:解码线程攒 window_secs(默认12s)送 whisper;取全部返回段的最后一段
//! 结束点 E 作为"已确定点",只落库 E 之前的段,把 E 之后的样本还回缓冲续传——
//! 这样下个窗口从已确定点接着解,天然无重复。长静音由 capture::trim_silence 控制。
//!
//! 特性开关:whisper feature 关闭时此处编译为空实现,采集/归档照常。

use anyhow::{bail, Context, Result};

#[cfg(feature = "whisper")]
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::capture::TARGET_RATE;

pub struct Transcriber {
    #[cfg(feature = "whisper")]
    ctx: WhisperContext,
    #[cfg(feature = "whisper")]
    language: String,
    _priv: (),
}

#[derive(Debug, Clone)]
pub struct RawSegment {
    /// 相对入参样本数组起点的毫秒
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

impl Transcriber {
    pub fn new(model_path: &std::path::Path, language: &str) -> Result<Self> {
        #[cfg(feature = "whisper")]
        {
            let ctx = WhisperContext::new_with_params(
                model_path.to_string_lossy().as_ref(),
                WhisperContextParameters::default(),
            )
            .with_context(|| format!("加载 whisper 模型失败: {}", model_path.display()))?;
            Ok(Self {
                ctx,
                language: language.to_string(),
                _priv: (),
            })
        }
        #[cfg(not(feature = "whisper"))]
        {
            let _ = (model_path, language);
            Err(anyhow::anyhow!("编译未启用 whisper feature,转写不可用"))
        }
    }

    /// 解码一段 16k mono 样本;off_ms 为样本起点对应的会议相对毫秒
    #[cfg(feature = "whisper")]
    pub fn decode(&self, samples: &[f32], off_ms: i64) -> Result<Vec<RawSegment>> {
        if samples.is_empty() {
            return Ok(vec![]);
        }
        let mut state = self.ctx.create_state().context("创建 whisper 状态失败")?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(if self.language == "auto" {
            None
        } else {
            Some(&self.language)
        });
        // whisper-rs 默认只开 4 线程;拉满物理核(CPU 推理的实时性关键)
        let threads = std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(4);
        params.set_n_threads(threads);
        // 简体中文引导(whisper 中文常输出繁体;initial_prompt 拉回简体)
        if self.language == "zh" {
            params.set_initial_prompt("以下是普通话会议内容,请用简体中文转写。");
        }
        params.set_print_progress(false);
        params.set_print_special(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_single_segment(false);
        params.set_no_context(true);
        state
            .full(params, &samples.to_vec())
            .context("whisper 推理失败")?;

        let n = state.full_n_segments()?;
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let t0 = state.full_get_segment_t0(i)?; // 毫秒
            let t1 = state.full_get_segment_t1(i)?;
            let text = state.full_get_segment_text(i)?.trim().to_string();
            if text.is_empty() {
                continue;
            }
            out.push(RawSegment {
                start_ms: off_ms + t0,
                end_ms: off_ms + t1,
                text,
            });
        }
        Ok(out)
    }

    #[cfg(not(feature = "whisper"))]
    pub fn decode(&self, _samples: &[f32], _off_ms: i64) -> Result<Vec<RawSegment>> {
        Err(anyhow::anyhow!("编译未启用 whisper feature,转写不可用"))
    }
}

/// 把 16k mono f32 样本编码为 WAV 字节(网关 /asr 上传用)。
pub fn encode_wav_16k(samples: &[f32]) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let mut out = Vec::with_capacity(44 + data_len);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());      // fmt 块长
    out.extend_from_slice(&1u16.to_le_bytes());       // PCM
    out.extend_from_slice(&1u16.to_le_bytes());       // 单声道
    out.extend_from_slice(&(TARGET_RATE as u32).to_le_bytes());
    out.extend_from_slice(&(TARGET_RATE as u32 * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes());       // block align
    out.extend_from_slice(&16u16.to_le_bytes());      // bits
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// 网关 /asr 执行器(SenseVoice,网关已就绪)。
/// 契约:POST /asr?lang=xx,body = WAV 字节(任意采样率,网关混单声道);
/// 响应 {"text": "...", "language": "..", "timestamps": ..., "ms": n}。
pub struct RemoteAsr {
    pub gateway_url: String,
    pub language: String,
    agent: ureq::Agent,
}

/// 远端返回的宽容段落(秒或毫秒都能吃)
#[derive(Debug, Clone)]
pub struct RemoteSegment {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

impl RemoteAsr {
    pub fn new(gateway_url: &str, language: &str) -> Self {
        Self {
            gateway_url: gateway_url.trim_end_matches('/').to_string(),
            language: language.to_string(),
            agent: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(120))
                .build(),
        }
    }

    pub fn transcribe(&self, samples: &[f32]) -> Result<Vec<RemoteSegment>> {
        let wav = encode_wav_16k(samples);
        let url = format!(
            "{}/asr?lang={}",
            self.gateway_url,
            if self.language == "auto" { "auto" } else { &self.language }
        );
        let resp = self
            .agent
            .post(&url)
            .set("Content-Type", "audio/wav")
            .send_bytes(&wav)
            .map_err(|e| anyhow::anyhow!("网关 {url} 不可用: {e}"))?;
        let v: serde_json::Value = resp.into_json().context("网关 /asr 响应解析失败")?;
        if let Some(err) = v["error"].as_object() {
            anyhow::bail!(
                "网关错误 [{}]: {}",
                err.get("code").and_then(|c| c.as_str()).unwrap_or("unknown"),
                err.get("message").and_then(|m| m.as_str()).unwrap_or("?")
            );
        }
        // 网关 v0.1 契约:整段 {text, language, timestamps, ms}(SenseVoice 全文)。
        // 兼容未来分段形式 {segments:[{start,end,text}]}。
        let dur_ms = (samples.len() as f64 / TARGET_RATE as f64 * 1000.0) as i64;
        if let Some(arr) = v["segments"].as_array().or_else(|| v["data"].as_array()) {
            let mut out = Vec::with_capacity(arr.len());
            for seg in arr {
                let get_ms = |a: &serde_json::Value, b: &str, c: &str| -> i64 {
                    if let Some(x) = a[b].as_f64() {
                        if x > 1000.0 { x as i64 } else { (x * 1000.0) as i64 }
                    } else if let Some(x) = a[c].as_f64() {
                        if x > 1000.0 { x as i64 } else { (x * 1000.0) as i64 }
                    } else {
                        0
                    }
                };
                let text = seg["text"].as_str().unwrap_or("").trim().to_string();
                if text.is_empty() {
                    continue;
                }
                out.push(RemoteSegment {
                    start_ms: get_ms(seg, "start", "start_ms"),
                    end_ms: get_ms(seg, "end", "end_ms"),
                    text,
                });
            }
            return Ok(out);
        }
        let text = v["text"].as_str().unwrap_or("").trim().to_string();
        if text.is_empty() {
            return Ok(vec![]);
        }
        Ok(vec![RemoteSegment {
            start_ms: 0,
            end_ms: dur_ms,
            text,
        }])
    }
}

/// 读取任意 WAV,重采样为 16k mono f32(离线转写用)。
pub fn load_wav_16k(path: &std::path::Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)
        .with_context(|| format!("打开 WAV 失败: {}", path.display()))?;
    let spec = reader.spec();
    let ch = spec.channels as usize;
    let src_rate = spec.sample_rate as f64;
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => match spec.bits_per_sample {
            16 => reader
                .samples::<i16>()
                .map(|s| Ok(s? as f32 / 32768.0))
                .collect::<std::result::Result<Vec<_>, anyhow::Error>>()?,
            32 => reader
                .samples::<i32>()
                .map(|s| Ok(s? as f32 / 2147483648.0))
                .collect::<std::result::Result<Vec<_>, anyhow::Error>>()?,
            b => bail!("不支持的位深: {b}"),
        },
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| Ok(s?))
            .collect::<std::result::Result<Vec<_>, anyhow::Error>>()?,
    };
    // 混单声道
    let mono: Vec<f32> = if ch <= 1 {
        raw
    } else {
        raw.chunks(ch).map(|c| c.iter().sum::<f32>() / ch as f32).collect()
    };
    // 线性重采样到 16k
    if (src_rate - TARGET_RATE as f64).abs() < 1.0 {
        return Ok(mono);
    }
    let ratio = src_rate / TARGET_RATE as f64;
    let out_len = (mono.len() as f64 / ratio) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 * ratio;
        let i0 = pos as usize;
        let frac = (pos - i0 as f64) as f32;
        let s0 = mono.get(i0).copied().unwrap_or(0.0);
        let s1 = mono.get(i0 + 1).copied().unwrap_or(s0);
        out.push(s0 + (s1 - s0) * frac);
    }
    Ok(out)
}

/// 流式解码循环状态机:供解码线程每 tick 调用。
pub struct Streaming {
    pub transcriber: Option<Transcriber>,
    /// wanai 网关执行器(与 transcriber 并存:远端失败回落本地,不丢内容)
    pub remote: Option<RemoteAsr>,
    pub window_secs: f64,
    /// 已落库的最后确定点(会议相对毫秒)
    pub committed_ms: i64,
}

pub enum StreamEvent {
    /// 新确定的转写段(会议相对毫秒)
    Segments(Vec<RawSegment>),
    /// 本 tick 无新段
    Idle,
    /// 解码失败(样本已还回,下次重试)
    Error(String),
}

impl Streaming {
    pub fn new(transcriber: Option<Transcriber>, window_secs: f64) -> Self {
        Self {
            transcriber,
            remote: None,
            window_secs,
            committed_ms: 0,
        }
    }

    /// 解码一窗:provider=wanai 先走网关,失败回落本地 whisper-rs
    pub fn decode_window(
        &self,
        tr: &Transcriber,
        buf: &[f32],
        buf_start_ms: i64,
    ) -> Result<Vec<RawSegment>> {
        if let Some(remote) = self.remote.as_ref() {
            match remote.transcribe(buf) {
                Ok(segs) => {
                    return Ok(segs
                        .into_iter()
                        .map(|r| RawSegment {
                            start_ms: buf_start_ms + r.start_ms,
                            end_ms: buf_start_ms + r.end_ms,
                            text: r.text,
                        })
                        .collect());
                }
                Err(e) => {
                    // 网关未就绪(M4 前常态):回落本地,只提示一次由调用方日志控制
                    return tr
                        .decode(buf, buf_start_ms)
                        .map_err(|le| anyhow::anyhow!("网关/本地均失败: {e}; 本地: {le}"));
                }
            }
        }
        tr.decode(buf, buf_start_ms)
    }

    /// 每个解码 tick:从 shared 取样本,够一个窗口才解;返回事件并处理 give_back。
    pub fn tick(&mut self, cap: &crate::capture::CaptureShared) -> StreamEvent {
        let (buf, buf_start_ms) = cap.take_all();
        let win_samples = (self.window_secs * TARGET_RATE as f64) as usize;

        // 无转写器(未启用/无模型):只做静音裁剪
        let Some(tr) = self.transcriber.as_ref() else {
            cap.trim_silence(30.0, 5.0);
            return StreamEvent::Idle;
        };

        let have_ms = buf.len() as f64 / TARGET_RATE as f64 * 1000.0;
        if have_ms < self.window_secs * 1000.0 && buf.len() < win_samples / 4 {
            // 攒不够半个窗口:还回去,等下个 tick
            cap.give_back(buf, buf_start_ms);
            cap.trim_silence(60.0, 10.0);
            return StreamEvent::Idle;
        }
        if buf.is_empty() {
            return StreamEvent::Idle;
        }

        match self.decode_window(tr, &buf, buf_start_ms) {
            Ok(segs) => {
                // 最后确定点 E(相对会议)
                let last_end = segs.last().map(|s| s.end_ms).unwrap_or(buf_start_ms);
                // 只取 end <= last_end 的段全量落库(E 即全量边界)
                let committed: Vec<RawSegment> = segs
                    .into_iter()
                    .filter(|s| s.end_ms <= last_end && s.end_ms > self.committed_ms)
                    .collect();
                self.committed_ms = self.committed_ms.max(last_end);
                // 尾巴 = E 之后的样本
                let tail_skip =
                    ((last_end - buf_start_ms).max(0) as f64 / 1000.0 * TARGET_RATE as f64) as usize;
                let tail_skip = tail_skip.min(buf.len());
                cap.give_back(buf[tail_skip..].to_vec(), last_end);
                // 整窗无语音:裁掉旧静音,防积累
                if last_end == buf_start_ms {
                    cap.trim_silence(30.0, 5.0);
                }
                if committed.is_empty() {
                    StreamEvent::Idle
                } else {
                    StreamEvent::Segments(committed)
                }
            }
            Err(e) => {
                // 解码失败:还回全部样本,避免丢内容,下个 tick 重试
                cap.give_back(buf, buf_start_ms);
                StreamEvent::Error(e.to_string())
            }
        }
    }
}
