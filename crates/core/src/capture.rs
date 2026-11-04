//! cpal 音频采集:默认输入设备 → 16kHz mono f32 喂转写,原始采样率写 WAV 归档。
//!
//! 线程模型:cpal 回调(实时线程)只做"转换+push";解码线程(transcribe::Worker)
//! 每 2s 醒来,从 shared 取走攒够 window_secs 的样本做流式转写,再还回未确定的尾巴。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub const TARGET_RATE: u32 = 16_000;

/// 回调与解码线程共享的实时缓冲(锁内不做 IO/解码)
pub struct Shared {
    /// 16k mono 样本,起点为 buf_start_ms(会议相对毫秒)
    pub buf: Vec<f32>,
    pub buf_start_ms: i64,
    /// 原始采样率 WAV 归档 writer
    pub writer: Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>>,
    /// 线性重采样的相位余量
    resample_pos: f64,
}

impl Shared {
    fn push_resampled(&mut self, native: &[f32], native_rate: u32) {
        if native_rate == TARGET_RATE {
            self.buf.extend_from_slice(native);
            return;
        }
        let ratio = native_rate as f64 / TARGET_RATE as f64;
        let out_len = (native.len() as f64 / ratio).floor() as usize;
        for i in 0..out_len {
            let pos = self.resample_pos + i as f64 * ratio;
            let i0 = pos.floor() as usize;
            let frac = (pos - i0 as f64) as f32;
            let s0 = native.get(i0).copied().unwrap_or(0.0);
            let s1 = native.get(i0 + 1).copied().unwrap_or(s0);
            self.buf.push(s0 + (s1 - s0) * frac);
        }
        self.resample_pos += out_len as f64 * ratio - native.len() as f64;
        if self.resample_pos < 0.0 {
            self.resample_pos = 0.0;
        }
    }
}

#[derive(Clone)]
pub struct CaptureShared(pub Arc<Mutex<Shared>>);

pub struct Capture {
    // 持有即保活:drop 时停止音频流(逻辑上"只写"字段)
    #[allow(dead_code)]
    stream: cpal::Stream,
    pub audio_path: PathBuf,
}

pub fn open(
    audio_path: &Path,
    on_error: impl Fn(String) + Send + 'static,
) -> Result<(Capture, CaptureShared)> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("没有默认录音设备"))?;
    let cfg = device.default_input_config().context("读默认输入配置失败")?;
    let native_rate = cfg.sample_rate().0;
    let channels = cfg.channels() as usize;

    if let Some(dir) = audio_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: native_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let writer = hound::WavWriter::create(audio_path, spec)
        .with_context(|| format!("创建 WAV 失败: {}", audio_path.display()))?;

    let shared = Arc::new(Mutex::new(Shared {
        buf: Vec::new(),
        buf_start_ms: 0,
        writer: Some(writer),
        resample_pos: 0.0,
    }));

    let cb_shared = Arc::clone(&shared);
    let err_fn = move |e| on_error(format!("音频流错误: {e}"));
    let stream = match cfg.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &cfg.into(),
            move |data: &[f32], _| feed(&cb_shared, data, native_rate, channels),
            err_fn,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &cfg.into(),
            move |data: &[i16], _| {
                let f: Vec<f32> = data.iter().map(|s| *s as f32 / 32768.0).collect();
                feed(&cb_shared, &f, native_rate, channels)
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            &cfg.into(),
            move |data: &[u16], _| {
                let f: Vec<f32> =
                    data.iter().map(|s| (*s as f32 - 32768.0) / 32768.0).collect();
                feed(&cb_shared, &f, native_rate, channels)
            },
            err_fn,
            None,
        ),
        other => return Err(anyhow!("不支持的采样格式: {other:?}")),
    }
    .context("建立输入流失败")?;

    stream.play().context("启动录音流失败")?;
    Ok((
        Capture {
            stream,
            audio_path: audio_path.to_path_buf(),
        },
        CaptureShared(shared),
    ))
}

/// 回调:混单声道 → 重采样缓冲 + WAV 归档
fn feed(shared: &Arc<Mutex<Shared>>, data: &[f32], rate: u32, channels: usize) {
    let mono: Vec<f32> = if channels <= 1 {
        data.to_vec()
    } else {
        data.chunks(channels)
            .map(|ch| ch.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    if let Ok(mut g) = shared.lock() {
        g.push_resampled(&mono, rate);
        if let Some(w) = g.writer.as_mut() {
            for s in &mono {
                let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                let _ = w.write_sample(v);
            }
        }
    }
}

impl CaptureShared {
    /// 取走缓冲全部样本(解码线程用)
    pub fn take_all(&self) -> (Vec<f32>, i64) {
        let mut g = self.0.lock().expect("capture 锁");
        (std::mem::take(&mut g.buf), g.buf_start_ms)
    }

    /// 解码后把未确定的尾巴还回(从 tail_start_ms 续传)
    pub fn give_back(&self, tail: Vec<f32>, tail_start_ms: i64) {
        let mut g = self.0.lock().expect("capture 锁");
        g.buf = tail;
        g.buf_start_ms = tail_start_ms;
    }

    /// 长静音时截断缓冲,只保留最后 keep_secs,防止积累
    pub fn trim_silence(&self, max_secs: f64, keep_secs: f64) {
        let mut g = self.0.lock().expect("capture 锁");
        let max_len = (TARGET_RATE as f64 * max_secs) as usize;
        if g.buf.len() > max_len {
            let drop_n = g.buf.len() - (TARGET_RATE as f64 * keep_secs) as usize;
            g.buf.drain(..drop_n);
            g.buf_start_ms += (drop_n as f64 / TARGET_RATE as f64 * 1000.0) as i64;
        }
    }

    /// 结束录音:落盘 WAV
    pub fn finalize(&self) -> Result<()> {
        let mut g = self.0.lock().expect("capture 锁");
        if let Some(w) = g.writer.take() {
            w.finalize().context("写 WAV 收尾失败")?;
        }
        Ok(())
    }
}

impl Capture {
    /// 停止并释放流(流 drop 即停)
    pub fn stop(self) {}
}
