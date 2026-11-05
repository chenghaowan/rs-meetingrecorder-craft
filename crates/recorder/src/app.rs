//! 悬浮主面板:状态 + 开始/结束 + 设置窗 + 命名窗 + 转写列表。
//! 录音/后处理都在工作线程,UI 永不阻塞。

use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Frame, PointerButton, Pos2, RichText, ScrollArea,
    TextEdit, ViewportCommand, WindowLevel,
};
use global_hotkey::{GlobalHotKeyEvent, HotKeyState};
use rusqlite::Connection;

use mrc_core::capture::{self, CaptureShared};
use mrc_core::config::Config;
use mrc_core::db;
use mrc_core::pipeline::{self, PipelineStop};
use mrc_core::transcribe::{Streaming, Transcriber};
use mrc_core::translate::Translator;
use mrc_core::{tools, windowcap};

use crate::i18n::{self, Lang};
use crate::subtitle::{self, SharedLive};
use crate::tray::{self, TrayEvent};

pub const BASE_SIZE: [f32; 2] = [132.0, 96.0]; // 同 pointtofixcraft 待机工具条
const TRANS_SIZE: [f32; 2] = [360.0, 470.0];

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Idle,
    Recording,
    Processing,
}

struct Session {
    meeting_id: i64,
    _capture: capture::Capture,
    shared: CaptureShared,
    stop_tx: mpsc::Sender<()>,
    done_rx: mpsc::Receiver<Result<()>>,
}

enum WorkerMsg {
    Pipeline {
        meeting_id: i64,
        outcome: std::result::Result<(String, Option<String>), WorkerFail>,
    },
}

enum WorkerFail {
    NeedNaming(Vec<String>),
    Fatal(String),
}

pub struct App {
    pub cfg: Config,
    conn: Connection,
    mode: Mode,
    live: SharedLive,
    session: Option<Session>,
    worker_tx: mpsc::Sender<WorkerMsg>,
    worker_rx: mpsc::Receiver<WorkerMsg>,

    show_settings: bool,
    show_transcript: bool,
    show_naming: bool,
    draft: Option<Draft>,
    naming: Option<NamingPanel>,
    notice: Option<(bool, String)>, // (是否错误, 文本)
    report_text: Option<String>,
    package_dir: Option<String>,
    prev_transcript_shown: bool,
    quitting: bool,
    tray: Option<crate::tray::Tray>,
    pf_job: Option<ToolJob>,
    lang: Lang,
}

/// 后台安装任务(下载/复制 pointtofixcraft)
struct ToolJob {
    rx: mpsc::Receiver<std::result::Result<std::path::PathBuf, String>>,
    status: ToolStatus,
}

enum ToolStatus {
    Running,
    Ok(std::path::PathBuf),
    Err(String),
}

impl ToolJob {
    fn start(src: Option<String>) -> Self {
        let cfg = Config::load().unwrap_or_default();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let res = mrc_core::tools::install(&cfg, src.as_deref())
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(res);
        });
        Self { rx, status: ToolStatus::Running }
    }
    fn poll(&mut self) {
        if matches!(self.status, ToolStatus::Running) {
            if let Ok(res) = self.rx.try_recv() {
                self.status = match res {
                    Ok(p) => ToolStatus::Ok(p),
                    Err(e) => ToolStatus::Err(e),
                };
            }
        }
    }
}

// ---------- 设置草稿(全 String 便于编辑) ----------

#[derive(Default)]
struct Draft {
    whisper_model: String,
    whisper_lang: String,
    whisper_window: String,
    diarize_enabled: bool,
    diarize_python: String,
    diarize_hf: String,
    llm_base: String,
    llm_key: String,
    llm_model: String,
    llm_schema: bool,
    llm_ctx: String,
    main_on_top: bool,
    sub_enabled: bool,
    sub_on_top: bool,
    sub_font: String,
    sub_opacity: String,
    sub_margin: String,
    shots_window: String,
    rag_base: String,
    chunk_window: String,
    chunk_tokens: String,
    pf_url: String,
    lang_code: String,
    tr_provider: String,
    tr_from: String,
    tr_to: String,
    tr_appid: String,
    tr_secret: String,
    tr_wanai_url: String,
    shots_default_win: String,
    shots_auto: String,
}

impl Draft {
    fn from_cfg(c: &Config) -> Self {
        Self {
            whisper_model: c.whisper.model_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
            whisper_lang: c.whisper.language.clone(),
            whisper_window: c.whisper.window_secs.to_string(),
            diarize_enabled: c.diarize.enabled,
            diarize_python: c.diarize.python.clone().unwrap_or_default(),
            diarize_hf: c.diarize.hf_token.clone().unwrap_or_default(),
            llm_base: c.llm.base_url.clone(),
            llm_key: c.llm.api_key.clone(),
            llm_model: c.llm.model.clone(),
            llm_schema: c.llm.json_schema,
            llm_ctx: c.llm.context_tokens.to_string(),
            main_on_top: c.main_on_top,
            sub_enabled: c.subtitle.enabled,
            sub_on_top: c.subtitle.always_on_top,
            sub_font: c.subtitle.font_size.to_string(),
            sub_opacity: c.subtitle.opacity.to_string(),
            sub_margin: c.subtitle.bottom_margin.to_string(),
            shots_window: c.shots.context_window_secs.to_string(),
            rag_base: c.rag.base_url.clone().unwrap_or_default(),
            chunk_window: c.post.chunk_window_secs.to_string(),
            chunk_tokens: c.post.chunk_token_limit.to_string(),
            pf_url: c.tools.pointtofix_url.clone().unwrap_or_default(),
            lang_code: c.lang.clone().unwrap_or_default(),
            tr_provider: c.translate.provider.clone(),
            tr_from: c.translate.from_lang.clone(),
            tr_to: c.translate.to_lang.clone(),
            tr_appid: c.translate.appid.clone(),
            tr_secret: c.translate.secret.clone(),
            tr_wanai_url: c.translate.wanai_url.clone(),
            shots_default_win: c.shots.default_window.clone(),
            shots_auto: c.shots.auto_window_secs.to_string(),
        }
    }

    fn parse<T: std::str::FromStr>(&self, s: &str, fallback: T) -> T {
        s.trim().parse().unwrap_or(fallback)
    }

    fn apply(self, old: &Config) -> Config {
        let mut c = old.clone();
        let p = self.whisper_model.trim();
        c.whisper.model_path = (!p.is_empty()).then(|| std::path::PathBuf::from(p));
        c.whisper.language = if self.whisper_lang.trim().is_empty() { "zh".into() } else { self.whisper_lang.trim().into() };
        c.whisper.window_secs = self.parse(&self.whisper_window, c.whisper.window_secs).max(4.0);
        c.diarize.enabled = self.diarize_enabled;
        c.diarize.python = (!self.diarize_python.trim().is_empty()).then(|| self.diarize_python.trim().into());
        c.diarize.hf_token = (!self.diarize_hf.trim().is_empty()).then(|| self.diarize_hf.trim().into());
        c.llm.base_url = self.llm_base.trim().to_string();
        c.llm.api_key = self.llm_key.trim().to_string();
        c.llm.model = if self.llm_model.trim().is_empty() { c.llm.model.clone() } else { self.llm_model.trim().into() };
        c.llm.json_schema = self.llm_schema;
        c.llm.context_tokens = self.parse(&self.llm_ctx, c.llm.context_tokens).max(4096);
        c.main_on_top = self.main_on_top;
        c.subtitle.enabled = self.sub_enabled;
        c.subtitle.always_on_top = self.sub_on_top;
        c.subtitle.font_size = self.parse(&self.sub_font, c.subtitle.font_size).clamp(12.0, 72.0);
        c.subtitle.opacity = self.parse(&self.sub_opacity, c.subtitle.opacity).clamp(0.1, 1.0);
        c.subtitle.bottom_margin = self.parse(&self.sub_margin, c.subtitle.bottom_margin).clamp(0.0, 400.0);
        c.shots.context_window_secs = self.parse(&self.shots_window, c.shots.context_window_secs).clamp(5.0, 300.0);
        c.rag.base_url = Some(self.rag_base.trim().to_string()).filter(|s| !s.is_empty());
        c.post.chunk_window_secs = self.parse(&self.chunk_window, c.post.chunk_window_secs).clamp(30.0, 1800.0);
        c.post.chunk_token_limit = self.parse(&self.chunk_tokens, c.post.chunk_token_limit).clamp(200, 100000);
        c.tools.pointtofix_url = Some(self.pf_url.trim().to_string()).filter(|s| !s.is_empty());
        c.lang = Some(self.lang_code.clone()).filter(|s| !s.is_empty());
        let prov = self.tr_provider.trim();
        c.translate.provider = match prov {
            "llm" | "baidu" | "wanai" => prov.to_string(),
            _ => "none".to_string(),
        };
        c.translate.wanai_url = if self.tr_wanai_url.trim().is_empty() {
            "http://127.0.0.1:17800".into()
        } else {
            self.tr_wanai_url.trim().into()
        };
        c.translate.from_lang = if self.tr_from.trim().is_empty() { "zh".into() } else { self.tr_from.trim().into() };
        c.translate.to_lang = if self.tr_to.trim().is_empty() { "en".into() } else { self.tr_to.trim().into() };
        c.translate.appid = self.tr_appid.trim().to_string();
        c.translate.secret = self.tr_secret.trim().to_string();
        c.shots.default_window = self.shots_default_win.trim().to_string();
        c.shots.auto_window_secs = self.parse(&self.shots_auto, c.shots.auto_window_secs).clamp(0.0, 600.0);
        c
    }
}

// ---------- 命名面板 ----------

struct NamingRow {
    key: String,
    input: String,
    dur_ms: i64,
    n_seg: usize,
    sample: String,
    proposed: Option<String>,
    sim: Option<f64>,
}

struct NamingPanel {
    meeting_id: i64,
    rows: Vec<NamingRow>,
    known_names: Vec<String>,
}

impl NamingPanel {
    fn load(conn: &Connection, meeting_id: i64) -> Result<Self> {
        let speakers = db::load_speakers(conn, meeting_id)?;
        let segs = db::load_segments(conn, meeting_id)?;
        let mut rows = Vec::new();
        for (key, name, proposed, sim) in &speakers {
            if name.is_some() {
                continue; // 只列未命名的
            }
            let of: Vec<&db::Segment> =
                segs.iter().filter(|s| s.speaker.as_deref() == Some(key.as_str())).collect();
            let dur_ms: i64 = of.iter().map(|s| s.end_ms - s.start_ms).sum();
            let sample = of
                .first()
                .map(|s| s.text.chars().take(42).collect::<String>())
                .unwrap_or_default();
            rows.push(NamingRow {
                key: key.clone(),
                input: proposed.clone().unwrap_or_default(),
                dur_ms,
                n_seg: of.len(),
                sample,
                proposed: proposed.clone(),
                sim: *sim,
            });
        }
        let known_names = {
            let mut st = conn.prepare("SELECT name FROM voiceprints ORDER BY updated_at DESC")?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            rows.flatten().take(12).collect()
        };
        Ok(Self { meeting_id, rows, known_names })
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, cfg: Config) -> Self {
        crate::fonts::load_cjk_font(&cc.egui_ctx);
        let lang = cfg.lang.as_deref().and_then(Lang::from_code).unwrap_or_else(Lang::detect);
        let tr = lang.strings();
        let tray = tray::init(&cc.egui_ctx, tr);
        let conn = db::open(&cfg.db_path()).expect("打开 meetings.db 失败");
        let (tx, rx) = mpsc::channel();
        Self {
            cfg,
            conn,
            mode: Mode::Idle,
            live: std::sync::Arc::new(std::sync::Mutex::new(subtitle::Live::default())),
            session: None,
            worker_tx: tx,
            worker_rx: rx,
            show_settings: false,
            show_transcript: false,
            show_naming: false,
            draft: None,
            naming: None,
            notice: Some((false, lang.strings().hint_start.into())),
            report_text: None,
            package_dir: None,
            prev_transcript_shown: false,
            quitting: false,
            tray,
            pf_job: None,
            lang,
        }
    }

    fn toggle_meeting(&mut self) {
        match self.mode {
            Mode::Idle => self.start_meeting(),
            Mode::Recording => self.stop_meeting(),
            Mode::Processing => {} // 后处理中不允许
        }
    }

    fn start_meeting(&mut self) {
        if let Err(e) = self.try_start_meeting() {
            let tr = self.lang.strings();
            self.notice = Some((true, i18n::fill(tr.err_start_fail, &[&format!("{e:#}")])));
        }
    }

    fn try_start_meeting(&mut self) -> Result<()> {
        let now = db::now_ms();
        let title = format!("{} {}", self.lang.strings().app_title, db::fmt_local(now));
        let meeting_id = db::insert_meeting(&self.conn, &title, now)?;
        let audio_path = self.cfg.audio_dir().join(format!("meeting_{meeting_id}.wav"));
        let (cap, shared) = capture::open(&audio_path, |e| eprintln!("[capture] {e}"))
            .context(self.lang.strings().err_capture)?;

        {
            let mut g = self.live.lock().expect("live 锁");
            g.segments.clear();
            g.started_at_ms = now;
            g.error = None;
        }

        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<Result<()>>();
        let cfg2 = self.cfg.clone();
        let live2 = std::sync::Arc::clone(&self.live);
        let shared2 = shared.clone();
        let translator = Translator::new(&cfg2.translate, &cfg2.llm).ok();
        // 自动截图(指定窗口,防漏截):间隔 0=关;pointtofix 缺失时静默跳过
        let auto_shots = cfg2.shots.auto_window_secs > 0.0 && !cfg2.shots.default_window.trim().is_empty();
        let shot_every = std::time::Duration::from_secs_f64(cfg2.shots.auto_window_secs.max(1.0));
        let mut last_shot = std::time::Instant::now() - shot_every; // 首个 tick 立即截一张
        let pf_exe = tools::resolve_available(&cfg2);
        std::thread::spawn(move || {
            // 模型加载放线程里(几秒),WAV 归档不等人
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
                            let mut new_texts: Vec<String> = Vec::new();
                            let mut new_idx: Vec<i64> = Vec::new();
                            let mut new_start: Vec<i64> = Vec::new();
                            for s in &segs {
                                db::insert_segment(&conn, meeting_id, idx, s.start_ms, s.end_ms, None, &s.text)?;
                                new_idx.push(idx);
                                new_texts.push(s.text.clone());
                                new_start.push(s.start_ms);
                                idx += 1;
                                subtitle::push_line(&live2, s.start_ms, "", &s.text, "");
                            }
                            // 实时翻译(未启用/失败静默,原文照常)
                            if let Some(tr) = translator.as_ref() {
                                let outs = tr.translate_batch(&new_texts);
                                for (k, o) in outs.iter().enumerate() {
                                    if let Some(v) = o {
                                        let _ = db::update_translation(&conn, new_idx[k], meeting_id, v);
                                        if let Ok(mut g) = live2.lock() {
                                            if let Some(e) = g.segments.iter_mut().rev().find(|e| e.0 == new_start[k] && e.3.is_empty()) {
                                                e.3 = v.clone();
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        mrc_core::transcribe::StreamEvent::Error(e) => {
                            if let Ok(mut g) = live2.lock() {
                                g.error = Some(e);
                            }
                        }
                        mrc_core::transcribe::StreamEvent::Idle => {}
                    }
                    // 自动窗口截图(失败静默,只打印一次)
                    if auto_shots && last_shot.elapsed() >= shot_every {
                        last_shot = std::time::Instant::now();
                        if let (Some(exe), Ok(info)) = (pf_exe.as_ref(), windowcap::find_window(&cfg2.shots.default_window)) {
                            match windowcap::capture_window(exe, &info, false) {
                                Ok(r) => eprintln!("[shots] 窗口截图 #{} {}x{}", r.id.unwrap_or(0), info.w, info.h),
                                Err(e) => eprintln!("[shots] 自动截图失败: {e:#}"),
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(2000));
                }
                // 收尾残余
                if let mrc_core::transcribe::StreamEvent::Segments(segs) = streaming.tick(&shared2) {
                    for s in &segs {
                        db::insert_segment(&conn, meeting_id, idx, s.start_ms, s.end_ms, None, &s.text)?;
                        subtitle::push_line(&live2, s.start_ms, "", &s.text, "");
                    }
                }
                Ok(())
            })();
            let _ = done_tx.send(res);
        });

        self.session = Some(Session {
            meeting_id,
            _capture: cap,
            shared,
            stop_tx,
            done_rx,
        });
        self.mode = Mode::Recording;
        self.report_text = None;
        self.package_dir = None;
        let tr = self.lang.strings();
        self.notice = Some((false, format!("#{} {}", meeting_id, tr.hint_recording)));
        Ok(())
    }

    fn stop_meeting(&mut self) {
        let Some(s) = self.session.take() else { return };
        self.mode = Mode::Processing;
        self.notice = Some((false, self.lang.strings().hint_finalizing.into()));
        let tx = self.worker_tx.clone();
        let cfg = self.cfg.clone();
        let live = std::sync::Arc::clone(&self.live);
        std::thread::spawn(move || {
            let _ = s.stop_tx.send(());
            if let Ok(Err(e)) = s.done_rx.recv_timeout(Duration::from_secs(30)) {
                eprintln!("[warn] 转写线程异常: {e:#}");
            }
            if let Err(e) = s.shared.finalize() {
                eprintln!("[warn] WAV 收尾失败: {e:#}");
            }
            let ended = db::now_ms();
            let outcome = (|| -> std::result::Result<(String, Option<String>), WorkerFail> {
                let mut conn = db::open(&cfg.db_path()).map_err(|e| WorkerFail::Fatal(e.to_string()))?;
                db::finish_meeting(&conn, s.meeting_id, ended, &format!("audio\\meeting_{}.wav", s.meeting_id))
                    .map_err(|e| WorkerFail::Fatal(e.to_string()))?;
                match pipeline::run(&cfg, &mut conn, s.meeting_id, false, false, false) {
                    Ok(r) => Ok((format!("{r}"), r.package_dir.clone())),
                    Err((_, Some(PipelineStop::NamingNeeded(keys)))) => Err(WorkerFail::NeedNaming(keys)),
                    Err((e, None)) => Err(WorkerFail::Fatal(format!("{e:#}"))),
                }
            })();
            if let Ok(mut g) = live.lock() {
                g.started_at_ms = 0;
            }
            let _ = tx.send(WorkerMsg::Pipeline { meeting_id: s.meeting_id, outcome });
        });
    }

    /// 手动截指定窗口(默认窗口标题取配置;后台线程,不卡 UI)
    fn shot_window_now(&mut self) {
        let cfg = self.cfg.clone();
        let kw = cfg.shots.default_window.trim().to_string();
        if kw.is_empty() {
            self.notice = Some((true, self.lang.strings().hint_shot_no_window.into()));
            return;
        }
        let tx = self.worker_tx.clone();
        let tr_done = self.lang.strings().hint_shot_ok;
        let tr_fail = self.lang.strings().hint_shot_fail;
        std::thread::spawn(move || {
            let outcome = (|| -> anyhow::Result<String> {
                let exe = tools::resolve_available(&cfg)
                    .ok_or_else(|| anyhow::anyhow!("pointtofixcraft 未安装"))?;
                let info = windowcap::find_window(&kw)?;
                let r = windowcap::capture_window(&exe, &info, false)?;
                Ok(format!("#{} ({}x{})", r.id.unwrap_or(0), info.w, info.h))
            })();
            let _ = tx.send(WorkerMsg::Pipeline {
                meeting_id: -1,
                outcome: match outcome {
                    Ok(msg) => Ok((i18n::fill(tr_done, &[&msg]), None)),
                    Err(e) => Err(WorkerFail::Fatal(format!("{}: {e:#}", tr_fail))),
                },
            });
        });
    }

    fn quit(&mut self) {
        if self.session.is_some() {
            self.quitting = true; // 等后处理线程回报再退
            self.stop_meeting();
        } else {
            std::process::exit(0);
        }
    }

    fn open_naming(&mut self, meeting_id: i64, keys: &[String]) {
        match NamingPanel::load(&self.conn, meeting_id) {
            Ok(np) => {
                self.naming = Some(np);
                self.show_naming = true;
                self.notice = Some((false, format!("{}: {:?}", self.lang.strings().hint_need_naming, keys)));
            }
            Err(e) => self.notice = Some((true, i18n::fill(self.lang.strings().err_naming_load, &[&format!("{e:#}")]))),
        }
    }
}

fn spawn_pipeline(tx: mpsc::Sender<WorkerMsg>, cfg: Config, meeting_id: i64) {
    std::thread::spawn(move || {
        let outcome = (|| -> std::result::Result<pipeline::Report, (anyhow::Error, Option<PipelineStop>)> {
            match db::open(&cfg.db_path()) {
                Ok(mut conn) => pipeline::run(&cfg, &mut conn, meeting_id, false, false, false),
                Err(e) => Err((e, None)),
            }
        })();
        let msg = match outcome {
            Ok(r) => WorkerMsg::Pipeline {
                meeting_id,
                outcome: Ok((format!("{r}"), r.package_dir.clone())),
            },
            Err((_, Some(PipelineStop::NamingNeeded(keys)))) => WorkerMsg::Pipeline {
                meeting_id,
                outcome: Err(WorkerFail::NeedNaming(keys)),
            },
            Err((e, None)) => WorkerMsg::Pipeline {
                meeting_id,
                outcome: Err(WorkerFail::Fatal(format!("{e:#}"))),
            },
        };
        let _ = tx.send(msg);
    });
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ---- 事件源:托盘 / 热键 / 工作线程 ----
        if let Some(t) = &self.tray {
            for ev in t.drain() {
                match ev {
                    TrayEvent::Toggle => self.toggle_meeting(),
                    TrayEvent::Settings => {
                        if self.draft.is_none() {
                            self.draft = Some(Draft::from_cfg(&self.cfg));
                        }
                        self.show_settings = !self.show_settings;
                    }
                    TrayEvent::ToggleSubtitle => self.cfg.subtitle.enabled = !self.cfg.subtitle.enabled,
                    TrayEvent::Exit => self.quit(),
                }
            }
        }
        if let Some(job) = self.pf_job.as_mut() {
            let before_running = matches!(job.status, ToolStatus::Running);
            job.poll();
            if before_running && !matches!(job.status, ToolStatus::Running) {
                let tr = self.lang.strings();
                match &job.status {
                    ToolStatus::Ok(p) => self.notice = Some((false, i18n::fill(tr.pf_installed, &[&p.display().to_string()]))),
                    ToolStatus::Err(e) => self.notice = Some((true, i18n::fill(tr.tool_fail, &[&e]))),
                    ToolStatus::Running => {}
                }
            }
        }
        while let Ok(ev) = GlobalHotKeyEvent::receiver().try_recv() {
            if ev.state == HotKeyState::Pressed {
                self.toggle_meeting();
            }
        }
        while let Ok(msg) = self.worker_rx.try_recv() {
            match msg {
                WorkerMsg::Pipeline { meeting_id, outcome } => match outcome {
                    Ok((text, pkg)) => {
                        self.mode = Mode::Idle;
                        self.session = None;
                        self.report_text = Some(text.clone());
                        self.package_dir = pkg;
                        self.notice = Some((false, format!("#{} {}", meeting_id, self.lang.strings().hint_done)));
                        if self.quitting {
                            std::process::exit(0);
                        }
                    }
                    Err(WorkerFail::NeedNaming(keys)) => {
                        self.mode = Mode::Idle;
                        self.session = None;
                        self.open_naming(meeting_id, &keys);
                        if self.quitting {
                            // 命名是交互,不自动退;清标志等用户操作
                            self.quitting = false;
                        }
                    }
                    Err(WorkerFail::Fatal(e)) => {
                        self.mode = Mode::Idle;
                        self.session = None;
                        let tr = self.lang.strings();
                        self.notice = Some((true, i18n::fill(tr.err_pipeline_fail, &[&e])));
                        if self.quitting {
                            std::process::exit(1);
                        }
                    }
                },
            }
        }

        // 主窗置顶(每帧发一次,幂等)
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(if self.cfg.main_on_top {
            WindowLevel::AlwaysOnTop
        } else {
            WindowLevel::Normal
        }));
        // 转写面板尺寸切换
        if self.show_transcript != self.prev_transcript_shown {
            let [w, h] = if self.show_transcript { TRANS_SIZE } else { BASE_SIZE };
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(w, h)));
            self.prev_transcript_shown = self.show_transcript;
        }

        paint_panel(self, ctx);

        if self.show_naming {
            paint_naming(self, ctx);
        }
        if self.show_settings {
            paint_settings(self, ctx);
        }

        // ---- 字幕条视口(录音中才显示;不渲染即自动关闭) ----
        if self.cfg.subtitle.enabled && self.mode == Mode::Recording {
            subtitle::show(ctx, &self.live, &self.cfg.subtitle);
        }

        ctx.request_repaint_after(Duration::from_millis(400));
    }
}

// ---------- 主面板绘制 ----------

fn paint_panel(app: &mut App, ctx: &egui::Context) {
    // 快照(闭包内避免借用冲突)
    let mode = app.mode;
    let (elapsed, lines, err_line) = {
        let g = app.live.lock().expect("live 锁");
        let elapsed = if g.started_at_ms > 0 {
            let t = ((chrono_now_ms() - g.started_at_ms) / 1000).max(0);
            format!("{:02}:{:02}:{:02}", t / 3600, (t % 3600) / 60, t % 60)
        } else {
            String::new()
        };
        (elapsed, g.segments.iter().rev().take(200).cloned().collect::<Vec<_>>(), g.error.clone())
    };
    let notice = app.notice.clone();
    let report = app.report_text.clone();
    let package = app.package_dir.clone();

    egui::CentralPanel::default()
        .frame(Frame::new().fill(Color32::TRANSPARENT))
        .show(ctx, |ui| {
            let rect = ui.max_rect();
            ui.painter().rect_filled(
                rect,
                CornerRadius::same(12),
                Color32::from_rgba_unmultiplied(24, 26, 34, 238),
            );

            // 行1:状态点 + 状态/计时 + ✕(整行拖拽)
            let header = rect_from(rect.min + egui::vec2(8.0, 6.0), egui::vec2(rect.width() - 16.0, 20.0));
            let resp = ui.allocate_rect(header, egui::Sense::click_and_drag());
            if resp.dragged_by(PointerButton::Primary) {
                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
            }
            let tr = app.lang.strings();
            let (dot, title) = match mode {
                Mode::Idle => (Color32::from_rgb(120, 130, 150), tr.app_title.to_string()),
                Mode::Recording => (Color32::from_rgb(232, 72, 72), if elapsed.is_empty() { tr.st_recording.into() } else { elapsed.clone() }),
                Mode::Processing => (Color32::from_rgb(240, 180, 60), tr.st_processing.to_string()),
            };
            ui.painter().circle_filled(Pos2::new(header.left() + 6.0, header.center().y), 4.0, dot);
            ui.painter().text(
                Pos2::new(header.left() + 16.0, header.center().y),
                Align2::LEFT_CENTER,
                title,
                FontId::proportional(13.0),
                Color32::from_rgb(225, 228, 235),
            );
            let r_close = head_btn(ui, header.right() - 8.0, header.center().y, "✕");
            let r_shot = if mode == Mode::Recording {
                Some(head_btn(ui, header.right() - 34.0, header.center().y, "📷"))
            } else {
                None
            };
            if ui.input(|i| i.pointer.primary_released()) {
                let p = ui.input(|i| i.pointer.interact_pos().unwrap_or_default());
                if r_close.contains(p) {
                    app.quit();
                } else if let Some(rs) = r_shot {
                    if rs.contains(p) {
                        app.shot_window_now();
                    }
                }
            }

            // 行2:主按钮 + ⚙ + ≡(录音中才有打开会议包;放不下,收进 ≡ 展开视图)
            ui.allocate_new_ui(
                egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                    rect.min + egui::vec2(0.0, 30.0),
                    egui::vec2(rect.width(), 34.0),
                )),
                |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        let tr = app.lang.strings();
                        let (label, fill) = match mode {
                            Mode::Idle => (tr.btn_start_s, Color32::from_rgb(34, 139, 84)),
                            Mode::Recording => (tr.btn_stop_s, Color32::from_rgb(178, 60, 60)),
                            Mode::Processing => (tr.btn_processing, Color32::from_rgb(90, 96, 110)),
                        };
                        let btn = egui::Button::new(RichText::new(label).size(14.0).color(Color32::WHITE))
                            .fill(fill)
                            .min_size(egui::vec2(72.0, 30.0))
                            .corner_radius(CornerRadius::same(7));
                        if ui.add_enabled(mode != Mode::Processing, btn).clicked() {
                            app.toggle_meeting();
                        }
                        if ui.add(egui::Button::new(RichText::new("⚙").size(13.0))).clicked() {
                            if app.draft.is_none() {
                                app.draft = Some(Draft::from_cfg(&app.cfg));
                            }
                            app.show_settings = true;
                        }
                        if ui.add(egui::Button::new(RichText::new("≡").size(13.0))).clicked() {
                            app.show_transcript = !app.show_transcript;
                        }
                    });
                },
            );

            // 行3(紧凑):一行小字提示/错误
            ui.allocate_new_ui(
                egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                    rect.min + egui::vec2(10.0, 68.0),
                    egui::vec2(rect.width() - 20.0, 24.0),
                )),
                |ui| {
                    let line = err_line
                        .map(|e| format!("{}: {}", app.lang.strings().tr_fail_line, e.chars().take(26).collect::<String>()))
                        .or(notice.map(|(_, t)| t.chars().take(30).collect::<String>()));
                    if let Some(t) = line {
                        ui.painter().text(
                            Pos2::new(ui.max_rect().left(), ui.max_rect().top() + 8.0),
                            Align2::LEFT_CENTER,
                            t,
                            FontId::proportional(10.0),
                            Color32::from_rgb(140, 146, 158),
                        );
                    }
                },
            );

            // 展开视图(≡):转写 + 报告 + 打开会议包
            if app.show_transcript {
                ui.allocate_new_ui(
                    egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                        rect.min + egui::vec2(10.0, 96.0),
                        egui::vec2(rect.width() - 20.0, rect.height() - 102.0),
                    )),
                    |ui| {
                        if let Some(t) = &report {
                            ui.label(RichText::new(t).size(11.0).color(Color32::from_rgb(140, 200, 160)));
                        }
                        if let Some(pkg) = &package {
                            let tr = app.lang.strings();
                            if mode != Mode::Processing && ui.add(egui::Button::new(tr.open_package)).clicked() {
                                let _ = std::process::Command::new("explorer").arg(pkg).spawn();
                            }
                        }
                        ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                            for (rel_ms, spk, text, trn) in lines.iter().rev() {
                                let t = (rel_ms / 1000).max(0);
                                let ts = format!("{:02}:{:02}:{:02}", t / 3600, (t % 3600) / 60, t % 60);
                                ui.label(
                                    RichText::new(format!("[{ts}] {spk} {text}"))
                                        .size(12.5)
                                        .color(Color32::from_rgb(210, 214, 222)),
                                );
                                if !trn.trim().is_empty() {
                                    ui.label(RichText::new(format!("      {trn}")).size(11.0).color(Color32::from_rgb(150, 170, 200)));
                                }
                            }
                        });
                    },
                );
            }
        });
}

fn rect_from(min: Pos2, size: egui::Vec2) -> egui::Rect {
    egui::Rect::from_min_size(min, size)
}

fn head_btn(ui: &mut egui::Ui, cx: f32, cy: f32, label: &str) -> egui::Rect {
    let r = egui::Rect::from_center_size(Pos2::new(cx, cy), egui::vec2(24.0, 22.0));
    let hov = ui.allocate_rect(r, egui::Sense::hover()).hovered();
    ui.painter().rect_filled(
        r,
        CornerRadius::same(5),
        if hov { Color32::from_rgba_unmultiplied(255, 255, 255, 26) } else { Color32::TRANSPARENT },
    );
    ui.painter().text(
        r.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(13.0),
        Color32::from_rgb(190, 196, 208),
    );
    r
}

fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------- 命名窗 ----------

fn paint_naming(app: &mut App, ctx: &egui::Context) {
    // 全部快照,窗口闭包里只动局部副本
    let tr = app.lang.strings();
    let (meeting_id, known, snapshot) = match app.naming.as_ref() {
        Some(np) => (
            np.meeting_id,
            np.known_names.clone(),
            np.rows
                .iter()
                .map(|r| (r.key.clone(), r.input.clone(), r.dur_ms, r.n_seg, r.sample.clone(), r.proposed.clone(), r.sim))
                .collect::<Vec<_>>(),
        ),
        None => return,
    };
    let mut open = app.show_naming;
    let mut inputs = snapshot.iter().map(|(_, v, ..)| v.clone()).collect::<Vec<_>>();
    let mut confirmed = false;
    let mut deferred = false;

    egui::Window::new(tr.naming_title)
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.label(
                RichText::new(tr.naming_hint)
                    .size(12.0)
                    .color(Color32::from_rgb(150, 158, 172)),
            );
            ui.add_space(4.0);
            ScrollArea::vertical().max_height(340.0).show(ui, |ui| {
                for (i, (key, _, dur_ms, n_seg, sample, proposed, sim)) in snapshot.iter().enumerate() {
                    let mins = dur_ms / 60000;
                    let secs = (dur_ms % 60000) / 1000;
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(key).strong().size(14.0));
                        ui.label(
                            RichText::new(i18n::fill(tr.spoken_stat, &[&mins.to_string(), &secs.to_string(), &n_seg.to_string()]))
                                .size(11.5)
                                .color(Color32::from_rgb(150, 158, 172)),
                        );
                    });
                    match (proposed, sim) {
                        (Some(p), Some(s)) => {
                            ui.label(RichText::new(i18n::fill(tr.vp_prefill, &[p, &format!("{s:.2}")])).size(11.5).color(Color32::from_rgb(140, 200, 160)));
                        }
                        (Some(p), None) => {
                            ui.label(RichText::new(i18n::fill(tr.prefill, &[p])).size(11.5).color(Color32::from_rgb(140, 200, 160)));
                        }
                        _ => {}
                    }
                    ui.label(RichText::new(i18n::fill(tr.sample, &[sample])).size(11.5).color(Color32::from_rgb(170, 176, 188)));
                    ui.add(
                        TextEdit::singleline(&mut inputs[i])
                            .hint_text(tr.input_name)
                            .desired_width(240.0),
                    );
                    if !known.is_empty() {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(tr.history).size(11.0).color(Color32::from_rgb(150, 158, 172)));
                            for kn in &known {
                                if ui.add(egui::Button::new(RichText::new(kn).size(11.0))).clicked() {
                                    inputs[i] = kn.clone();
                                }
                            }
                        });
                    }
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(tr.confirm_naming).min_size(egui::vec2(150.0, 28.0))).clicked() {
                    confirmed = true;
                }
                if ui.add(egui::Button::new(tr.btn_later)).clicked() {
                    deferred = true;
                }
            });
        });

    app.show_naming = open;
    if confirmed {
        let mid = meeting_id;
        let mut named = 0;
        for (i, (key, ..)) in snapshot.iter().enumerate() {
            let name = inputs[i].trim();
            if !name.is_empty() && db::upsert_speaker(&app.conn, mid, key, Some(name), None, None).is_ok() {
                named += 1;
            }
        }
        app.naming = None;
        app.show_naming = false;
        app.mode = Mode::Processing;
        app.notice = Some((false, i18n::fill(app.lang.strings().hint_named_continue, &[&named.to_string()])));
        spawn_pipeline(app.worker_tx.clone(), app.cfg.clone(), meeting_id);
    }
    if deferred {
        app.naming = None;
        app.show_naming = false;
        app.notice = Some((false, format!("{}: mrc-postprocess run {meeting_id}", i18n::fill(app.lang.strings().hint_later, &[&meeting_id.to_string()]))));
    }
}

// ---------- 设置窗 ----------

fn paint_settings(app: &mut App, ctx: &egui::Context) {
    let mut open = app.show_settings;
    let mut save_clicked = false;
    let mut cancel_clicked = false;

    let tr = app.lang.strings();
    egui::Window::new(tr.set_title)
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
            let Some(d) = app.draft.as_mut() else { return };
            ScrollArea::vertical().max_height(430.0).show(ui, |ui| {
                ui.heading(tr.sec_whisper);
                row(ui, tr.lbl_model, &mut d.whisper_model, tr.hint_model);
                row(ui, tr.lbl_language, &mut d.whisper_lang, "zh / en / auto");
                row(ui, tr.lbl_window, &mut d.whisper_window, "12");
                ui.separator();
                ui.heading(tr.sec_diarize);
                ui.checkbox(&mut d.diarize_enabled, tr.lbl_diarize_on);
                row(ui, tr.lbl_python, &mut d.diarize_python, "");
                row(ui, tr.lbl_hf, &mut d.diarize_hf, tr.hint_hf);
                ui.separator();
                ui.heading(tr.sec_llm);
                row(ui, tr.lbl_base, &mut d.llm_base, tr.hint_base);
                row(ui, tr.lbl_key, &mut d.llm_key, "");
                row(ui, tr.lbl_llm_model, &mut d.llm_model, tr.hint_llm_model);
                ui.checkbox(&mut d.llm_schema, tr.lbl_schema);
                row(ui, tr.lbl_ctx, &mut d.llm_ctx, "200000");
                ui.separator();
                ui.heading(tr.sec_subtitle);
                ui.checkbox(&mut d.sub_enabled, tr.lbl_sub_on);
                ui.checkbox(&mut d.sub_on_top, tr.lbl_sub_top);
                row(ui, tr.lbl_sub_font, &mut d.sub_font, "26");
                row(ui, tr.lbl_sub_opacity, &mut d.sub_opacity, "0.55");
                row(ui, tr.lbl_sub_margin, &mut d.sub_margin, "48");
                ui.separator();
                ui.heading(tr.sec_window_post);
                ui.checkbox(&mut d.main_on_top, tr.lbl_main_top);
                row(ui, tr.lbl_shots_window, &mut d.shots_window, "30");
                row(ui, tr.lbl_chunk_window, &mut d.chunk_window, "300");
                row(ui, tr.lbl_chunk_tokens, &mut d.chunk_tokens, "1000");
                row(ui, tr.lbl_rag, &mut d.rag_base, tr.hint_rag);
                ui.separator();
                ui.heading(tr.sec_translate);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tr.lbl_tr_provider).size(12.5));
                    let mut prov = d.tr_provider.clone();
                    egui::ComboBox::from_id_salt("tr-provider")
                        .selected_text(match prov.as_str() {
                            "llm" => "LLM",
                            "baidu" => "Baidu",
                            "wanai" => "WanAI (local)",
                            _ => "off",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut prov, "none".to_string(), "off");
                            ui.selectable_value(&mut prov, "llm".to_string(), "LLM");
                            ui.selectable_value(&mut prov, "baidu".to_string(), "Baidu");
                            ui.selectable_value(&mut prov, "wanai".to_string(), "WanAI (local)");
                        });
                    d.tr_provider = prov;
                });
                row(ui, tr.lbl_tr_from, &mut d.tr_from, "zh");
                row(ui, tr.lbl_tr_to, &mut d.tr_to, "en");
                if d.tr_provider == "wanai" {
                    row(ui, tr.lbl_wanai_url, &mut d.tr_wanai_url, tr.hint_wanai_url);
                }
                if d.tr_provider == "baidu" {
                    row(ui, tr.lbl_tr_appid, &mut d.tr_appid, "");
                    row(ui, tr.lbl_tr_secret, &mut d.tr_secret, "");
                }
                ui.separator();
                ui.heading(tr.sec_shots);
                row(ui, tr.lbl_shots_default, &mut d.shots_default_win, tr.hint_shots_default);
                row(ui, tr.lbl_auto_shots, &mut d.shots_auto, "0");
                ui.separator();
                ui.heading(tr.sec_tools);
                let pf_state = {
                    let installed = mrc_core::tools::is_installed(&app.cfg).map(|p| p.display().to_string());
                    let sibling = installed.is_none().then(|| mrc_core::tools::resolve_available(&app.cfg)).flatten();
                    match (installed, sibling) {
                        (Some(p), _) => i18n::fill(tr.pf_installed, &[&p]),
                        (None, Some(p)) => i18n::fill(tr.pf_sibling, &[&p.display().to_string()]),
                        (None, None) => tr.pf_missing.to_string(),
                    }
                };
                ui.label(RichText::new(&pf_state).size(12.0).color(Color32::from_rgb(150, 158, 172)));
                row(ui, tr.lbl_pf_src, &mut d.pf_url, tr.hint_pf);
                // 界面语言(保存后生效;托盘文案需重启)
                let mut lang_code = app.cfg.lang.clone().unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tr.lbl_lang_ui).size(12.5));
                    egui::ComboBox::from_id_salt("ui-lang")
                        .selected_text(match lang_code.as_str() {
                            "en" => "English",
                            "de" => "Deutsch",
                            "ja" => "日本語",
                            _ => "中文",
                        })
                        .show_ui(ui, |ui| {
                            for l in Lang::ALL {
                                ui.selectable_value(&mut lang_code, l.code().to_string(), l.native_name());
                            }
                        });
                });
                d.lang_code = lang_code;
                ui.horizontal(|ui| {
                    let installing = app.pf_job.is_some();
                    let label = if installing { tr.btn_installing } else { tr.btn_install };
                    if ui.add_enabled(!installing, egui::Button::new(label)).clicked() {
                        let src = d.pf_url.trim().to_string();
                        let src = (!src.is_empty()).then_some(src);
                        app.pf_job = Some(ToolJob::start(src));
                    }
                    if ui.button(tr.btn_launch_tool).clicked() {
                        let tr2 = app.lang.strings();
                        match mrc_core::tools::launch(&app.cfg) {
                            Ok(p) => app.notice = Some((false, i18n::fill(tr2.tool_launch_ok, &[&p.display().to_string()]))),
                            Err(e) => app.notice = Some((true, format!("{e:#}"))),
                        }
                    }
                });
                if let Some(job) = &app.pf_job {
                    match &job.status {
                        ToolStatus::Running => { ui.spinner(); ui.label(RichText::new(tr.tool_running).size(11.5)); }
                        ToolStatus::Ok(p) => { ui.label(RichText::new(i18n::fill(tr.tool_done, &[&p.display().to_string()])).size(11.5).color(Color32::from_rgb(140,200,160))); }
                        ToolStatus::Err(e) => { ui.label(RichText::new(e).size(11.5).color(Color32::from_rgb(240,120,120))); }
                    }
                }
            }); // ScrollArea
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(tr.btn_save).min_size(egui::vec2(90.0, 26.0))).clicked() {
                    save_clicked = true;
                }
                if ui.add(egui::Button::new(tr.btn_cancel)).clicked() {
                    cancel_clicked = true;
                }
            });
        });

    app.show_settings = open;
    if cancel_clicked {
        app.draft = None;
        app.show_settings = false;
    }
    if save_clicked {
        if let Some(d) = app.draft.take() {
            let new_cfg = d.apply(&app.cfg);
            let text = match toml::to_string_pretty(&new_cfg) {
                Ok(t) => t,
                Err(e) => {
                    app.notice = Some((true, i18n::fill(app.lang.strings().err_cfg_ser, &[&e.to_string()])));
                    return;
                }
            };
            let path = Config::config_path();
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match std::fs::write(&path, text) {
                Ok(()) => {
                    let top_changed = new_cfg.main_on_top != app.cfg.main_on_top;
                    app.cfg = new_cfg;
                    app.lang = app.cfg.lang.as_deref().and_then(Lang::from_code).unwrap_or(app.lang);
                    app.show_settings = false;
                    app.notice = Some((false, i18n::fill(app.lang.strings().hint_cfg_saved, &[&path.display().to_string()])));
                    if top_changed {
                        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(if app.cfg.main_on_top {
                            WindowLevel::AlwaysOnTop
                        } else {
                            WindowLevel::Normal
                        }));
                    }
                }
                Err(e) => app.notice = Some((true, i18n::fill(app.lang.strings().err_cfg_write, &[&e.to_string()]))),
            }
        }
    }
}

fn row(ui: &mut egui::Ui, label: &str, val: &mut String, hint: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(12.5)).on_hover_text(hint);
        ui.add(TextEdit::singleline(val).desired_width(230.0).hint_text(hint));
    });
}
