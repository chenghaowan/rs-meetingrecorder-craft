//! whisper-rs 流式转写。
//!
//! 策略:解码线程攒 window_secs(默认12s)送 whisper;取全部返回段的最后一段
//! 结束点 E 作为"已确定点",只落库 E 之前的段,把 E 之后的样本还回缓冲续传——
//! 这样下个窗口从已确定点接着解,天然无重复。长静音由 capture::trim_silence 控制。
//!
//! 特性开关:whisper feature 关闭时此处编译为空实现,采集/归档照常。

use anyhow::{Context, Result};

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

/// 流式解码循环状态机:供解码线程每 tick 调用。
pub struct Streaming {
    pub transcriber: Option<Transcriber>,
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
            window_secs,
            committed_ms: 0,
        }
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

        match tr.decode(&buf, buf_start_ms) {
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
