//! STT 端到端探针:喂一段 WAV,走本地 SenseVoice(sherpa-rs 进程内 ONNX)转写。
//!
//! 用法: cargo run --release -p mrc-core --example stt_offline_probe -- <wav路径> [语言]
//! 语言缺省 auto;模型缺失时先跑 mrc-recorder --fetch-models。

use mrc_core::config::Config;
use mrc_core::stt::LocalAsr;
use mrc_core::transcribe::load_wav_16k;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let wav_path = args.get(1).expect("用法: stt_offline_probe <wav> [语言]");
    let lang = args.get(2).cloned().unwrap_or_else(|| "auto".into());

    let mut cfg = Config::load()?;
    cfg.stt.language = lang;
    let samples = load_wav_16k(std::path::Path::new(wav_path))?;
    println!(
        "音频: {:.1}s(重采样 16kHz mono)",
        samples.len() as f64 / 16000.0
    );

    let asr = LocalAsr::from_config(&cfg)?
        .ok_or_else(|| anyhow::anyhow!("STT 未启用(provider=none);模型缺失时先跑 mrc-recorder --fetch-models"))?;
    let t0 = std::time::Instant::now();
    let segs = asr.transcribe(16000, &samples)?;
    println!("转写: {:.1}s", t0.elapsed().as_secs_f64());
    for s in &segs {
        println!("[{:>5.1}s] {}", s.start_ms as f64 / 1000.0, s.text);
    }
    if segs.is_empty() {
        anyhow::bail!("无转写结果");
    }
    Ok(())
}
