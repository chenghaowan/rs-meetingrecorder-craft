//! 配置:%APPDATA%\ltl-meetingrecord\config.toml,缺省值全可跑。
//! 环境变量 LTL_MR_CONFIG 可指向任意路径(测试/多实例用)。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SttCfg {
    /// "sensevoice"(本地 sherpa-rs,进程内 ONNX) | "none"(只录音归档)
    pub provider: String,
    /// 模型目录(需 model.int8.onnx + tokens.txt);留空自动找
    /// data_dir/models/sensevoice、exe 旁 models/sensevoice、cwd models/sensevoice
    pub model_dir: Option<PathBuf>,
    /// 识别语言:auto/zh/en/ja/ko/yue
    pub language: String,
    /// 推理线程数
    pub num_threads: u32,
    /// 实时解码窗口秒数:攒够这么长就送本地引擎,并把已确定的尾巴提交
    pub window_secs: f64,
}

impl Default for SttCfg {
    fn default() -> Self {
        Self {
            provider: "sensevoice".into(),
            model_dir: None,
            language: "auto".into(),
            num_threads: 2,
            window_secs: 12.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DiarizeCfg {
    /// 失败/未配置时:跳过说话人分离,转写保持 SPEAKER 缺省
    pub enabled: bool,
    /// 聚类阈值(簇数未知走阈值模式;越小簇越少)
    pub threshold: f32,
    /// 跨流(mic/remote)簇质心合并的余弦阈值
    pub cross_match: f32,
}

impl Default for DiarizeCfg {
    fn default() -> Self {
        Self { enabled: true, threshold: 0.5, cross_match: 0.4 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmCfg {
    /// OpenAI 兼容端点,如 https://dashscope.aliyuncs.com/compatible-mode/v1
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub temperature: f32,
    pub timeout_secs: u64,
    /// 模型可用上下文 token(触发两段式降级的阈值按此×0.8)
    pub context_tokens: usize,
    /// true=response_format json_schema;部分端点不支持时改 false(用 json_object+提示词)
    pub json_schema: bool,
    pub max_retries: u32,
}

impl Default for LlmCfg {
    fn default() -> Self {
        Self {
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
            api_key: String::new(),
            model: "qwen-long".into(),
            temperature: 0.2,
            timeout_secs: 300,
            context_tokens: 200_000,
            json_schema: true,
            max_retries: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ShotsCfg {
    /// pointtofixcraft 的 gallery.db;留空=%APPDATA%\pointtofix\gallery.db
    pub db_path: Option<PathBuf>,
    /// gallery 图片根目录;留空=%APPDATA%\pointtofix\gallery
    pub root: Option<PathBuf>,
    /// 截图时间点前后各取多少秒的发言做 context
    pub context_window_secs: f64,
    /// 指定窗口截图:窗口标题关键词(如 "飞书");留空=面板📷/CLI 需显式给
    pub default_window: String,
    /// 会议中自动截指定窗口的间隔秒;0=关闭(防漏截,截图落 gallery 由会议包自动关联)
    pub auto_window_secs: f64,
}

impl Default for ShotsCfg {
    fn default() -> Self {
        Self {
            db_path: None,
            root: None,
            context_window_secs: 30.0,
            default_window: String::new(),
            auto_window_secs: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RagCfg {
    /// ym3dRagCraft 的 /ingest 基址,如 http://127.0.0.1:8720;留空=不入库
    pub base_url: Option<String>,
    pub timeout_secs: u64,
}

impl Default for RagCfg {
    fn default() -> Self {
        Self { base_url: None, timeout_secs: 60 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PostCfg {
    /// chunk 时间窗(秒)
    pub chunk_window_secs: f64,
    /// chunk token 上限(近似)
    pub chunk_token_limit: usize,
    /// 声纹匹配阈值:>=match 自动预填;[match, uncertain) 标疑似;<uncertain 视为新人
    pub voiceprint_match: f32,
    pub voiceprint_uncertain: f32,
    /// 每人保留的声纹样本上限(超出淘汰最旧的)
    pub voiceprint_max_samples: usize,
}

impl Default for PostCfg {
    fn default() -> Self {
        Self {
            chunk_window_secs: 300.0,
            chunk_token_limit: 1000,
            voiceprint_match: 0.65,
            voiceprint_uncertain: 0.55,
            voiceprint_max_samples: 10,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureCfg {
    /// 采集现场麦克风
    pub mic_enabled: bool,
    /// 环回采集系统输出声(Teams 等会议软件的远端声音)
    pub loopback_enabled: bool,
    /// 输出设备友好名子串;留空 = 默认渲染设备
    pub loopback_device: String,
}

impl Default for CaptureCfg {
    fn default() -> Self {
        Self { mic_enabled: true, loopback_enabled: true, loopback_device: String::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// meetings.db 与会议包根目录所在数据目录
    pub data_dir: Option<PathBuf>,
    /// 录音归档目录(默认 data_dir/audio)
    pub audio_dir: Option<PathBuf>,
    /// 会议包根目录(默认 data_dir/packages)
    pub packages_dir: Option<PathBuf>,

    /// STT(SenseVoice 本地 sherpa-rs;alias 兼容旧 [whisper] 段)
    #[serde(alias = "whisper")]
    pub stt: SttCfg,
    /// 采集(麦克风 + 环回)
    pub capture: CaptureCfg,
    pub diarize: DiarizeCfg,
    pub llm: LlmCfg,
    pub shots: ShotsCfg,
    pub rag: RagCfg,
    pub post: PostCfg,
    /// 主悬浮窗置顶
    pub main_on_top: bool,
    /// 界面语言(zh/en/de/ja);None = 首次跟随系统
    pub lang: Option<String>,
    /// 字幕实时翻译
    pub translate: TranslateCfg,
    /// 底部字幕条
    pub subtitle: SubtitleCfg,
    /// 外部工具(pointtofixcraft 截图)
    pub tools: ToolsCfg,
}

/// 字幕实时翻译配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TranslateCfg {
    /// "llm"(复用 [llm] 端点) | "baidu" | "none"
    pub provider: String,
    pub from_lang: String,
    pub to_lang: String,
    /// llm provider:每次调用翻译的段数
    pub batch_size: usize,
    /// baidu 凭据
    pub appid: String,
    pub secret: String,
    pub api_url: String,
}

impl Default for TranslateCfg {
    fn default() -> Self {
        Self {
            provider: "none".into(),
            from_lang: "zh".into(),
            to_lang: "en".into(),
            batch_size: 4,
            appid: String::new(),
            secret: String::new(),
            api_url: "https://fanyi-api.baidu.com/api/trans/vip/translate".into(),
        }
    }
}

/// 外部工具配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolsCfg {
    /// pointtofixcraft 获取来源:http(s) .exe 直链,或本地路径(便携 exe 或含 exe 的目录)。
    /// 留空 = 自动探测兄弟仓库 dist(…\pointtofixcraft\dist\pointtofix-portable\pointtofix.exe)
    pub pointtofix_url: Option<String>,
    /// 已安装位置覆盖;留空 = %APPDATA%\ltl-meetingrecord	ools\pointtofix\pointtofix.exe
    pub pointtofix_exe: Option<PathBuf>,
}

impl Default for ToolsCfg {
    fn default() -> Self {
        Self { pointtofix_url: None, pointtofix_exe: None }
    }
}

/// 底部字幕条设置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SubtitleCfg {
    pub enabled: bool,
    /// 窗口置顶(可被全屏游戏压住的就是 false)
    pub always_on_top: bool,
    /// 字号(逻辑像素)
    pub font_size: f32,
    /// 背景不透明度 0~1
    pub opacity: f32,
    /// 距屏幕底部边距(逻辑像素)
    pub bottom_margin: f32,
}

impl Default for SubtitleCfg {
    fn default() -> Self {
        Self { enabled: true, always_on_top: true, font_size: 26.0, opacity: 0.55, bottom_margin: 48.0 }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: None,
            audio_dir: None,
            packages_dir: None,
            stt: SttCfg::default(),
            capture: CaptureCfg::default(),
            diarize: DiarizeCfg::default(),
            llm: LlmCfg {
                base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
                api_key: String::new(),
                model: "qwen-long".into(),
                temperature: 0.2,
                timeout_secs: 300,
                context_tokens: 200_000,
                json_schema: true,
                max_retries: 3,
            },
            shots: ShotsCfg {
                db_path: None,
                root: None,
                context_window_secs: 30.0,
                default_window: String::new(),
                auto_window_secs: 0.0,
            },
            rag: RagCfg {
                base_url: None,
                timeout_secs: 60,
            },
            post: PostCfg {
                chunk_window_secs: 300.0,
                chunk_token_limit: 1000,
                voiceprint_match: 0.65,
                voiceprint_uncertain: 0.55,
                voiceprint_max_samples: 10,
            },
            main_on_top: true,
            lang: None,
            subtitle: SubtitleCfg::default(),
            tools: ToolsCfg::default(),
            translate: TranslateCfg::default(),
        }
    }
}

fn appdata() -> PathBuf {
    std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

impl Config {
    pub fn config_path() -> PathBuf {
        if let Ok(p) = std::env::var("LTL_MR_CONFIG") {
            return PathBuf::from(p);
        }
        appdata().join("ltl-meetingrecord").join("config.toml")
    }

    /// 读取配置;文件不存在返回默认值(首次运行友好)
    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("读取配置失败: {}", path.display()))?;
        let cfg: Config = toml::from_str(&text)
            .with_context(|| format!("解析配置失败: {}", path.display()))?;
        Ok(cfg)
    }

    pub fn save_example(to: &Path) -> Result<()> {
        let text = toml::to_string_pretty(&Self::default())?;
        if let Some(p) = to.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(to, text)?;
        Ok(())
    }

    pub fn data_dir(&self) -> PathBuf {
        self.data_dir
            .clone()
            .unwrap_or_else(|| appdata().join("ltl-meetingrecord"))
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir().join("meetings.db")
    }

    pub fn audio_dir(&self) -> PathBuf {
        self.audio_dir
            .clone()
            .unwrap_or_else(|| self.data_dir().join("audio"))
    }

    pub fn packages_dir(&self) -> PathBuf {
        self.packages_dir
            .clone()
            .unwrap_or_else(|| self.data_dir().join("packages"))
    }
}
