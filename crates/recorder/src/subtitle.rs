//! 屏幕底部字幕条:第二个原生视口(show_viewport_immediate),
//! 无边框+半透明,显示最近 2 条转写 + 录音红点 + 时长。

use std::sync::{Arc, Mutex};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, ViewportBuilder, ViewportId, WindowLevel,
};

use crate::screen;

/// 实时链路共享状态(转写线程写,UI/字幕条读)
#[derive(Default)]
pub struct Live {
    /// 最近转写段:(会议相对毫秒, 说话人显示名, 文本),最多保留 200 条
    pub segments: Vec<(i64, String, String)>,
    pub started_at_ms: i64,
    pub error: Option<String>,
}

pub type SharedLive = Arc<Mutex<Live>>;

fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn push_line(live: &SharedLive, rel_ms: i64, speaker: &str, text: &str) {
    let mut g = live.lock().expect("live 锁");
    g.segments.push((rel_ms, speaker.to_string(), text.to_string()));
    if g.segments.len() > 200 {
            let n = g.segments.len();
        g.segments.drain(..n - 200);
    }
}

/// 画字幕视口。每帧在 update() 里调用;不满足显示条件时直接不调用,
/// 视口随之自动关闭。
pub fn show(
    ctx: &egui::Context,
    live: &SharedLive,
    cfg: &mrc_core::config::SubtitleCfg,
) {
    let last: Vec<(String, String)> = {
        let g = live.lock().expect("live 锁");
        g.segments
            .iter()
            .rev()
            .take(2)
            .rev()
            .map(|(_, spk, text)| {
                let line = if spk.is_empty() {
                    text.clone()
                } else {
                    format!("{spk}:{text}")
                };
                (spk.clone(), line)
            })
            .collect()
    };
    if last.is_empty() {
        return;
    }
    let started = {
        let g = live.lock().expect("live 锁");
        g.started_at_ms
    };

    let (sw, sh) = screen::primary_screen();
    // 物理像素 → 逻辑(96dpi 基准;主屏缩放非 100% 时由 DPI 感知换算,误差可接受)
    let scale = ctx.pixels_per_point();
    let sub_w = (sw as f32 / scale * 0.72).min(1200.0);
    let sub_h = cfg.font_size * 2.2 + 60.0;
    let x = sw as f32 / scale / 2.0 - sub_w / 2.0;
    let y = sh as f32 / scale - cfg.bottom_margin - sub_h;

    let bg_alpha = cfg.opacity.clamp(0.0, 1.0);
    let font = FontId::proportional(cfg.font_size);

    ctx.show_viewport_immediate(
        ViewportId(egui::Id::new("subtitle")),
        ViewportBuilder::default()
            .with_title("meeting-subtitle")
            .with_decorations(false)
            .with_transparent(true)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_resizable(false)
            .with_active(false)
            .with_position(Pos2::new(x, y))
            .with_inner_size([sub_w, sub_h]),
        |ctx, _class| {
            let bg = Color32::from_rgba_unmultiplied(16, 18, 24, (255.0 * bg_alpha) as u8);
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(Color32::TRANSPARENT))
                .show(ctx, |ui| {
                    let rect = ui.max_rect();
                    // 背景圆角块(半透明)
                    ui.painter().rect_filled(
                        rect.shrink(6.0),
                        CornerRadius::same(10),
                        bg,
                    );
                    // 录音红点 + 时长(左上角小字)
                    let elapsed = if started > 0 {
                        let ms = chrono_now_ms() - started;
                        let total = (ms / 1000).max(0);
                        format!("{:02}:{:02}:{:02}", total / 3600, (total % 3600) / 60, total % 60)
                    } else {
                        "00:00:00".into()
                    };
                    ui.painter().circle_filled(
                        Pos2::new(rect.left() + 18.0, rect.top() + 18.0),
                        4.0,
                        Color32::from_rgb(232, 72, 72),
                    );
                    ui.painter().text(
                        Pos2::new(rect.left() + 30.0, rect.top() + 18.0),
                        Align2::LEFT_CENTER,
                        elapsed,
                        FontId::proportional(13.0),
                        Color32::from_rgba_unmultiplied(255, 255, 255, 160),
                    );

                    // 字幕两行:上一行淡,当前行亮
                    let inner = rect.shrink2(egui::vec2(16.0, 10.0));
                    let line_h = cfg.font_size + 10.0;
                    for (i, (_, line)) in last.iter().enumerate() {
                        let cy = inner.bottom() - (last.len() - 1 - i) as f32 * line_h - line_h / 2.0;
                        let alpha = if i + 1 == last.len() { 255 } else { 140 };
                        ui.painter().text(
                            Pos2::new(inner.center().x, cy),
                            Align2::CENTER_CENTER,
                            truncate_line(line, sub_w as usize, cfg.font_size),
                            font.clone(),
                            Color32::from_rgba_unmultiplied(255, 255, 255, alpha),
                        );
                    }
                });
        },
    );
}

/// 粗略按字符宽截断(中文≈1em,ASCII≈0.5em)
fn truncate_line(line: &str, max_px_w: usize, font_size: f32) -> String {
    let max_units = ((max_px_w as f32 - 32.0) / font_size).max(4.0) as usize;
    let mut units = 0usize;
    let mut out = String::new();
    for c in line.chars() {
        let w = if c.is_ascii() { 1 } else { 2 };
        if units + w > max_units.saturating_sub(2) {
            out.push('…');
            break;
        }
        units += w;
        out.push(c);
    }
    out
}
