//! mrc-postprocess — 后处理 CLI。
//!
//! 用法:
//!   mrc-postprocess list [--json]                      # 会议列表
//!   mrc-postprocess run <id|last> [--no-llm] [--non-interactive] [--json]
//!   mrc-postprocess name <id> SPEAKER_00 张三  # 设说话人名
//!   mrc-postprocess speak <id> SPEAKER_00 [--json]    # 看该说话人的样例发言(对号用)
//!   mrc-postprocess package <id>              # 只重建会议包(不跑 LLM)
//!   mrc-postprocess package-read <id|last> [--json]   # 读会议包内容(转写/总结/行动项/截图)
//!   mrc-postprocess fixture-shots             # 生成测试用 gallery.db(联调截图链路)

use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde_json::{json, Value};

use mrc_core::config::Config;
use mrc_core::db;
use mrc_core::pipeline;

fn main() {
    if let Err(e) = run() {
        eprintln!("错误: {e:#}");
        std::process::exit(1);
    }
}

/// 摘出全局 --json 标志(出现即 true), 返回剩余参数(拥有所有权)
fn split_json_flag(args: &[String]) -> (Vec<String>, bool) {
    let json = args.iter().any(|a| a == "--json");
    let rest: Vec<String> = args.iter().filter(|a| a.as_str() != "--json").cloned().collect();
    (rest, json)
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        print_help();
        return Ok(());
    };
    let cfg = Config::load()?;
    match cmd.as_str() {
        "list" => {
            let (_rest, json) = split_json_flag(&args[1..]);
            cmd_list(&cfg, json)
        }
        "run" => {
            let (rest, json) = split_json_flag(&args[1..]);
            cmd_run(&cfg, &rest, json)
        }
        "name" => cmd_name(&cfg, &args[1..]),
        "speak" => {
            let (rest, json) = split_json_flag(&args[1..]);
            cmd_speak(&cfg, &rest, json)
        }
        "package" => cmd_package(&cfg, &args[1..]),
        "package-read" => {
            let (rest, _json) = split_json_flag(&args[1..]);
            cmd_package_read(&cfg, &rest)
        }
        "fixture-shots" => cmd_fixture_shots(&cfg, &args[1..]),
        "init-config" => {
            let p = Config::config_path();
            if p.exists() {
                bail!("配置已存在: {}", p.display());
            }
            Config::save_example(&p)?;
            println!("已生成示例配置: {}", p.display());
            println!("请配置 [llm](api_key 或 provider=\"local\")");
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

  init-config                    生成示例 config.toml(配置 [llm] 后可出总结)
  list [--json]                  会议列表
  run <id|last> [--no-llm] [--non-interactive] [--json]
                                 跑/重跑后处理流水线
  name <id> <SPEAKER_XX> <名字>  命名说话人(多次调用)
  speak <id> <SPEAKER_XX> [--json]
                                 打印该说话人样例发言(帮对号)
  package <id>                   只重建会议包(截图+骨架 JSON+minutes.md,不跑 LLM)
  package-read <id|last> [--json]
                                 读会议包内容(转写/总结/行动项/截图), 供页面解析
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

fn cmd_list(cfg: &Config, json: bool) -> Result<()> {
    let conn = db::open(&cfg.db_path())?;
    let meetings = db::list_meetings(&conn)?;
    if json {
        let rows: Vec<Value> = meetings
            .iter()
            .map(|m| {
                json!({
                    "id": m.id,
                    "title": m.title,
                    "started_at": m.started_at,
                    "status": m.status,
                    "duration_min": m.duration_ms().map(|d| d / 60000),
                    "package_dir": m.package_dir,
                })
            })
            .collect();
        println!("{}", serde_json::to_string(&json!({ "meetings": rows }))?);
        return Ok(());
    }
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

fn cmd_run(cfg: &Config, args: &[String], json: bool) -> Result<()> {
    let mut skip_llm = false;
    let mut interactive = true;
    let mut spec = None;
    for a in args {
        match a.as_str() {
            "--no-llm" => skip_llm = true,
            "--non-interactive" => interactive = false,
            _ if spec.is_none() => spec = Some(a.clone()),
            _ => {}
        }
    }
    let spec = spec.context("缺会议 id(数字或 last)")?;
    let conn = db::open(&cfg.db_path())?;
    let id = resolve_meeting(&conn, &spec)?;
    match pipeline::run(cfg, &mut db::open(&cfg.db_path())?, id, interactive, skip_llm) {
        Ok(r) => {
            if json {
                println!("{}", serde_json::to_string(&r)?);
            } else {
                println!("{r}");
            }
        }
        Err((_, Some(pipeline::PipelineStop::NamingNeeded(keys)))) => {
            if json {
                println!("{}", serde_json::to_string(&json!({
                    "status": "need_naming",
                    "meeting_id": id,
                    "keys": keys,
                }))?);
                return Ok(());
            }
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

fn cmd_speak(cfg: &Config, args: &[String], json: bool) -> Result<()> {
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
    let total_min = rows.iter().map(|s| s.end_ms - s.start_ms).sum::<i64>() / 60000;
    // 首中尾各一条样例
    let samples: Vec<Value> = [0, rows.len() / 2, rows.len().saturating_sub(1)]
        .into_iter()
        .collect::<Vec<_>>()
        .into_iter()
        .filter_map(|i| rows.get(i))
        .map(|s| {
            json!({
                "start_ms": s.start_ms,
                "end_ms": s.end_ms,
                "text": s.text,
            })
        })
        .collect();
    if json {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "meeting_id": id,
                "key": key,
                "segments": rows.len(),
                "minutes": total_min,
                "samples": samples,
            }))?
        );
        return Ok(());
    }
    println!("{key}: {} 段,共 {} 分钟", rows.len(), total_min);
    for s in &samples {
        let start_ms = s["start_ms"].as_i64().unwrap_or(0);
        let text = s["text"].as_str().unwrap_or("");
        println!("  [{:02}:{:02}] {}", start_ms / 60000 % 60, start_ms / 1000 % 60, text);
    }
    Ok(())
}

/// 读会议包内容(不跑流水线): transcript/summary/actions/decisions/risks/screenshots/meta,
/// 文件缺失的字段为 null。ltl 内嵌会议页读详情用。
fn cmd_package_read(cfg: &Config, args: &[String]) -> Result<()> {
    let spec = args.first().context("缺会议 id(数字或 last)")?;
    let conn = db::open(&cfg.db_path())?;
    let id = resolve_meeting(&conn, spec)?;
    let m = db::get_meeting(&conn, id)?;
    let Some(pkg_dir) = m.package_dir.clone() else {
        bail!("会议 #{id} 还没有会议包(先跑 run 或 package)");
    };
    let dir = std::path::PathBuf::from(&pkg_dir);
    let read = |name: &str| -> Value {
        std::fs::read_to_string(dir.join(name))
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or(Value::Null)
    };
    let out = json!({
        "meeting_id": id,
        "title": m.title,
        "started_at": m.started_at,
        "status": m.status,
        "package_dir": pkg_dir,
        "transcript": read("transcript.json"),
        "summary": read("summary.json"),
        "actions": read("actions.json"),
        "decisions": read("decisions.json"),
        "risks": read("risks.json"),
        "screenshots": read("screenshots.json"),
        "meta": read("meta.json"),
    });
    // package-read 是 JSON 专用命令(供页面解析), --json 可省
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}
fn cmd_package(cfg: &Config, args: &[String]) -> Result<()> {
    let spec = args.first().context("缺会议 id")?;
    let conn = db::open(&cfg.db_path())?;
    let id = resolve_meeting(&conn, &spec)?;
    // 不跑 LLM,只重建包(summary 沿用库里已有的)
    let r = pipeline::run(cfg, &mut db::open(&cfg.db_path())?, id, false, true)
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
