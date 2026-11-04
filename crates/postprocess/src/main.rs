//! mrc-postprocess — 后处理 CLI。
//!
//! 用法:
//!   mrc-postprocess list                      # 会议列表
//!   mrc-postprocess run <id|last> [--no-llm] [--no-rag] [--non-interactive]
//!   mrc-postprocess name <id> SPEAKER_00 张三  # 设说话人名
//!   mrc-postprocess speak <id> SPEAKER_00     # 看该说话人的样例发言(对号用)
//!   mrc-postprocess package <id>              # 只重建会议包(不跑 LLM)
//!   mrc-postprocess rag <id>                  # 重试 RAG 入库
//!   mrc-postprocess fixture-shots             # 生成测试用 gallery.db(联调截图链路)

use anyhow::{bail, Context, Result};
use rusqlite::Connection;

use mrc_core::config::Config;
use mrc_core::db;
use mrc_core::pipeline;

fn main() {
    if let Err(e) = run() {
        eprintln!("错误: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        print_help();
        return Ok(());
    };
    let cfg = Config::load()?;
    match cmd.as_str() {
        "list" => cmd_list(&cfg),
        "run" => cmd_run(&cfg, &args[1..]),
        "name" => cmd_name(&cfg, &args[1..]),
        "speak" => cmd_speak(&cfg, &args[1..]),
        "package" => cmd_package(&cfg, &args[1..]),
        "rag" => cmd_rag(&cfg, &args[1..]),
        "fixture-shots" => cmd_fixture_shots(&cfg, &args[1..]),
        "init-config" => {
            let p = Config::config_path();
            if p.exists() {
                bail!("配置已存在: {}", p.display());
            }
            Config::save_example(&p)?;
            println!("已生成示例配置: {}", p.display());
            println!("请填 [llm] api_key、[diarize] hf_token、[rag] base_url");
            Ok(())
        }
        _ => {
            print_help();
            bail!("未知命令: {cmd}");
        }
    }
}

fn print_help() {
    println!(
        r#"mrc-postprocess — 会议后处理 CLI

  init-config                    生成示例 config.toml(填 api_key 等)
  list                           会议列表
  run <id|last> [--no-llm] [--no-rag] [--non-interactive]
                                 跑/重跑后处理流水线
  name <id> <SPEAKER_XX> <名字>  命名说话人(多次调用)
  speak <id> <SPEAKER_XX>        打印该说话人样例发言(帮对号)
  package <id>                   只重建会议包(截图+骨架 JSON,不跑 LLM)
  rag <id>                       重试 RAG 入库
  fixture-shots <目录>           造测试 gallery.db(截图链路联调)"#
    );
}

fn resolve_meeting(conn: &Connection, spec: &str) -> Result<i64> {
    if spec == "last" {
        return db::latest_meeting(conn)?
            .map(|m| m.id)
            .context("没有可后处理的会议");
    }
    spec.parse::<i64>().context("会议 id 必须是数字或 last")
}

fn cmd_list(cfg: &Config) -> Result<()> {
    let conn = db::open(&cfg.db_path())?;
    let meetings = db::list_meetings(&conn)?;
    if meetings.is_empty() {
        println!("(还没有会议)");
        return Ok(());
    }
    println!("{:>4}  {:<19} {:<12} {:<6} 标题", "id", "开始时间", "状态", "时长");
    for m in meetings {
        let dur = m
            .duration_ms()
            .map(|d| format!("{}m", d / 60000))
            .unwrap_or_else(|| "-".into());
        println!(
            "{:>4}  {:<19} {:<12} {:<6} {}",
            m.id,
            db::fmt_local(m.started_at),
            m.status,
            dur,
            m.title
        );
    }
    Ok(())
}

fn cmd_run(cfg: &Config, args: &[String]) -> Result<()> {
    let mut skip_llm = false;
    let mut skip_rag = false;
    let mut interactive = true;
    let mut spec = None;
    for a in args {
        match a.as_str() {
            "--no-llm" => skip_llm = true,
            "--no-rag" => skip_rag = true,
            "--non-interactive" => interactive = false,
            _ if spec.is_none() => spec = Some(a.clone()),
            _ => {}
        }
    }
    let spec = spec.context("缺会议 id(数字或 last)")?;
    let conn = db::open(&cfg.db_path())?;
    let id = resolve_meeting(&conn, &spec)?;
    match pipeline::run(cfg, &mut db::open(&cfg.db_path())?, id, interactive, skip_llm, skip_rag) {
        Ok(r) => println!("{r}"),
        Err((_, Some(pipeline::PipelineStop::NamingNeeded(keys)))) => {
            println!("\n需要先命名说话人: {keys:?}");
            for k in &keys {
                println!("  mrc-postprocess speak {id} {k}    # 看样例");
                println!("  mrc-postprocess name {id} {k} 名字");
            }
            println!("命名完成后重新运行: mrc-postprocess run {id}");
        }
        Err((e, None)) => return Err(e),
    }
    Ok(())
}

fn cmd_name(cfg: &Config, args: &[String]) -> Result<()> {
    let (id, key, name) = match args {
        [a, b, c] => (
            a.parse::<i64>().context("会议 id 必须是数字")?,
            b.clone(),
            c.clone(),
        ),
        _ => bail!("用法: mrc-postprocess name <id> <SPEAKER_XX> <名字>"),
    };
    let conn = db::open(&cfg.db_path())?;
    db::get_meeting(&conn, id)?;
    db::upsert_speaker(&conn, id, &key, Some(&name), None, None)?;
    println!("已命名 {key} → {name}(展示时按 speakers 表解析)");
    Ok(())
}

fn cmd_speak(cfg: &Config, args: &[String]) -> Result<()> {
    let (id, key) = match args {
        [a, b] => (a.parse::<i64>().context("会议 id 必须是数字")?, b.clone()),
        _ => bail!("用法: mrc-postprocess speak <id> <SPEAKER_XX>"),
    };
    let conn = db::open(&cfg.db_path())?;
    let segs = db::load_segments(&conn, id)?;
    let rows: Vec<&db::Segment> = segs
        .iter()
        .filter(|s| s.speaker.as_deref() == Some(key.as_str()))
        .collect();
    println!("{key}: {} 段,共 {} 分钟", rows.len(), rows.iter().map(|s| s.end_ms - s.start_ms).sum::<i64>() / 60000);
    // 首中尾各一条样例
    for i in [0, rows.len() / 2, rows.len().saturating_sub(1)] {
        if let Some(s) = rows.get(i) {
            println!("  [{:02}:{:02}] {}", s.start_ms / 60000 % 60, s.start_ms / 1000 % 60, s.text);
        }
    }
    Ok(())
}

fn cmd_package(cfg: &Config, args: &[String]) -> Result<()> {
    let spec = args.first().context("缺会议 id")?;
    let conn = db::open(&cfg.db_path())?;
    let id = resolve_meeting(&conn, spec)?;
    // 不跑 LLM 不入库,只重建包(summary 沿用库里已有的)
    let r = pipeline::run(cfg, &mut db::open(&cfg.db_path())?, id, false, true, true)
        .map_err(|(e, _)| e)?;
    println!("{r}");
    Ok(())
}

fn cmd_rag(cfg: &Config, args: &[String]) -> Result<()> {
    let spec = args.first().context("缺会议 id")?;
    let conn = db::open(&cfg.db_path())?;
    let id = resolve_meeting(&conn, spec)?;
    let r = pipeline::run(cfg, &mut db::open(&cfg.db_path())?, id, false, true, false)
        .map_err(|(e, _)| e)?;
    println!("{r}");
    Ok(())
}

/// 造一个测试 gallery.db + 几张 PNG,验证截图→会议包链路。
fn cmd_fixture_shots(cfg: &Config, args: &[String]) -> Result<()> {
    let root = args
        .first()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap().join("fixture_gallery"));
    let imgs = root.join("imgs");
    std::fs::create_dir_all(&imgs)?;

    let conn = Connection::open(root.join("gallery.db"))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS shots (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           created_at INTEGER NOT NULL,
           file_path TEXT NOT NULL,
           thumb_path TEXT, format TEXT NOT NULL, width INTEGER, height INTEGER,
           file_size INTEGER, content_hash TEXT, app_name TEXT,
           ocr_text TEXT NOT NULL DEFAULT '', ocr_lines TEXT,
           ocr_status TEXT NOT NULL DEFAULT 'pending'
         );",
    )?;

    // 用固定会议时间造 3 张图:会议起点(取最近的会议,没有就 now-30min)
    let meetings = db::list_meetings(&conn2(cfg)?)?;
    let started_at = meetings
        .first()
        .map(|m| m.started_at)
        .unwrap_or_else(db::now_ms);
    // 放在有发言的时段(2:30/2:53/6:00),保证 ±30s 窗口能匹配到上下文
    let samples = [
        (150_000, "张三 李四 王五\n参会人列表"),
        (173_000, "会议纪要: 增加偷胶结构"),
        (360_000, "DFM 检查表 v2"),
    ];
    for (i, (rel_ms, ocr)) in samples.iter().enumerate() {
        let rel = format!("imgs/fixture_{i:03}.png");
        let abs = root.join(&rel);
        if !abs.exists() {
            write_fixture_png(&abs, i as u8)?;
        }
        conn.execute(
            "INSERT INTO shots(created_at, file_path, format, width, height, file_size, ocr_text)
             VALUES (?1,?2,'png',64,64,999,?3)",
            rusqlite::params![started_at + rel_ms, rel, ocr],
        )?;
    }
    println!(
        "fixture 已生成:\n  db   = {}\n  root = {}\n联调: 在 config.toml [shots] 填 db_path/root 后跑 run",
        root.join("gallery.db").display(),
        root.display()
    );
    Ok(())
}

fn conn2(cfg: &Config) -> Result<Connection> {
    db::open(&cfg.db_path())
}

/// 64x64 纯色测试 PNG(不同灰度便于肉眼区分)
fn write_fixture_png(path: &std::path::Path, idx: u8) -> Result<()> {
    use std::io::Write;
    let img = image::RgbaImage::from_pixel(64, 64, image::Rgba([idx * 60 + 40, 80, 160, 255]));
    img.save_with_format(path, image::ImageFormat::Png)?;
    path.parent().map(|p| std::fs::create_dir_all(p).ok());
    let _ = std::io::stdout().flush();
    Ok(())
}
