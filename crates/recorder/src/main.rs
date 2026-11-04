//! mrc-recorder — 常驻端 GUI:悬浮小面板 + 屏幕底部字幕 + 设置/命名窗。
//!
//! F9 全局热键 开始/结束会议;F8 留给 pointtofixcraft 截图,本工具不碰。
//! release 版隐藏控制台(同 pointtofixcraft);debug 保留控制台看日志。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cli;
mod i18n;
mod fonts;
mod screen;
mod subtitle;
mod tray;

use eframe::egui;

fn load_icon() -> egui::IconData {
    let img = image::load_from_memory(include_bytes!("tray_icon.png"))
        .expect("tray_icon.png 解码失败")
        .into_rgba8();
    let (width, height) = img.dimensions();
    egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    }
}

fn main() -> eframe::Result<()> {
    // 无头 CLI:--start / --status / --tool-install / --tool-launch(同 pointtofixcraft --capture)
    match cli::parse(std::env::args().skip(1)) {
        cli::CliAction::Bad(msg) => {
            std::process::exit(cli::run(cli::CliAction::Bad(msg)));
        }
        cli::CliAction::Gui => {}
        other => std::process::exit(cli::run(other)),
    }

    let cfg = mrc_core::config::Config::load().unwrap_or_default();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("会议记录")
            .with_inner_size(app::BASE_SIZE)
            .with_resizable(false)
            .with_decorations(false)
            .with_transparent(true)
            .with_window_level(if cfg.main_on_top {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            })
            .with_icon(std::sync::Arc::new(load_icon())),
        ..Default::default()
    };
    // F9 全局热键
    let hk_mgr = global_hotkey::GlobalHotKeyManager::new().ok();
    if let Some(m) = &hk_mgr {
        let _ = m.register(global_hotkey::hotkey::HotKey::new(
            None,
            global_hotkey::hotkey::Code::F9,
        ));
    }
    eframe::run_native(
        "mrc-recorder",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc, cfg)))),
    )
}
