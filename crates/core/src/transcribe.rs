//! 流式转写(本地 SenseVoice,sherpa-rs 进内 ONNX,唯一 STT 引擎)。
//!
//! 双模式:有 silero-vad → 按语音活动切句(一段=一句话,延迟低);
//! 无 VAD → 固定窗口模式(攒 window_secs 送识别,尾巴续传,天然无重复)。
//! 长静音由 capture::trim_silence 控制(仅窗口/归档模式)。
//! 引擎不可用时样本还回、下个 tick 重试。

use std::sync::Arc;

use anyhow::{bail, Context, Result};

use crate::capture::TARGET_RATE;
use crate::stt::LocalAsr;

/// 已落库确定的转写段(会议相对毫秒)
#[derive(Debug, Clone)]
pub struct RawSegment {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
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

/// 离线转写一整段 16k mono f32:优先 VAD 切句(VAD 时间轴从 0 累计),
/// VAD 缺失时整段一次识别。
pub fn transcribe_offline(
    asr: &LocalAsr,
    vad: Option<&mut crate::stt::VadStream>,
    samples: &[f32],
) -> Result<Vec<RawSegment>> {
    let mut out = Vec::new();
    match vad {
        Some(v) => {
            v.accept(samples.to_vec());
            v.flush();
            while let Some((start_sample, seg)) = v.next_segment() {
                if seg.is_empty() {
                    continue;
                }
                let start_ms = (start_sample as f64 / TARGET_RATE as f64 * 1000.0) as i64;
                let end_ms = start_ms + (seg.len() as f64 / TARGET_RATE as f64 * 1000.0) as i64;
                for s in asr.transcribe(TARGET_RATE, &seg)? {
                    if s.text.trim().is_empty() {
                        continue;
                    }
                    out.push(RawSegment { start_ms, end_ms, text: s.text });
                }
            }
        }
        None => {
            for s in asr.transcribe(TARGET_RATE, samples)? {
                if s.text.trim().is_empty() {
                    continue;
                }
                out.push(RawSegment { start_ms: s.start_ms, end_ms: s.end_ms, text: s.text });
            }
        }
    }
    Ok(out)
}

/// 流式解码循环状态机:供解码线程每 tick 调用。
///
/// 双模式:有 VAD → 按语音活动切句(一段=一句话,延迟低,粒度自然);
/// 无 VAD → 固定窗口模式(攒 window_secs 送识别,尾巴续传,兼容回退)。
pub struct Streaming {
    /// 本地 SenseVoice 执行器(None = 只归档不转写)
    pub asr: Option<Arc<LocalAsr>>,
    /// silero-vad 切句器(None = 回退固定窗口模式)
    pub vad: Option<crate::stt::VadStream>,
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
    pub fn new(asr: Option<Arc<LocalAsr>>, vad: Option<crate::stt::VadStream>, window_secs: f64) -> Self {
        Self { asr, vad, window_secs, committed_ms: 0 }
    }

    /// 解码一窗:走本地 SenseVoice(固定窗口模式用)。
    pub fn decode_window(&self, buf: &[f32], buf_start_ms: i64) -> Result<Vec<RawSegment>> {
        let asr = self
            .asr
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("转写器未配置(本地 SenseVoice)"))?;
        let segs = asr.transcribe(TARGET_RATE, buf)?;
        Ok(segs
            .into_iter()
            .map(|r| RawSegment {
                start_ms: buf_start_ms + r.start_ms,
                end_ms: buf_start_ms + r.end_ms,
                text: r.text,
            })
            .collect())
    }

    /// 每个解码 tick:有 VAD 走切句,否则固定窗口。
    pub fn tick(&mut self, cap: &crate::capture::CaptureShared) -> StreamEvent {
        if self.vad.is_some() && self.asr.is_some() {
            self.tick_vad(cap)
        } else {
            self.tick_window(cap)
        }
    }

    /// 会议收尾:冲刷 VAD 尾部语音并识别(窗口模式等价最后一次 tick)。
    pub fn drain(&mut self, cap: &crate::capture::CaptureShared) -> StreamEvent {
        if let (Some(vad), Some(_)) = (self.vad.as_mut(), self.asr.as_ref()) {
            let (buf, _buf_start_ms) = cap.take_all();
            vad.accept(buf);
            vad.flush();
            let segs = self.pop_and_recognize();
            return if segs.is_empty() { StreamEvent::Idle } else { StreamEvent::Segments(segs) };
        }
        self.tick_window(cap)
    }

    /// VAD 模式 tick:VAD 时间轴从会议起点累计(capture 缓冲不裁不还,buf_start_ms 恒 0),
    /// 段时间 = 样本偏移直接换算会议相对毫秒。
    fn tick_vad(&mut self, cap: &crate::capture::CaptureShared) -> StreamEvent {
        let (buf, _buf_start_ms) = cap.take_all();
        if buf.is_empty() {
            return StreamEvent::Idle;
        }
        let Some(vad) = self.vad.as_mut() else { return StreamEvent::Idle };
        vad.accept(buf);
        let segs = self.pop_and_recognize();
        if segs.is_empty() { StreamEvent::Idle } else { StreamEvent::Segments(segs) }
    }

    /// 弹出 VAD 已就绪段并逐段识别(单段失败丢弃不阻塞,上限 20s 损失可控)
    fn pop_and_recognize(&mut self) -> Vec<RawSegment> {
        let Some(vad) = self.vad.as_mut() else { return vec![] };
        let Some(asr) = self.asr.as_ref() else { return vec![] };
        let mut out = Vec::new();
        while let Some((start_sample, samples)) = vad.next_segment() {
            if samples.is_empty() {
                continue;
            }
            let start_ms = (start_sample as f64 / TARGET_RATE as f64 * 1000.0) as i64;
            let end_ms = start_ms + (samples.len() as f64 / TARGET_RATE as f64 * 1000.0) as i64;
            match asr.transcribe(TARGET_RATE, &samples) {
                Ok(segs) => {
                    for s in segs {
                        if s.text.trim().is_empty() {
                            continue;
                        }
                        out.push(RawSegment { start_ms, end_ms, text: s.text });
                    }
                }
                Err(e) => eprintln!("[transcribe] 段识别失败({start_ms}~{end_ms}ms,丢弃): {e}"),
            }
        }
        self.committed_ms = out.last().map(|s| s.end_ms).unwrap_or(self.committed_ms);
        out
    }

    /// 固定窗口模式 tick:攒 window_secs 送识别;取最后确定点 E 之前的段落库,
    /// E 之后样本 give_back 续传(天然无重复)。
    fn tick_window(&mut self, cap: &crate::capture::CaptureShared) -> StreamEvent {
        let (buf, buf_start_ms) = cap.take_all();
        let win_samples = (self.window_secs * TARGET_RATE as f64) as usize;

        // 无转写器:只做静音裁剪(纯归档模式)
        let Some(_) = self.asr.as_ref() else {
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
