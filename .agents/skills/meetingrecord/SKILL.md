---
name: meetingrecord
description: 查询/检索本地会议记录(meetings.db + 会议包 JSON),或用无头 CLI 驱动"录音→转写→说话人→LLM总结→会议包→RAG入库"全流程。适用于用户要求"把今天的会议内容找出来"、"上次谁定了××"、"开个会录下来"、"重新总结某场会议"、"查某次会议的行动项"等场景。
---

# ltl-meetingrecordCraft 会议记录接入

会议记录器把每场会议的转写、说话人、LLM 总结(decisions/actions/risks)、截图关联存入本地 SQLite 与"会议包"目录,并提供无头 CLI。你可以**直接读库/读包检索**,也可以**调用 CLI 驱动录音与后处理**。两种方式都不需要 MCP——普通 sqlite3 / 子进程即可。

## 1. 数据在哪

- 数据库:`%APPDATA%\ltl-meetingrecord\meetings.db`(WAL,可只读并发)
- 会议包:`%APPDATA%\ltl-meetingrecord\packages\meeting_YYYYMMDD\`
  - `transcript.json` — 全量段落(时间戳/说话人/文本)+ chunk 索引
  - `summary.json` — LLM 结构化总结(summary/decisions/actions/risks,条目带 evidence_time)
  - `actions.json` / `decisions.json` — 单独拆出的条目(供任务导入)
  - `screenshots.json` + `screenshots/img001.png…` — 截图与上下文关联
  - `meta.json` — 元信息
- 录音归档:`%APPDATA%\ltl-meetingrecord\audio\meeting_<id>.wav`

关键表:`meetings`(status: recording/processing/done/failed)、`segments`(speaker 列是簇键 SPEAKER_xx,显示名在 `speakers` 表 JOIN)、`speakers`(name/proposed_name/similarity)、`voiceprints`(声纹库,name+centroid BLOB)。

## 2. CLI(recorder 二进制,双职责)

`mrc-recorder` 不带参数启动 GUI;带参数走无头模式:

```bash
mrc-recorder --start [--title "DFM评审"]   # 无 GUI 录音,回车/Ctrl+C 结束,
                                           # 自动后处理,stdout 输出 JSON 回执
mrc-recorder --status                      # 最近 10 场会议 + pointtofix 位置(JSON)
mrc-recorder --tool-install [URL|路径]     # 安装/更新 pointtofixcraft(截图工具)
mrc-recorder --tool-launch                 # 启动截图工具
```

`--start` 回执示例:
```json
{"meeting_id": 3, "status": "done", "segments": 412, "package_dir": "...\\meeting_20261105", "note": null}
```
`status` 为 `need_naming` 时按 note 提示用 postprocess 命名后重跑。

`mrc-postprocess` 负责后处理:
```bash
mrc-postprocess list                    # 会议列表
mrc-postprocess run last [--no-llm] [--no-rag] [--non-interactive]
mrc-postprocess name <id> SPEAKER_00 张三   # 命名(声纹随之注册)
mrc-postprocess speak <id> SPEAKER_00       # 看某人样例发言(帮对号)
mrc-postprocess rag <id>                    # 重试 RAG 入库
```

## 3. 典型任务

**找历史决议/行动项**:读最近会议包(或按 `--status` 拿 package_dir):
```bash
cat "$(cygpath "$APPDATA")/ltl-meetingrecord/packages/<最新目录>/actions.json"
```
再要原文溯源 → `transcript.json` 按 `evidence_time` 找段落。

**全库检索**:meetings.db 无 FTS,直接 SQL:
```sql
SELECT m.id, m.title, m.started_at FROM meetings m WHERE m.status='done';
SELECT text FROM segments WHERE meeting_id=3 AND text LIKE '%偷胶%';
```

**录一场会(agent 驱动)**:先 `mrc-recorder --start --title xxx` 前台跑,结束时流水线自动执行;输出 JSON 后按需 `mrc-postprocess name/run` 补命名。

## 4. 注意

- 转写需 whisper ggml 模型(config.toml `[whisper] model_path` 或 `models\*.bin`);缺失时只录音归档,会后可用 `postprocess` 不补转写(模型补上后重新录才有效)
- LLM 总结需 `[llm] api_key`;`--no-llm` 只出转写/截图/骨架包
- 说话人分离需 pyannote(python + HF token);没有时名字全靠手动命名
- 会议包重跑幂等(复用同目录覆盖);截图源是 pointtofixcraft 的 gallery.db(只读)
- F8 截图归 pointtofixcraft;本工具热键是 F9
