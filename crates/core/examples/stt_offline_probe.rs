//! STT 端到端探针:喂一段 WAV,走 wanai 网关 /asr(SenseVoice)转写。
//!
//! 用法: cargo run --release -p mrc-core --example stt_offline_probe -- <wav路径> [网关URL]
//! 网关缺省 http://127.0.0.1:17800(未启动请先跑 mrc-recorder --gateway)。

use mrc_core::transcribe::{load_wav_16k, RemoteAsr};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let wav_path = args.get(1).expect("用法: stt_offline_probe <wav> [网关URL]");
    let gateway = args.get(2).cloned().unwrap_or_else(|| "http://127.0.0.1:17800".into());

    let samples = load_wav_16k(std::path::Path::new(wav_path))?;
    println!(
        "网关: {gateway}\n音频: {:.1}s(重采样 16kHz mono)",
        samples.len() as f64 / 16000.0
    );

    let asr = RemoteAsr::new(&gateway, "zh");
    let t0 = std::time::Instant::now();
    let segs = asr.transcribe(&samples)?;
    println!("转写: {:.1}s", t0.elapsed().as_secs_f64());
    for s in &segs {
        println!("[{:>5.1}s] {}", s.start_ms as f64 / 1000.0, s.text);
    }
    if segs.is_empty() {
        anyhow::bail!("无转写结果");
    }
    Ok(())
}
