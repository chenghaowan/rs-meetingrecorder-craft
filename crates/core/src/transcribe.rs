//! 流式转写(wanai 网关 /asr,SenseVoice,唯一 STT 引擎)。
//!
//! 策略:解码线程攒 window_secs(默认12s)送网关;取全部返回段的最后一段
//! 结束点 E 作为"已确定点",只落库 E 之前的段,把 E 之后的样本还回缓冲续传——
//! 这样下个窗口从已确定点接着解,天然无重复。长静音由 capture::trim_silence 控制。
//! 网关不可用时样本还回、下个 tick 重试(gateway::ensure 负责自动安装/拉起)。

use anyhow::{bail, Context, Result};

use crate::capture::TARGET_RATE;

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

/// 已落库确定的转写段(会议相对毫秒)
#[derive(Debug, Clone)]
pub struct RawSegment {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
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
    /// wanai 网关执行器(唯一 STT 引擎;None = 只归档不转写)
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
    pub fn new(remote: Option<RemoteAsr>, window_secs: f64) -> Self {
        Self {
            remote,
            window_secs,
            committed_ms: 0,
        }
    }

    /// 解码一窗:走网关 /asr(SenseVoice)。
    pub fn decode_window(&self, buf: &[f32], buf_start_ms: i64) -> Result<Vec<RawSegment>> {
        let remote = self
            .remote
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("转写器未配置(网关 /asr)"))?;
        let segs = remote.transcribe(buf)?;
        Ok(segs
            .into_iter()
            .map(|r| RawSegment {
                start_ms: buf_start_ms + r.start_ms,
                end_ms: buf_start_ms + r.end_ms,
                text: r.text,
            })
            .collect())
    }

    /// 每个解码 tick:从 shared 取样本,够一个窗口才解;返回事件并处理 give_back。
    pub fn tick(&mut self, cap: &crate::capture::CaptureShared) -> StreamEvent {
        let (buf, buf_start_ms) = cap.take_all();
        let win_samples = (self.window_secs * TARGET_RATE as f64) as usize;

        // 无转写器:只做静音裁剪(纯归档模式)
        let Some(_) = self.remote.as_ref() else {
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

        match self.decode_window(&buf, buf_start_ms) {
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
