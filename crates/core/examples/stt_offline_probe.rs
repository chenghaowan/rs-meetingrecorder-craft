//! 离线 STT 端到端探针:喂一段 16k mono WAV,用本地 whisper 转写。
//!
//! 用法: cargo run --release -p mrc-core --example stt_offline_probe -- <wav路径> [模型路径]
//! 模型缺省自动查找(models/ggml-*.bin)。完全不联网。

use mrc_core::transcribe::Transcriber;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let wav_path = args.get(1).expect("用法: stt_offline_probe <wav> [模型路径]");
    let model = args
        .get(2)
        .map(std::path::PathBuf::from)
        .or_else(|| {
            // 与 Config::find_whisper_model 相同的查找逻辑(简化版:仓库 models\)
            let dir = std::path::Path::new("models");
            let mut bins: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
                .ok()?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "bin"))
                .collect();
            bins.sort();
            bins.into_iter().next()
        })
        .expect("找不到 whisper 模型: 先 python scripts/fetch_whisper.py");

    let mut reader = hound::WavReader::open(wav_path)?;
    let spec = reader.spec();
    assert_eq!(spec.sample_rate, mrc_core::capture::TARGET_RATE, "WAV 须为 16kHz");
    assert_eq!(spec.channels, 1, "WAV 须为单声道");
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| s.unwrap() as f32 / 32768.0)
        .collect();
    println!(
        "模型: {}\n音频: {:.1}s @16kHz mono",
        model.display(),
        samples.len() as f64 / 16000.0
    );

    let t0 = std::time::Instant::now();
    let tr = Transcriber::new(&model, "zh")?;
    println!("模型加载: {:.1}s", t0.elapsed().as_secs_f64());

    let t1 = std::time::Instant::now();
    let segs = tr.decode(&samples, 0)?;
    println!("推理: {:.1}s", t1.elapsed().as_secs_f64());
    for s in &segs {
        println!("[{:>5.1}s] {}", s.start_ms as f64 / 1000.0, s.text);
    }
    if segs.is_empty() {
        anyhow::bail!("无转写结果");
    }
    Ok(())
}
