//! WASAPI 环回采集:系统输出声(默认渲染设备,或按友好名指定的输出设备)
//! → 混单声道 → 16k mono f32 喂转写 + 原始采样率 mono WAV 归档。
//!
//! Teams/会议软件的远端声音都走系统输出,环回抓取后与麦克风并行转写,
//! 段落带 source='remote' 标记,可粗分"远端/现场"。
//!
//! 线程模型:独立采集线程,**轮询**共享模式——静音时渲染端不触发事件,
//! 事件驱动环回会卡死,必须轮询(每 20ms 读一次);stop 标志置位后收尾退出。
//! COM 在本线程内初始化/释放。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use wasapi::{DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

use crate::capture::{feed, open_shared, CaptureShared};

/// 环回轮询间隔(毫秒):静音时也持续读,时间轴连续
const POLL_MS: u64 = 20;
/// 环回请求的缓冲时长(100ns 单位,200ms)
const BUFFER_HNS: i64 = 2_000_000;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    F32,
    I16,
    I32,
}

struct Fmt {
    rate: u32,
    channels: usize,
    kind: Kind,
}

pub struct LoopbackCapture {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    pub audio_path: PathBuf,
}

impl LoopbackCapture {
    /// 置停止标志并等采集线程收尾(毫秒级)
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for LoopbackCapture {
    fn drop(&mut self) {
        // 未显式 stop 就丢弃:置标志让线程自行退出(不 join,避免阻塞 UI)
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// 打开环回采集。device_hint 非空时按友好名子串匹配输出设备,空 = 默认渲染设备。
/// 失败返回 Err(调用方降级:只录麦克风)。
pub fn open(
    audio_path: &Path,
    device_hint: &str,
    on_error: impl Fn(String) + Send + Clone + 'static,
) -> Result<(LoopbackCapture, CaptureShared)> {
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<Result<CaptureShared>>();
    let stop2 = Arc::clone(&stop);
    let apath = audio_path.to_path_buf();
    let hint = device_hint.to_string();
    let handle = std::thread::spawn(move || {
        // 初始化(设备/格式/WAV)成功后把共享缓冲交回主线程,再进入采集循环
        let mut handoff: Option<mpsc::Sender<Result<CaptureShared>>> = Some(tx);
        let _ = run_capture(&apath, &hint, &stop2, on_error, &mut handoff);
        wasapi::deinitialize();
    });
    let shared = rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| anyhow!("环回采集线程未就绪(超时)"))??;
    Ok((
        LoopbackCapture { stop, handle: Some(handle), audio_path: audio_path.to_path_buf() },
        shared,
    ))
}

fn run_capture(
    audio_path: &Path,
    device_hint: &str,
    stop: &AtomicBool,
    on_error: impl Fn(String),
    handoff: &mut Option<mpsc::Sender<Result<CaptureShared>>>,
) -> Result<()> {
    let inner = || -> Result<(CaptureShared, Fmt, wasapi::AudioClient, wasapi::AudioCaptureClient)> {
        wasapi::initialize_mta()
            .ok()
            .map_err(|e| anyhow!("COM 初始化失败: {e}"))?;
        let enumerator = DeviceEnumerator::new().map_err(|e| anyhow!("枚举音频设备失败: {e}"))?;
        let device = if device_hint.trim().is_empty() {
            enumerator
                .get_default_device(Direction::Render)
                .map_err(|e| anyhow!("无默认输出设备(环回不可用): {e}"))?
        } else {
            let coll = enumerator
                .get_device_collection(Direction::Render)
                .map_err(|e| anyhow!("枚举输出设备失败: {e}"))?;
            let kw = device_hint.trim().to_lowercase();
            let n = coll.get_nbr_devices()?;
            let mut found = None;
            for i in 0..n {
                let d = coll.get_device_at_index(i)?;
                if let Ok(name) = d.get_friendlyname() {
                    if name.to_lowercase().contains(&kw) {
                        found = Some(d);
                        break;
                    }
                }
            }
            found.ok_or_else(|| anyhow!("找不到输出设备(环回): {device_hint}"))?
        };
        let name = device.get_friendlyname().unwrap_or_default();
        eprintln!("[loopback] 环回采集输出设备: {name}");

        let mut client = device
            .get_iaudioclient()
            .map_err(|e| anyhow!("激活输出设备失败: {e}"))?;        // 首选 48k f32 立体声(autoconvert 保证格式);失败退设备混合格式
        let desired = WaveFormat::new(32, 32, SampleType::Float, 48_000, 2, None);
        let shared_mode = StreamMode::PollingShared { autoconvert: true, buffer_duration_hns: BUFFER_HNS };
        let fmt = match client.initialize_client(&desired, Direction::Capture, shared_mode, None) {
            Ok(()) => Fmt { rate: 48_000, channels: 2, kind: Kind::F32 },
            Err(_) => {
                let mix = client.get_mixformat().context("读设备混合格式失败")?;
                let bits = mix.get_bitspersample();
                let kind = match mix.get_subformat()? {
                    SampleType::Float if bits == 32 => Kind::F32,
                    SampleType::Int if bits == 16 => Kind::I16,
                    SampleType::Int if bits == 32 => Kind::I32,
                    other => return Err(anyhow!("环回不支持的设备格式({other:?},{bits}bit)")),
                };
                let fmt = Fmt {
                    rate: mix.get_samplespersec(),
                    channels: mix.get_nchannels() as usize,
                    kind,
                };
                client.initialize_client(
                    &mix,
                    Direction::Capture,
                    StreamMode::PollingShared { autoconvert: false, buffer_duration_hns: BUFFER_HNS },
                    None,
                )?;
                fmt
            }
        };
        let shared = open_shared(audio_path, fmt.rate)?;
        let capture = client
            .get_audiocaptureclient()
            .map_err(|e| anyhow!("建环回捕获客户端失败: {e}"))?;
        client.start_stream().map_err(|e| anyhow!("启动环回流失败: {e}"))?;
        Ok((shared, fmt, client, capture))
    };

    let (shared, fmt, client, capture) = match inner() {
        Ok(v) => v,
        Err(e) => {
            on_error(format!("环回采集未启动: {e:#}"));
            if let Some(tx) = handoff.take() {
                let _ = tx.send(Err(anyhow!("{e:#}")));
            }
            return Err(e);
        }
    };
    if let Some(tx) = handoff.take() {
        let _ = tx.send(Ok(shared.clone()));
    }
    eprintln!("[loopback] 环回采集运行中({}Hz {}ch)", fmt.rate, fmt.channels);

    // 渲染设备方向 + Capture + 共享模式 => wasapi 自动加 LOOPBACK 标志
    let mut deque: VecDeque<u8> = VecDeque::with_capacity(1 << 20);
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(POLL_MS));
        if let Err(e) = capture.read_from_device_to_deque(&mut deque) {
            on_error(format!("环回读取失败,停止环回: {e}"));
            break;
        }
        if deque.is_empty() {
            continue;
        }
        let bytes: Vec<u8> = deque.drain(..).collect();
        match fmt.kind {
            Kind::F32 => {
                let f: Vec<f32> = bytes
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                feed(&shared, &f, fmt.rate, fmt.channels);
            }
            Kind::I16 => {
                let f: Vec<f32> = bytes
                    .chunks_exact(2)
                    .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                    .collect();
                feed(&shared, &f, fmt.rate, fmt.channels);
            }
            Kind::I32 => {
                let f: Vec<f32> = bytes
                    .chunks_exact(4)
                    .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / 2147483648.0)
                    .collect();
                feed(&shared, &f, fmt.rate, fmt.channels);
            }
        }
    }
    let _ = client.stop_stream();
    Ok(())
}
