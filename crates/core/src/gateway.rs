//! 网关(rs-aiprivaderCraft)生命周期管理:必要软件的"确保就绪"三段式。
//!
//! 1. /health 探测(1.5s)——活着直接用;模型由网关按 config.yaml 声明自动下载
//! 2. 未启动:探测本地 exe(tools 目录 → 兄弟仓库 target)→ 分离进程启动 → 轮询就绪
//! 3. 本地也没有:`gh release download`(走已认证的 gh CLI,代理/TLS 通吃)
//!    从 GitHub release 拉便携 zip → PowerShell 解压 → 启动 → 就绪
//!
//! 全失败时返回带手动指引的错误。模型( SenseVoice/opus-mt/BGE )全部归网关管理,
//! 本项目不下载任何模型文件。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use crate::config::Config;

pub const GATEWAY_REPO: &str = "chenghaowan/rs-aiprivader-craft";
pub const GATEWAY_TAG: &str = "v0.1.0";
pub const GATEWAY_ZIP: &str = "wanai-runtime-v0.1.0-windows-x64.zip";
pub const EXE_NAME: &str = "ai-runtime.exe";

/// /health 快速探测
pub fn health(url: &str, timeout: Duration) -> bool {
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    agent
        .get(&format!("{}/health", url.trim_end_matches('/')))
        .call()
        .is_ok()
}

fn install_dir(cfg: &Config) -> PathBuf {
    cfg.data_dir().join("tools").join("wanai-runtime")
}

/// 本地 exe 探测:配置覆盖 → tools 安装位 → 兄弟仓库 target
pub fn resolve_local_exe(cfg: &Config) -> Option<PathBuf> {
    if let Some(p) = &cfg.gateway.exe_path {
        if p.is_file() {
            return Some(p.clone());
        }
    }
    let installed = install_dir(cfg).join("wanai-runtime-portable").join(EXE_NAME);
    if installed.is_file() {
        return Some(installed);
    }
    // 兄弟仓库开发版探测可被配置关闭(用于测试 release 下载路径)
    if !cfg.gateway.dev_probe_disabled {
        let mut roots: Vec<PathBuf> = vec![];
        if let Ok(cwd) = std::env::current_dir() {
            for anc in cwd.ancestors() {
                roots.push(anc.to_path_buf());
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            for anc in exe.ancestors().skip(1) {
                roots.push(anc.to_path_buf());
            }
        }
        for root in roots {
            let p = root
                .join("rs-aiprivaderCraft")
                .join("target")
                .join("release")
                .join(EXE_NAME);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// exe 对应的 config.yaml:exe 旁 → 向上各级(兄弟仓库布局是仓库根放 config)
fn find_config(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let direct = dir.join("config.yaml");
    if direct.is_file() {
        return Some(direct);
    }
    for anc in dir.ancestors().skip(1) {
        let p = anc.join("config.yaml");
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn find_exe_deep(dir: &Path, depth: usize) -> Option<PathBuf> {
    if depth > 2 {
        return None;
    }
    let exe = dir.join(EXE_NAME);
    if exe.is_file() {
        return Some(exe);
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return None;
    };
    for e in rd.flatten() {
        if e.path().is_dir() {
            if let Some(found) = find_exe_deep(&e.path(), depth + 1) {
                return Some(found);
            }
        }
    }
    None
}

fn spawn_detached(exe: &Path) -> Result<()> {
    let mut cmd = std::process::Command::new(exe);
    if let Some(cfg) = find_config(exe) {
        cmd.arg("--config").arg(&cfg);
        // config 里 model_dir 相对 cwd:取 config 所在目录
        if let Some(d) = cfg.parent() {
            cmd.current_dir(d);
        }
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("启动网关失败: {}", exe.display()))?;
    Ok(())
}

fn wait_health(url: &str, secs: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if health(url, Duration::from_millis(800)) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(600));
    }
    bail!("网关 {url} 在 {secs}s 内未就绪(看进程是否启动/端口是否被占)");
}

/// 三段式确保就绪。返回实际发生的动作描述(供日志/CLI 输出)。
pub fn ensure(cfg: &Config) -> Result<String> {
    let url = cfg.gateway_url();
    if health(&url, Duration::from_millis(1500)) {
        return Ok(format!("already-running ({url})"));
    }

    // 2. 本地 exe 启动
    if cfg.gateway.auto_start {
        if let Some(exe) = resolve_local_exe(cfg) {
            spawn_detached(&exe)?;
            wait_health(&url, 20).map_err(|e| {
                anyhow!("{e}; 手动排查: 直接运行 {}", exe.display())
            })?;
            return Ok(format!("spawned {} ({url})", exe.display()));
        }

        // 3. release 下载(gh 已认证,代理/TLS 由它处理)
        let dir = install_dir(cfg);
        std::fs::create_dir_all(&dir)?;
        let status = std::process::Command::new("gh")
            .args([
                "release",
                "download",
                GATEWAY_TAG,
                "-R",
                GATEWAY_REPO,
                "--pattern",
                "*.zip",
                "--clobber",
                "--dir",
                dir.to_string_lossy().as_ref(),
            ])
            .stdin(std::process::Stdio::null())
            .output()
            .context("调用 gh 失败(未安装 GitHub CLI?)")?;
        if !status.status.success() {
            bail!(
                "网关 release 下载失败: {}",
                String::from_utf8_lossy(&status.stderr).lines().last().unwrap_or("")
            );
        }
        let zip = dir.join(GATEWAY_ZIP);
        if !zip.exists() {
            bail!("release 里没有 {GATEWAY_ZIP}");
        }
        let ps = format!(
            "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
            zip.display(),
            dir.join("wanai-runtime-portable").display()
        );
        let unp = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &ps])
            .output()
            .context("调用 PowerShell 解压失败")?;
        if !unp.status.success() {
            bail!("解压失败: {}", String::from_utf8_lossy(&unp.stderr));
        }
        // 解压布局兼容:zip 根直接是文件,或多一层版本目录
        let portable = dir.join("wanai-runtime-portable");
        let exe = find_exe_deep(&portable, 0).ok_or_else(|| anyhow!("解压后没有 {}", EXE_NAME))?;
        spawn_detached(&exe)?;
        wait_health(&url, 25)?;
        return Ok(format!("downloaded-release → {} ({url})", exe.display()));
    }

    bail!(
        "网关未启动且 auto_start=false; 手动启动: {},或 curl {url}/health 检查",
        resolve_local_exe(cfg).map(|p| p.display().to_string()).unwrap_or_else(|| format!("gh release download {GATEWAY_TAG} -R {GATEWAY_REPO}"))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_on_dead_port_is_false() {
        assert!(!health("http://127.0.0.1:59999", Duration::from_millis(300)));
    }
}
