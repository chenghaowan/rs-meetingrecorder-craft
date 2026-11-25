//! 无头 CLI(供脚本 / AI 代理,同 pointtofixcraft --capture 模式):
//!
//! ```text
//! mrc-recorder --start [--title 标题] [--stop-file 路径]
//!                                       # 无 GUI 开始会议; 交互终端回车结束,
//!                                       # 无头调用方触碰 stop-file 结束;
//!                                       # 结束后自动跑后处理流水线, 输出 JSON 回执
//! mrc-recorder --status                 # 最近 10 场会议状态 JSON
//! 不带参数                              # 正常启动 GUI
//! ```
//! 退出码:0 成功,1 失败,2 参数错误。--start 全程控制台日志可跟。

use std::io::{BufRead, IsTerminal};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};

use mrc_core::capture;
use mrc_core::config::Config;
use mrc_core::db;
use mrc_core::pipeline::{self, PipelineStop};
use mrc_core::stt::LocalAsr;
use mrc_core::transcribe::{load_wav_16k, Streaming};
use mrc_core::tools;
use mrc_core::translate::Translator;
use mrc_core::windowcap;
use std::sync::Arc;

use crate::i18n::Lang;

pub enum CliAction {
    /// 正常启动 GUI
    Gui,
    /// 无头录音;stop_file 存在时结束录音(供 ltl 页面等无控制台调用方)
    Start { title: Option<String>, stop_file: Option<String> },
    Status,
    /// 工具子命令(安装/启动 pointtofixcraft)
    ToolInstall { src: Option<String> },
    ToolLaunch,
    /// 下载本地模型(SenseVoice 转写;后续扩展 VAD/diarization)
    FetchModels,
    /// 指定窗口截图(经 pointtofixcraft 落库)
    ShotWindow { keyword: String, ocr: bool },
    /// 无头转写一个 WAV(本地 SenseVoice),stdout 输出 JSON 段落
    Transcribe { wav: String },
    Bad(String),
}

pub fn parse(args: impl Iterator<Item = String>) -> CliAction {
    let args: Vec<String> = args.collect();
    let mut i = 0;
    let mut title = None;
    let mut stop_file = None;
    let mut start = false;
    while i < args.len() {
        match args[i].as_str() {
            // 不再立刻返回: --start 之后的 --title/--stop-file 也要生效
            "--start" => start = true,
            "--title" => {
                i += 1;
                title = args.get(i).cloned();
            }
            "--stop-file" => {
                i += 1;
                stop_file = args.get(i).cloned();
            }
            "--status" if !start => return CliAction::Status,
            "--tool-install" => {
                i += 1;
                return CliAction::ToolInstall { src: args.get(i).cloned() };
            }
            "--tool-launch" => return CliAction::ToolLaunch,
            "--fetch-models" => return CliAction::FetchModels,
            "--transcribe" => {
                i += 1;
                let wav = args.get(i).cloned()
                    .ok_or_else(|| "xxx".to_string())
                    .unwrap_or_default();
                return CliAction::Transcribe { wav };
            }
            "--shot-window" => {
                i += 1;
                let keyword = args.get(i).cloned()
                    .unwrap_or_else(|| std::env::var("LTL_MR_SHOT_WINDOW").unwrap_or_default());
                return CliAction::ShotWindow { keyword, ocr: false };
            }
            "--shot-window-ocr" => {
                i += 1;
                let keyword = args.get(i).cloned()
                    .unwrap_or_else(|| std::env::var("LTL_MR_SHOT_WINDOW").unwrap_or_default());
                return CliAction::ShotWindow { keyword, ocr: true };
            }
            other => return CliAction::Bad(format!("未知参数: {other}")),
        }
        i += 1;
    }
    if start {
        return CliAction::Start { title, stop_file };
    }
    CliAction::Gui
}

pub fn run(action: CliAction) -> i32 {
    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("错误: {e:#}");
            return 1;
        }
    };
    let tr = cfg.lang.as_deref().and_then(Lang::from_code).unwrap_or_else(Lang::detect).strings();
    match action {
        CliAction::Status => cmd_status(&cfg),
        CliAction::Start { title, stop_file } => match cmd_start(&cfg, title.as_deref(), stop_file.as_deref()) {
            Ok(json) => {
                println!("{json}");
                0
            }
            Err(e) => {
                eprintln!("错误: {e:#}");
                1
            }
        },
        CliAction::ToolInstall { src } => match tools::install(&cfg, src.as_deref()) {
            Ok(p) => {
                println!("{}: {}", tr.cli_installed, p.display());
                0
            }
            Err(e) => {
                eprintln!("错误: {e:#}");
                1
            }
        },
        CliAction::ToolLaunch => match tools::launch(&cfg) {
            Ok(p) => {
                println!("{}: {}", tr.cli_started, p.display());
                0
            }
            Err(e) => {
                eprintln!("错误: {e:#}");
                1
            }
        },
        CliAction::Transcribe { wav } => match cmd_transcribe(&cfg, &wav) {
            Ok(v) => {
                println!("{v}");
                0
            }
            Err(e) => {
                eprintln!("错误: {e:#}");
                1
            }
        },
        CliAction::FetchModels => match mrc_core::stt::install_model(&cfg) {
            Ok(p) => {
                println!("models: {}", p.display());
                0
            }
            Err(e) => {
                eprintln!("错误: {e:#}");
                1
            }
        },
        CliAction::ShotWindow { keyword, ocr } => match shot_window(&cfg, &keyword, ocr) {
            Ok(v) => {
                println!("{v}");
                0
            }
            Err(e) => {
                eprintln!("错误: {e:#}");
                1
            }
        },
        CliAction::Bad(msg) => {
            eprintln!("参数错误: {msg}");
            eprintln!("用法: mrc-recorder [--start [--title X] | --status | --tool-install [来源] | --tool-launch | --fetch-models | --shot-window 关键词 [--shot-window-ocr] | --transcribe <wav>]");
    eprintln!("  [stt] 唯一引擎 = 本地 SenseVoice(sherpa-rs 进程内 ONNX);--fetch-models 从 hf-mirror 下载模型(约 230 MB)");
            2
        }
        CliAction::Gui => 0,
    }
}

fn cmd_transcribe(cfg: &Config, wav: &str) -> Result<serde_json::Value> {
    let samples = load_wav_16k(std::path::Path::new(wav))?;
    let dur = samples.len() as f64 / 16000.0;
    let asr = LocalAsr::from_config(cfg)?
        .ok_or_else(|| anyhow::anyhow!("STT 未启用(provider=none);模型缺失时先跑 mrc-recorder --fetch-models"))?;
    let mut vad = match mrc_core::stt::VadStream::from_config(cfg) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[stt] VAD 不可用,整段转写: {e:#}");
            None
        }
    };
    let t0 = std::time::Instant::now();
    let segs = mrc_core::transcribe::transcribe_offline(&asr, vad.as_mut(), &samples)?;
    let elapsed = t0.elapsed().as_secs_f64();
    let items: Vec<serde_json::Value> = segs
        .iter()
        .map(|s| {
            serde_json::json!({
                "start_ms": s.start_ms, "end_ms": s.end_ms, "text": s.text,
            })
        })
        .collect();
    Ok(serde_json::json!({
        "wav": wav,
        "duration_s": (dur * 10.0).round() / 10.0,
        "engine": "local/sensevoice",
        "elapsed_s": (elapsed * 10.0).round() / 10.0,
        "segments": items,
    }))
}

fn shot_window(cfg: &Config, keyword: &str, ocr: bool) -> Result<serde_json::Value> {
    let kw = if keyword.trim().is_empty() { cfg.shots.default_window.as_str() } else { keyword };
    let exe = tools::resolve_available(cfg)
        .ok_or_else(|| anyhow::anyhow!("pointtofixcraft 未安装(--tool-install 或设置里安装)"))?;
    let info = windowcap::find_window(kw)?;
    let receipt = windowcap::capture_window(&exe, &info, ocr)?;
    Ok(serde_json::json!({
        "window": { "title": info.title, "x": info.x, "y": info.y, "w": info.w, "h": info.h },
        "shot_id": receipt.id,
        "file": receipt.file,
        "ocr": receipt.ocr,
    }))
}

fn cmd_status(cfg: &Config) -> i32 {
    let conn = match db::open(&cfg.db_path()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("错误: {e:#}");
            return 1;
        }
    };
    let meetings = db::list_meetings(&conn).unwrap_or_default();
    let items: Vec<serde_json::Value> = meetings
        .iter()
        .take(10)
        .map(|m| {
            serde_json::json!({
                "id": m.id,
                "title": m.title,
                "started_at": db::fmt_local(m.started_at),
                "status": m.status,
                "duration_min": m.duration_ms().map(|d| d / 60000),
                "package_dir": m.package_dir,
            })
        })
        .collect();
    println!(
        "{}",
        serde_json::json!({
            "db": cfg.db_path().display().to_string(),
            "pointtofix": tools::resolve_available(cfg).map(|p| p.display().to_string()),
            "meetings": items,
        })
    );
    0
}

/// 无头录音:交互终端=回车结束;无控制台调用方(ltl 页面)传 --stop-file,
/// 主循环轮询文件存在即结束 → 后处理 → JSON 回执
fn cmd_start(cfg: &Config, title: Option<&str>, stop_file: Option<&str>) -> Result<serde_json::Value> {
    // 模型兜底:缺 SenseVoice 模型先自动下载(一次性,可能耗时)
    if mrc_core::stt::enabled(&cfg.stt) && mrc_core::stt::resolve_dir(cfg).is_none() {
        match mrc_core::stt::install_model(cfg) {
            Ok(p) => eprintln!("[stt] 模型就绪: {}", p.display()),
            Err(e) => eprintln!("[stt] 模型缺失: {e:#}(本次只录音归档,不转写)"),
        }
    }
    let now = db::now_ms();
    let title = title
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("会议 {}", db::fmt_local(now)));
    let conn = db::open(&cfg.db_path())?;
    let meeting_id = db::insert_meeting(&conn, &title, now)?;
    let audio_path = cfg.audio_dir().join(format!("meeting_{meeting_id}.wav"));
    let (_cap, shared) = capture::open(&audio_path, |e| eprintln!("[capture] {e}"))
        .context("打开录音设备失败")?;

    // 转写线程(同 GUI 版逻辑)
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel::<Result<()>>();
    let cfg2 = cfg.clone();
    let shared2 = shared.clone();
    let translator = Translator::new(&cfg2.translate, &cfg2.llm).ok();
    let asr = LocalAsr::from_config(&cfg2).ok().flatten().map(Arc::new);
    let vad = match mrc_core::stt::VadStream::from_config(&cfg2) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[stt] VAD 不可用,回退固定窗口切句: {e:#}");
            None
        }
    };
    std::thread::spawn(move || {
        let mut streaming = Streaming::new(asr, vad, cfg2.stt.window_secs);
        let mut idx: i64 = 0;
        let res = (|| -> Result<()> {
            let conn = db::open(&cfg2.db_path())?;
            loop {
                match stop_rx.try_recv() {
                    Ok(()) | Err(mpsc::TryRecvError::Disconnected) => break,
                    Err(mpsc::TryRecvError::Empty) => {}
                }
                match streaming.tick(&shared2) {
                    mrc_core::transcribe::StreamEvent::Segments(segs) => {
                        let mut new_texts: Vec<String> = Vec::new();
                        let mut new_idx: Vec<i64> = Vec::new();
                        for s in &segs {
                            db::insert_segment(&conn, meeting_id, idx, s.start_ms, s.end_ms, None, &s.text)?;
                            new_idx.push(idx);
                            new_texts.push(s.text.clone());
                            idx += 1;
                            eprintln!("[cli] + {}", s.text.chars().take(50).collect::<String>());
                        }
                        if let Some(tr) = translator.as_ref() {
                            let outs = tr.translate_batch(&new_texts);
                            for (k, o) in outs.iter().enumerate() {
                                if let Some(v) = o {
                                    let _ = db::update_translation(&conn, new_idx[k], meeting_id, v);
                                    eprintln!("[cli] = {}", v.chars().take(50).collect::<String>());
                                }
                            }
                        }
                    }
                    mrc_core::transcribe::StreamEvent::Error(e) => eprintln!("[warn] 转写失败(重试): {e}"),
                    mrc_core::transcribe::StreamEvent::Idle => {}
                }
                std::thread::sleep(Duration::from_millis(2000));
            }
            if let mrc_core::transcribe::StreamEvent::Segments(segs) = streaming.drain(&shared2) {
                for s in &segs {
                    db::insert_segment(&conn, meeting_id, idx, s.start_ms, s.end_ms, None, &s.text)?;
                }
            }
            Ok(())
        })();
        let _ = done_tx.send(res);
    });

    // 等结束: 交互终端另起线程等回车(无终端时不等——CREATE_NO_WINDOW 下
    // stdin 是 EOF, 会立刻返回导致录音秒停); 无头调用方靠 --stop-file 轮询
    let interactive = std::io::stdin().is_terminal();
    let (quit_tx, quit_rx) = mpsc::channel::<()>();
    if interactive {
        std::thread::spawn(move || {
            let _ = std::io::stdin().lock().read_line(&mut String::new());
            let _ = quit_tx.send(());
        });
        eprintln!("[cli] 会议 #{meeting_id} 录音中… 回车结束");
    } else if let Some(p) = stop_file {
        eprintln!("[cli] 会议 #{meeting_id} 录音中… 停止文件 {} 出现即结束", p);
    } else {
        eprintln!("[cli] 会议 #{meeting_id} 录音中… 非交互且未给 --stop-file, 将一直录到进程被杀");
    }
    loop {
        if quit_rx.try_recv().is_ok() {
            break;
        }
        if let Some(p) = stop_file {
            if std::path::Path::new(p).exists() {
                eprintln!("[cli] 检测到停止文件, 结束录音");
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let _ = stop_tx.send(());
    if let Ok(Err(e)) = done_rx.recv_timeout(Duration::from_secs(30)) {
        eprintln!("[warn] 转写线程异常: {e:#}");
    }
    shared.finalize()?;
    let ended = db::now_ms();
    db::finish_meeting(&conn, meeting_id, ended, &format!("audio\\meeting_{meeting_id}.wav"))?;

    // 后处理(控制台交互命名)
    let outcome = pipeline::run(cfg, &mut { conn }, meeting_id, true, false, false);
    let (status, pkg, note) = match outcome {
        Ok(r) => ("done".to_string(), r.package_dir.clone(), None),
        Err((_, Some(PipelineStop::NamingNeeded(keys)))) => (
            "need_naming".to_string(),
            None,
            Some(format!("未命名说话人: {keys:?} — mrc-postprocess run {meeting_id}")),
        ),
        Err((e, None)) => ("failed".to_string(), None, Some(format!("{e:#}"))),
    };
    let segments = db::open(&cfg.db_path())?
        .query_row("SELECT COUNT(*) FROM segments WHERE meeting_id=?1", [meeting_id], |r| r.get::<_, i64>(0))
        .unwrap_or(0);
    Ok(serde_json::json!({
        "meeting_id": meeting_id,
        "title": title,
        "status": status,
        "segments": segments,
        "package_dir": pkg,
        "note": note,
    }))
}
