//! 外部工具安装:pointtofixcraft 便携 exe 的探测/安装/启动。
//!
//! 来源三选一(优先级):
//!   1. 配置 tools.pointtofix_url: http(s) .exe 直链,或本地路径(exe / 含 exe 的目录)
//!   2. 兄弟仓库自动探测: <exe 向上找>\pointtofixcraft\dist\pointtofix-portable\pointtofix.exe
//!      及 cwd 相对 ..\pointtofixcraft\dist\...
//!   3. 已安装: tools.pointtofix_exe 或 %APPDATA%\ltl-meetingrecord\tools\pointtofix\pointtofix.exe
//!
//! 只支持 .exe 直链/本地文件,不支持 zip(zip 解压需引第三方依赖,便携包构建脚本
//! 可直接产出单 exe)。下载走 ureq,写 .part 再改名,半截文件不会误认。

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use crate::config::Config;

pub const EXE_NAME: &str = "pointtofix.exe";

pub fn install_dir(cfg: &Config) -> PathBuf {
    cfg.data_dir().join("tools").join("pointtofix")
}

pub fn installed_exe(cfg: &Config) -> PathBuf {
    cfg.tools
        .pointtofix_exe
        .clone()
        .unwrap_or_else(|| install_dir(cfg).join(EXE_NAME))
}

/// 已安装且文件存在
pub fn is_installed(cfg: &Config) -> Option<PathBuf> {
    let p = installed_exe(cfg);
    p.is_file().then_some(p)
}

/// 兄弟仓库 dist 自动探测(开发机场景)
fn sibling_dist() -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = vec![];
    if let Ok(exe) = std::env::current_exe() {
        // …/target/{debug,release}/mrc-recorder.exe → 向上两级 = 仓库根
        for anc in exe.ancestors().skip(1) {
            roots.push(anc.to_path_buf());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        for anc in cwd.ancestors() {
            roots.push(anc.to_path_buf());
        }
    }
    for root in roots {
        let p = root
            .join("pointtofixcraft")
            .join("dist")
            .join("pointtofix-portable")
            .join(EXE_NAME);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// 找一个"现在就能用"的 exe(已安装 > 兄弟仓库),供启动按钮用
pub fn resolve_available(cfg: &Config) -> Option<PathBuf> {
    is_installed(cfg).or_else(sibling_dist)
}

/// 安装/更新。src 为空 = 配置 url → 兄弟仓库便携包拷贝。
pub fn install(cfg: &Config, src_override: Option<&str>) -> Result<PathBuf> {
    enum Src {
        Url(String),
        Local(PathBuf),
    }
    let src: Src = match src_override
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| cfg.tools.pointtofix_url.clone().filter(|s| !s.is_empty()))
    {
        Some(s) if s.starts_with("http://") || s.starts_with("https://") => Src::Url(s),
        Some(s) => Src::Local(PathBuf::from(s)),
        None => {
            let p = sibling_dist()
                .ok_or_else(|| anyhow!("未指定来源,且未找到兄弟仓库便携包(…\\pointtofixcraft\\dist\\pointtofix-portable)"))?;
            Src::Local(p)
        }
    };

    let dst_dir = install_dir(cfg);
    std::fs::create_dir_all(&dst_dir)?;
    let dst = dst_dir.join(EXE_NAME);

    match src {
        Src::Url(src) => {
            if !src.to_lowercase().ends_with(".exe") {
                bail!("仅支持 .exe 直链(zip 请先解压或用 build-portable.bat 产出便携 exe)");
            }
            let agent = ureq::AgentBuilder::new().build();
            let resp = agent.get(&src).call().with_context(|| format!("下载失败: {src}"))?;
            let len = resp
                .header("Content-Length")
                .and_then(|v| v.parse::<u64>().ok());
            let part = dst.with_extension("exe.part");
            {
                let mut reader = resp.into_reader();
                let mut f = std::io::BufWriter::new(
                    std::fs::File::create(&part).with_context(|| format!("创建临时文件失败: {}", part.display()))?,
                );
                std::io::copy(&mut reader, &mut f).context("下载写入失败")?;
            }
            if let Some(expect) = len {
                let got = std::fs::metadata(&part)?.len();
                if got != expect {
                    let _ = std::fs::remove_file(&part);
                    bail!("下载不完整: {got}/{expect} 字节");
                }
            }
            std::fs::rename(&part, &dst).context("落盘改名失败")?;
        }
        Src::Local(file) => {
            // 本地路径:exe 文件或含 exe 的目录
            let file = if file.is_file() {
                file
            } else if file.is_dir() {
                let f = file.join(EXE_NAME);
                if !f.is_file() {
                    bail!("目录里没有 {}: {}", EXE_NAME, file.display());
                }
                f
            } else {
                bail!("路径不存在: {}", file.display());
            };
            std::fs::copy(&file, &dst).with_context(|| format!("复制失败: {} → {}", file.display(), dst.display()))?;
        }
    }
    Ok(dst)
}

    /// 启动截图工具(分离进程,不随本程序退出)
pub fn launch(cfg: &Config) -> Result<PathBuf> {
    let exe = resolve_available(cfg).ok_or_else(|| anyhow!("未安装且未找到兄弟仓库便携包,请先在设置里安装"))?;
    std::process::Command::new(&exe)
        .spawn()
        .with_context(|| format!("启动失败: {}", exe.display()))?;
    Ok(exe)
}

#[allow(unused)]
fn _unused(p: &Path) -> bool {
    p.exists()
}
