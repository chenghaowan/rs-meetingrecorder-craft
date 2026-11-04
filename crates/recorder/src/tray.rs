//! 系统托盘(同 pointtofixcraft 模式):菜单事件来自全局回调线程,
//! 塞进共享队列并 request_repaint 唤醒 eframe 帧循环。

use std::sync::{Arc, Mutex};

use eframe::egui::Context;

use crate::i18n::Strings;

pub enum TrayEvent {
    /// 菜单「开始/结束会议」(热键同效)
    Toggle,
    /// 菜单「设置」
    Settings,
    /// 菜单「字幕 显示/隐藏」
    ToggleSubtitle,
    /// 菜单「退出」
    Exit,
}

struct Shared {
    events: Mutex<Vec<TrayEvent>>,
}

pub struct Tray {
    shared: Arc<Shared>,
    /// 持有图标实例:drop 时托盘图标自动移除
    _icon: tray_icon::TrayIcon,
}

const MENU_TOGGLE: &str = "mrc-toggle";
const MENU_SETTINGS: &str = "mrc-settings";
const MENU_SUBTITLE: &str = "mrc-subtitle";
const MENU_EXIT: &str = "mrc-exit";

/// 建托盘(必须在主线程调用);失败不致命(仅失去托盘入口),返回 None
pub fn init(ctx: &Context, tr: &'static Strings) -> Option<Tray> {
    let rgba = image::load_from_memory(include_bytes!("tray_icon.png"))
        .ok()?
        .into_rgba8();
    let (w, h) = rgba.dimensions();
    let icon = tray_icon::Icon::from_rgba(rgba.into_raw(), w, h).ok()?;

    let menu = tray_icon::menu::Menu::new();
    let item_toggle = tray_icon::menu::MenuItem::with_id(MENU_TOGGLE, tr.menu_toggle, true, None);
    let item_sub = tray_icon::menu::MenuItem::with_id(MENU_SUBTITLE, tr.menu_subtitle, true, None);
    let item_settings = tray_icon::menu::MenuItem::with_id(MENU_SETTINGS, tr.menu_settings, true, None);
    let item_exit = tray_icon::menu::MenuItem::with_id(MENU_EXIT, tr.menu_exit, true, None);
    let _ = menu.append_items(&[&item_toggle, &item_sub, &item_settings, &item_exit]);

    let icon_builder = tray_icon::TrayIconBuilder::new()
        .with_id("mrc-tray")
        .with_icon(icon)
        .with_tooltip("会议记录 (F9 开始/结束)")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false);

    let tray_icon = icon_builder.build().ok()?;

    let shared = Arc::new(Shared {
        events: Mutex::new(Vec::new()),
    });

    // 回调线程:事件入队 + 唤醒帧循环(主窗隐藏时也有效)
    let shared2 = Arc::clone(&shared);
    let ctx2 = ctx.clone();
    std::thread::spawn(move || {
        for ev in tray_icon::menu::MenuEvent::receiver().iter() {
            let kind = match ev.id.as_ref() {
                MENU_TOGGLE => Some(TrayEvent::Toggle),
                MENU_SETTINGS => Some(TrayEvent::Settings),
                MENU_SUBTITLE => Some(TrayEvent::ToggleSubtitle),
                MENU_EXIT => Some(TrayEvent::Exit),
                _ => None,
            };
            if let Some(kind) = kind {
                if let Ok(mut q) = shared2.events.lock() {
                    q.push(kind);
                }
                ctx2.request_repaint();
            }
        }
    });

    Some(Tray {
        shared,
        _icon: tray_icon,
    })
}

impl Tray {
    /// 帧循环里取走积压的托盘事件
    pub fn drain(&self) -> Vec<TrayEvent> {
        let mut q = self.shared.events.lock().expect("tray 队列锁");
        std::mem::take(&mut *q)
    }
}
