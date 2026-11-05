<div align="center">

# rs-meetingrecorder-craft

**Windows 本地会议记录器 —— 录音、转写、认人,让 LLM 写会议纪要。Rust 编写。**

开会时按一个键,屏幕底部实时出字幕;结束时再按一个键,得到一份结构化会议包:带时间戳的转写、决议、行动项、风险,以及每一张截图和"当时正在说什么"的自动关联。

[![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11%20x64-0078D6?logo=windows&logoColor=white)](#%E7%8E%AF%E5%A2%83%E8%A6%81%E6%B1%82)
[![Rust](https://img.shields.io/badge/rust-2021-DEA584?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Version](https://img.shields.io/badge/version-0.1.0-2ea44f)](#)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

[English](README.md)

</div>

---

rs-meetingrecorder-craft 是本地工具链里专管"会议纪要"的那一半:刻意**不做**在线会议、不做音视频通话。截图由兄弟工具 [PointToFix Craft](https://gitee.com/chenghao_wan/pointtofixcraft) 负责(热键分工:**F8 = 截图,F9 = 开始/结束会议**),会议历史的全文/向量检索交给本地 RAG 服务。一切都在本机运行;除非你把 LLM 总结指向云端 API,音频不会离开你的电脑。

## 流水线

```
(会议进行中)                            (会议结束)
CPAL 采集                                Meeting Post Process
  ↓                                        ↓
PCM Buffer                               Chunk(5 分钟 / 1000 token)
  ↓                                        ↓
whisper.cpp 流式转写(whisper-rs)        截图 ↔ 发言匹配(±30 秒)
  ↓                                        ↓
说话人分离(pyannote 外挂)               LLM 一次性总结,输出结构化 JSON
  ↓                                        ↓
meetings.db                     →  transcript.json · summary.json
                                   actions.json · decisions.json
(并行,独立软件)                         ↓
F8 截图 → gallery.db                    RAG 入库
```

## 功能

- **悬浮小面板**(132×96,与 PointToFix Craft 待机条同尺寸):状态灯、计时、开始/结束,置顶可配,可拖拽。
- **屏幕底部字幕条**(第二个原生视口):最近两条转写 + 录音计时;置顶/字号/透明度/底部边距可配。
- **本地转写**:whisper.cpp(whisper-rs),12 秒滑动窗口 + 尾巴续传,段落不重复。
- **务实的说话人命名**:分离只产出匿名簇(SPEAKER_00…);名字靠 10 秒人工确认(样例发言 + 历史名单一键点选),**声纹匹配**跨会议自动预填(192/512 维向量存 SQLite BLOB,余弦暴力扫,百人库微秒级)。
- **LLM 一次成稿**:长上下文模型整场一次性总结,强制输出结构化 JSON(summary / decisions / actions / risks,每条带 evidence_time 溯源);超长自动降级为两段总结 + 一次合并。
- **会议包** —— 纯文件,零锁定:
  ```
  meeting_20261105/
  ├── transcript.json      # 全量段落 + 说话人 + chunk 索引
  ├── summary.json         # LLM 总结(只存 JSON,展示时再转 Markdown)
  ├── actions.json  decisions.json  risks.json
  ├── screenshots.json     # 每张截图 {"image","ts","context"}
  ├── screenshots/         # img001.png…(复制快照)
  └── meta.json
  ```
- **设置界面**:whisper 模型路径/语言、pyannote token、LLM 端点(任何 OpenAI 兼容 API)、字幕选项、chunk 参数、RAG 地址 —— 内置 **PointToFix Craft 安装器**(.exe 直链 / 本地路径 / 自动探测兄弟仓库便携包)。
- **国际化**:中文 / English / Deutsch / 日本語,首次启动跟随系统,设置里可切换。
- **无头 CLI** 供脚本与 AI 代理使用(`--start`、`--status`、`--tool-install`、`--tool-launch`),另有 `mrc-postprocess` CLI 重跑后处理。

## 环境要求

- Windows 10/11 x64
- 构建:Rust 2021 + CMake + libclang(`pip install cmake libclang`;`LIBCLANG_PATH` 已在 `.cargo/config.toml` 预置指向 pip 提供的 clang)
- 转写:[whisper.cpp ggml 模型](https://huggingface.co/ggerganov/whisper.cpp)(中文建议 `ggml-small.bin` 起)
- 可选:python + `pyannote.audio`(说话人分离);任意 OpenAI 兼容 API key(LLM 总结)

## 从源码构建

```bash
git clone https://github.com/chenghaowan/rs-meetingrecorder-craft.git
cd rs-meetingrecorder-craft
cargo build --release
# 产物:target/release/mrc-recorder.exe(GUI + 无头 CLI)
#      target/release/mrc-postprocess.exe(后处理 CLI)
```

## 快速开始

```bash
mrc-postprocess init-config        # 生成 %APPDATA%\ltl-meetingrecord\config.toml(填 [llm].api_key 等)
mrc-recorder                       # GUI;F9 开始/结束会议,录音中底部出字幕
mrc-recorder --start --title "DFM评审"   # 无头:录音到回车,结束自动后处理
```

会议结束后流水线自动执行:分离 → 命名(需要时弹窗)→ 切块 → 截图匹配 → LLM 总结 → 会议包落在 `%APPDATA%\ltl-meetingrecord\packages\`。

## CLI 参考

```bash
mrc-recorder --start [--title X]   # 无头录音;stdout 输出 JSON 回执(退出码 0/1/2)
mrc-recorder --status              # 最近 10 场会议 + 截图工具位置(JSON)
mrc-recorder --tool-install [来源] # 安装/更新 PointToFix Craft(URL 或本地路径)
mrc-recorder --tool-launch         # 启动截图工具

mrc-recorder --transcribe <wav>        # 无头离线转写(whisper 本地),JSON 段落输出
mrc-postprocess list               # 会议列表
mrc-postprocess run last           # (重)跑后处理;交互式命名
mrc-postprocess run 3 --no-llm     # 不跑 LLM,只重建包
mrc-postprocess speak 3 SPEAKER_00 # 看某人样例发言(帮对号)
mrc-postprocess name 3 SPEAKER_00 张三
mrc-postprocess rag 3              # 重试 RAG 入库
```

AI 代理技能(`.agents/skills/meetingrecord`)文档化了数据库/会议包契约,供自动化检索使用。

## 字幕翻译与窗口截图
- **本地模型翻译**(`provider = "wanai"`):路由到 WanAI Runtime 网关(rs-aiprivaderCraft,`http://127.0.0.1:17800`)的 `/translate`,跑本地 opus-mt ONNX 模型,完全离线。网关 M3 翻译端点就位前,该 provider 自动降级为不翻译。`provider = "llm"` 为云端备选。


- **字幕实时翻译**(`[translate]`):`provider = "llm"` 复用 `[llm]` 的 OpenAI 兼容端点(统一模型网关 rs-aiprivaderCraft 就位后指向它即可,零改动切换);`"baidu"` 走百度翻译开放 API(与 ltl-manage 同一套 appid/secret);`"none"` 关闭。llm 模式按 3~5 段攒批翻译,术语按行业惯例保留;译文写进 `segments.translation`、字幕条第二行、`transcript.json`。
- **指定窗口截图**:窗口标题关键词 → DWM 精确可见矩形 → 复用 pointtofixcraft 无头 `--capture` 落库。三处入口:CLI `mrc-recorder --shot-window 飞书 [--shot-window-ocr]`;面板录音中 📷 按钮;`auto_window_secs` 定时自动截图(防漏截)。被遮挡/最小化的窗口截到的是屏幕该区域当前内容(区域截图本质限制);窗口内容级截取在二期。

## 与兄弟工具的契约

- **PointToFix Craft**:本工具**只读**打开 `%APPDATA%\pointtofix\gallery.db`(`shots` 表:`created_at` unix 毫秒、`file_path` 相对 `gallery\`、含 OCR 文本);图片复制进会议包。
- **本地 RAG 服务**:`POST {base_url}/ingest`,body 为 `{meeting_id, docs:[{id,type,text,metadata}]}`;按文档 id 幂等(`m3_chunk_000`、`m3_action_00`…)。

## 许可

[MIT](LICENSE) © 2026 chenghao.wan
