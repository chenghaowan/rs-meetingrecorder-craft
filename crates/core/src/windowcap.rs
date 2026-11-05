//! 指定窗口截图:窗口 → 精确可见矩形(DWM 扩展边界)→ 复用
//! pointtofixcraft 的无头区域截图(`--capture x,y,w,h --ocr --json`)。
//! 本模块只解析矩形,不写对方数据库——落库/OCR/去重全归 pointtofixcraft。

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowRect, IsIconic, IsWindowVisible,
};

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub title: String,
    /// 物理像素,虚拟桌面坐标(可为负,副屏在左侧/上方时)
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// 精确可见矩形(去掉阴影边);失败退回 GetWindowRect
fn frame_bounds(hwnd: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
    };
    hr.ok().map(|_| r)
}

/// 按标题子串(大小写不敏感)找可见窗口;命中多个时取面积最大的(主窗口通常是它)。
pub fn find_window(title_substr: &str) -> Result<WindowInfo> {
    if title_substr.trim().is_empty() {
        bail!("窗口标题关键词为空");
    }
    let needle = title_substr.trim().to_lowercase();
    struct Ctx {
        needle: String,
        best: Option<(WindowInfo, i64)>,
    }
    let mut ctx = Ctx {
        needle,
        best: None,
    };
    unsafe extern "system" fn on_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut Ctx);
        if unsafe { IsWindowVisible(hwnd) }.as_bool() && !unsafe { IsIconic(hwnd) }.as_bool() {
            let title = window_title(hwnd);
            if !title.is_empty()
                && title.to_lowercase().contains(&ctx.needle)
            {
                let (x, y, w, h) = match frame_bounds(hwnd) {
                    Some(r) => (r.left, r.top, r.right - r.left, r.bottom - r.top),
                    None => {
                        let mut r = RECT::default();
                        let _ = unsafe { GetWindowRect(hwnd, &mut r) };
                        (r.left, r.top, r.right - r.left, r.bottom - r.top)
                    }
                };
                if w > 50 && h > 50 {
                    let area = (w as i64) * (h as i64);
                    if ctx
                        .best
                        .as_ref()
                        .map(|(_, a)| area > *a)
                        .unwrap_or(true)
                    {
                        ctx.best = Some((
                            WindowInfo {
                                hwnd: hwnd.0 as isize,
                                title,
                                x,
                                y,
                                w,
                                h,
                            },
                            area,
                        ));
                    }
                }
            }
        }
        BOOL(1) // TRUE 继续枚举
    }
    unsafe {
        EnumWindows(Some(on_window), LPARAM(&mut ctx as *mut Ctx as isize))
    }
    .map_err(|e| anyhow!("EnumWindows 失败: {e}"))?;
    match ctx.best {
        Some((info, _)) => Ok(info),
        None => bail!("找不到可见窗口标题含「{}」的窗口", title_substr),
    }
}

/// pointtofix --capture --json 的回执(只要我们用到的字段)
#[derive(Debug, Deserialize)]
pub struct CaptureReceipt {
    #[serde(default)]
    pub id: Option<i64>,
    /// pointtofixcraft 回执键名是 "file"
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub ocr: Option<serde_json::Value>,
}

/// 截指定窗口(经 pointtofixcraft 落库);返回其回执。
pub fn capture_window(pointtofix_exe: &PathBuf, info: &WindowInfo, ocr: bool) -> Result<CaptureReceipt> {
    let mut cmd = std::process::Command::new(pointtofix_exe);
    cmd.arg("--capture")
        .arg(format!("{},{},{},{}", info.x, info.y, info.w, info.h))
        .arg("--json");
    if ocr {
        cmd.arg("--ocr");
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let out = cmd
        .output()
        .with_context(|| format!("启动截图工具失败: {}", pointtofix_exe.display()))?;
    if !out.status.success() {
        bail!("截图失败(退出码 {:?}): {}", out.status.code(), String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or(""));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let json_start = text.find('{').ok_or_else(|| anyhow!("截图工具未输出 JSON: {}", text.chars().take(80).collect::<String>()))?;
    serde_json::from_str(&text[json_start..]).context("解析截图回执失败")
}

use windows::core::BOOL;
