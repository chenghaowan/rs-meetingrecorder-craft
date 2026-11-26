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

rs-meetingrecorder-craft 是本地工具链里专管"会议纪要"的那一半:刻意**不做**在线会议、不做音视频通话,也**不做 RAG**——会后产物就是一份人类可读的 `minutes.md` 会议纪要。截图由兄弟工具 [PointToFix Craft](https://gitee.com/chenghao_wan/pointtofixcraft) 负责(热键分工:**F8 = 截图,F9 = 开始/结束会议**)。一切都在本机运行;除非你把 LLM 总结指向云端 API,音频不会离开你的电脑。

## 流水线

```
(会议进行中)                            (会议结束)
CPAL 麦克风 + WASAPI 环回                Meeting Post Process
  ↓                                        ↓
PCM 双路缓冲(现场/远端)                  Chunk(5 分钟 / 1000 token)
  ↓                                        ↓
本地 SenseVoice + silero-VAD 切句        截图 ↔ 发言匹配(±30 秒)
(进程内,按自然语句)                        ↓
  ↓                                      LLM 一次性总结,输出结构化 JSON
说话人分离(本地 ONNX)                      ↓
  ↓                                     transcript.json · summary.json
meetings.db                     →       actions.json · decisions.json
(并行,独立软件)                            ↓
F8 截图 → gallery.db                     minutes.md 会议纪要
```

## 功能

- **四层全本地,零网关**:采集(cpal 麦克风 + WASAPI 环回)→ 转写(SenseVoice 进程内 ONNX)→ 说话人(pyannote-segmentation + CAM++ ONNX,全 CPU)→ 翻译(llama.cpp Vulkan + Hy-MT3,可选)。全部跑在本机。
- **双路采集**:麦克风录现场(`source='mic'`),WASAPI 环回抓系统输出——Teams 等会议软件的远端声音(`source='remote'`),两路并行转写、双 WAV 归档。
- **唯一 STT 引擎,全本地**:进程内 SenseVoice-small(sherpa-onnx ONNX 跑 CPU,数倍于实时,自带标点,zh/en/ja/ko/yue),内嵌 silero-VAD(630KB 随二进制分发)按自然语句切句。模型首次自动下载(~230 MB,经 hf-mirror),由本软件自行管理。
- **悬浮小面板**(132×96,与 PointToFix Craft 待机条同尺寸):状态灯、计时、开始/结束、**✂ 一键截图**(仅录音中;设置了截图窗口关键词就只截该窗口——如 Teams——否则全屏;直拍进当前会议,散会自动进会议包),置顶可配,可拖拽。
- **屏幕底部字幕条**(第二个原生视口):最近两条转写 + 录音计时;置顶/字号/透明度/底部边距可配。
- **流式转写**:silero-VAD 按语音活动切句(延迟低,不用等固定窗口);无 VAD 时回退固定窗口模式。失败窗口自动重试,音频不丢。
- **务实的说话人命名**:开会时**实时标注说话人 A/B/C**(逐段 CAM++ 声纹聚类,现场/远端同一人自动归同簇;阈值同 [diarize]),面板转写列表里**点 A 即可当场改成真名**(字幕、列表、会议包同步生效);会议结束的命名窗照常收尾(样例发言 + 历史名单一键点选),并用语言线索纠偏(短应答继承/回合约束/称呼反推);**声纹匹配**跨会议自动预填(实时簇质心随会议入库,向量存 SQLite BLOB,余弦暴力扫;跨模型质心永不比对);无 VAD 的回退模式仍走会后离线分离(pyannote-segmentation,产出 SPEAKER_00…)。
- **LLM 一次成稿,API/本地双模式**:整场一次性总结,强制输出结构化 JSON(summary / decisions / actions / risks,每条带 evidence_time 溯源);超长自动降级为两段总结 + 一次合并。`[llm].provider` 二选一:`"api"`(任何 OpenAI 兼容端点)或 `"local"`(llama-server + 本地 GGUF,离线)。本地档**按独显显存自动推荐**(DXGI 探测,设置里一键下载到 `data_dir\models\llm`):≥11 GB → Qwen3-8B@32k;≥7.5 GB → Qwen3-8B@16k;≥5 GB → Qwen3-4B@16k;核显/无独显 → Qwen3-1.7B@16k(轻薄本 CPU/核显也能跑)。
- **会议包** —— 纯文件,零锁定:
  ```
  meeting_20261105/
  ├── minutes.md           # 会议纪要(人类可读:总结/决议/行动项/风险 + 转写全文 + 截图)
  ├── transcript.json      # 全量段落 + 说话人 + chunk 索引
  ├── summary.json         # LLM 总结(只存 JSON,展示时再转 Markdown)
  ├── actions.json  decisions.json  risks.json
  ├── screenshots.json     # 每张截图 {"image","ts","context"}
  ├── screenshots/         # img001.png…(复制快照)
  └── meta.json
  ```
- **内置 Nomo 阅读器**:便携包自带单文件 Markdown 阅读器 [Nomo](https://github.com/nomo-md/nomo)(`thirdpart\Nomo\`,AGPL-3.0 原样分发,独立进程调用,不影响本项目许可)。散会后展开面板点**「Nomo 查看」**,纪要 + 截图一屏看完——minutes.md 里的截图是标准 markdown 相对路径引用(不内嵌 base64:Tinta/Nomo 等阅读器不支持 data URI),任何支持图片的 Markdown 阅读器打开同样有效;无 Nomo 时该按钮自动隐藏。
- **设置界面**(可拖拽缩放,内容滚动):转写语言下拉(auto/zh/en/ja/ko/yue)、说话人分离开关、总结服务 API/本地切换、字幕选项、chunk 参数、字幕翻译(目标语言下拉)—— 内置 **PointToFix Craft 安装器**(.exe 直链 / 本地路径 / 自动探测兄弟仓库便携包,附双端 release 下载链接)。
- **国际化**:中文 / English / Deutsch / 日本語,首次启动跟随系统,设置里可切换。
- **无头 CLI** 供脚本与 AI 代理使用(`--start`、`--status`、`--tool-install`、`--tool-launch`、`--fetch-models`),另有 `mrc-postprocess` CLI 重跑后处理。

## 环境要求

- Windows 10/11 x64
- 构建:Rust 2021(无需 CMake/libclang —— whisper.cpp 已移除)
- 转写:`mrc-recorder --fetch-models` 下载一次本地 SenseVoice 模型(hf-mirror;开会缺模型时也会自动下载)
- LLM 总结(二选一):任意 OpenAI 兼容 API key;或本地 GGUF——设置里"下载推荐模型"**按本机独显显存自动选档**(≥11GB→Qwen3-8B 4.7GB / ≥7.5GB→8B@16k / ≥5GB→[Qwen3-4B](https://hf-mirror.com/unsloth/Qwen3-4B-Instruct-2507-GGUF) 2.3GB / 核显→[Qwen3-1.7B](https://hf-mirror.com/unsloth/Qwen3-1.7B-GGUF) 1.0GB),`[llm.local].server_exe` 留空复用字幕翻译的 llama-server(Vulkan 包,[llama.cpp releases](https://github.com/ggml-org/llama.cpp/releases);无 GPU 时自动走 CPU)
- 可选:Hy-MT3 GGUF(放 `%APPDATA%\ltl-meetingrecord\models\hy-mt3\` 即可离线 GPU 翻译)

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
mrc-postprocess init-config        # 生成 %APPDATA%\ltl-meetingrecord\config.toml(配置 [llm])
mrc-recorder                       # GUI;F9 开始/结束会议,录音中底部出字幕
mrc-recorder --start --title "DFM评审"   # 无头:录音到回车,结束自动后处理
```

会议结束后流水线自动执行:分离 → 命名(需要时弹窗)→ 切块 → 截图匹配 → LLM 总结 → 会议包(含 minutes.md)落在 `%APPDATA%\ltl-meetingrecord\packages\`。

## CLI 参考

```bash
mrc-recorder --start [--title X]   # 无头录音;stdout 输出 JSON 回执(退出码 0/1/2)
mrc-recorder --status              # 最近 10 场会议 + 截图工具位置(JSON)
mrc-recorder --tool-install [来源] # 安装/更新 PointToFix Craft(URL 或本地路径)
mrc-recorder --tool-launch         # 启动截图工具
mrc-recorder --fetch-models        # 下载本地 SenseVoice 模型(~230 MB,hf-mirror)

mrc-recorder --transcribe <wav>        # 无头转写(本地 SenseVoice),JSON 段落输出
mrc-postprocess list               # 会议列表
mrc-postprocess run last           # (重)跑后处理;交互式命名
mrc-postprocess run 3 --no-llm     # 不跑 LLM,只重建包(含 minutes.md)
mrc-postprocess speak 3 SPEAKER_00 # 看某人样例发言(帮对号)
mrc-postprocess name 3 SPEAKER_00 张三
```

AI 代理技能(`.agents/skills/meetingrecord`)文档化了数据库/会议包契约,供自动化检索使用。

## 字幕翻译与窗口截图
- **字幕实时翻译**(`[translate]`):源语言**自动识别**(Hy-MT3 模板本就不需要源语言,百度 API 用 from=auto),只配目标语言。`provider = "local"` 走本机 llama.cpp Vulkan 服务 + Hy-MT3 GGUF(离线、GPU、自动拉起 127.0.0.1:17810,官方 Hy-MT 提示词模板逐段直翻);`"api"` 复用 `[llm]` 的 OpenAI 兼容端点(按 3~5 段攒批,术语按行业惯例保留;配置键仍为 `"llm"`);`"baidu"` 走百度翻译开放 API;`"none"` 关闭。译文写进 `segments.translation`、字幕条第二行、`transcript.json`。
- **指定窗口截图**:窗口标题关键词 → DWM 精确可见矩形。**会议中的截图(✂ 全屏 / 📷 指定窗口 / 定时自动)一律"会议直拍"**:经 pointtofixcraft 无头 CLI `--out` 直接写 `data_dir\shots\meeting_<id>\`,**不进 pointtofix 画廊**;散会拷贝进会议包 `screenshots/` 并按时间关联发言。pointtofix 画廊保持只读,用于关联你在 pointtofixcraft 里自己按 F8 截的图(与直拍按时间合并、统一编号)。CLI `mrc-recorder --shot-window 飞书 [--shot-window-ocr]` 仍走画廊(独立用途)。被遮挡/最小化的窗口截到的是屏幕该区域当前内容(区域截图本质限制);窗口内容级截取在二期。

## 与兄弟工具的契约

- **PointToFix Craft**:本工具只复用其**无头截图 CLI**(`--capture x,y,w,h` / `--capture-full`,加 `--out` 直写文件不落库);会议中的截图不再写它的 gallery.db,画廊只读用于关联你自己截的图。便携包在 [GitHub Releases](https://github.com/chenghaowan/pointtofixcraft/releases) 与 [Gitee Releases](https://gitee.com/chenghao_wan/pointtofixcraft/releases)。

## 许可

[MIT](LICENSE) © 2026 chenghao.wan
