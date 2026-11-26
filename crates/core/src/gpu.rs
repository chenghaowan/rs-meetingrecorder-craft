//! 显卡探测(DXGI 枚举适配器):给本地 LLM 模型按显存分档推荐做依据。
//!
//! 只取 DedicatedVideoMemory(独显的实打实显存;核显通常只报几百 MB,
//! 共享内存不算),再排除 DXGI_ADAPTER_FLAG_SOFTWARE 的软件适配器
//! (远程桌面会话里常见),避免把 llvmpipe 当独显。

use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_DESC1};

#[derive(Debug, Clone)]
pub struct GpuInfo {
    pub name: String,
    pub dedicated_mb: u64,
    pub shared_mb: u64,
    /// DXGI_ADAPTER_FLAG_SOFTWARE(如 Microsoft Basic Render Driver)
    pub software: bool,
}

/// 枚举全部 DXGI 适配器(失败返回空,调用方按"无独显"降级)
pub fn gpus() -> Vec<GpuInfo> {
    let mut out = Vec::new();
    unsafe {
        let factory: IDXGIFactory1 = match CreateDXGIFactory1() {
            Ok(f) => f,
            Err(_) => return out,
        };
        let mut i = 0u32;
        loop {
            let adapter = match factory.EnumAdapters1(i) {
                Ok(a) => a,
                Err(_) => break,
            };
            i += 1;
            let desc: DXGI_ADAPTER_DESC1 = match adapter.GetDesc1() {
                Ok(d) => d,
                Err(_) => continue,
            };
            let name = String::from_utf16_lossy(&desc.Description);
            out.push(GpuInfo {
                name: name.trim_end_matches('\0').to_string(),
                dedicated_mb: desc.DedicatedVideoMemory as u64 / (1024 * 1024),
                shared_mb: desc.SharedSystemMemory as u64 / (1024 * 1024),
                software: (desc.Flags & 1) != 0, // DXGI_ADAPTER_FLAG_SOFTWARE
            });
        }
    }
    out
}

/// 用于推荐的"独显":非软件适配器里 DedicatedVideoMemory 最大者,
/// 且 >=2 GB(核显一般只报几百 MB,共享内存不在此列);None = 核显/无卡/RDP
pub fn discrete() -> Option<GpuInfo> {
    gpus()
        .into_iter()
        .filter(|g| !g.software && g.dedicated_mb >= 2048)
        .max_by_key(|g| g.dedicated_mb)
}
