//! 无头 CLI(供脚本 / AI 代理,同 pointtofixcraft --capture 模式):
//!
//! ```text
//! mrc-recorder --start [--title 标题]   # 无 GUI 开始会议,录音到回车/Ctrl+C,
//!                                       # 结束后自动跑后处理流水线,输出 JSON 回执
//! mrc-recorder --status                 # 最近 10 场会议状态 JSON
//! 不带参数                              # 正常启动 GUI
//! ```
//! 退出码:0 成功,1 失败,2 参数错误。--start 全程控制台日志可跟。

use std::io::BufRead;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};

use mrc_core::capture;
use mrc_core::config::Config;
use mrc_core::db;
use mrc_core::pipeline::{self, PipelineStop};
use mrc_core::transcribe::{Streaming, Transcriber};
use mrc_core::tools;

use crate::i18n::Lang;

pub enum CliAction {
    /// 正常启动 GUI
    Gui,
    Start { title: Option<String> },
    Status,
    /// 工具子命令(安装/启动 pointtofixcraft)
    ToolInstall { src: Option<String> },
    ToolLaunch,
    Bad(String),
}

pub fn parse(args: impl Iterator<Item = String>) -> CliAction {
    let args: Vec<String> = args.collect();
    let mut i = 0;
    let mut title = None;
    while i < args.len() {
        match args[i].as_str() {
            "--start" => return CliAction::Start { title: title.take() },
            "--title" => {
                i += 1;
                title = args.get(i).cloned();
            }
            "--status" => return CliAction::Status,
            "--tool-install" => {
                i += 1;
                return CliAction::ToolInstall { src: args.get(i).cloned() };
            }
            "--tool-launch" => return CliAction::ToolLaunch,
            other => return CliAction::Bad(format!("未知参数: {other}")),
        }
        i += 1;
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
        CliAction::Start { title } => match cmd_start(&cfg, title.as_deref()) {
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
        CliAction::Bad(msg) => {
            eprintln!("参数错误: {msg}");
            eprintln!("用法: mrc-recorder [--start [--title X] | --status | --tool-install [来源] | --tool-launch]");
            2
        }
        CliAction::Gui => 0,
    }
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

/// 无头录音:回车 / Ctrl+C 结束 → 后处理(控制台交互命名)→ JSON 回执
fn cmd_start(cfg: &Config, title: Option<&str>) -> Result<serde_json::Value> {
    let now = db::now_ms();
    let title = title
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("会议 {}", db::fmt_local(now)));
    let conn = db::open(&cfg.db_path())?;
    let meeting_id = db::insert_meeting(&conn, &title, now)?;
    let audio_path = cfg.audio_dir().join(format!("meeting_{meeting_id}.wav"));
    let (_cap, shared) = capture::open(&audio_path, |e| eprintln!("[capture] {e}"))
        .context("打开录音设备失败")?;
    eprintln!("[cli] 会议 #{meeting_id} 录音中… 回车或 Ctrl+C 结束");

    // 转写线程(同 GUI 版逻辑)
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel::<Result<()>>();
    let cfg2 = cfg.clone();
    let shared2 = shared.clone();
    std::thread::spawn(move || {
        let tr_th = cfg2.lang.as_deref().and_then(Lang::from_code).unwrap_or_else(Lang::detect).strings();
        let transcriber = cfg2.find_whisper_model().and_then(|m| {
            match Transcriber::new(&m, &cfg2.whisper.language) {
                Ok(t) => Some(t),
                Err(e) => {
                    eprintln!("[warn] {}: {e:#}", tr_th.err_tr_disabled);
                    None
                }
            }
        });
        let mut streaming = Streaming::new(transcriber, cfg2.whisper.window_secs);
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
                        for s in &segs {
                            db::insert_segment(&conn, meeting_id, idx, s.start_ms, s.end_ms, None, &s.text)?;
                            idx += 1;
                            eprintln!("[cli] + {}", s.text.chars().take(50).collect::<String>());
                        }
                    }
                    mrc_core::transcribe::StreamEvent::Error(e) => eprintln!("[warn] 转写失败(重试): {e}"),
                    mrc_core::transcribe::StreamEvent::Idle => {}
                }
                std::thread::sleep(Duration::from_millis(2000));
            }
            if let mrc_core::transcribe::StreamEvent::Segments(segs) = streaming.tick(&shared2) {
                for s in &segs {
                    db::insert_segment(&conn, meeting_id, idx, s.start_ms, s.end_ms, None, &s.text)?;
                }
            }
            Ok(())
        })();
        let _ = done_tx.send(res);
    });

    // 等回车 / Ctrl+C
    let _ = std::io::stdin().lock().read_line(&mut String::new());
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
