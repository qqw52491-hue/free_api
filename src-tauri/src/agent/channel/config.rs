//! 通道配置：channel-config.json 读写、默认值与 scratch 工作区解析。
//! 从 channel.rs 原样抽出，行为不变。

use super::*;

pub(crate) const DEFAULT_API_PORT: u16 = 8790;
pub(crate) const DEFAULT_BRIDGE_PORT: u16 = 8791;
pub(crate) const SCRATCH_PROJECT_NAME: &str = "wechat-scratch";
pub(crate) const DEFAULT_HEARTBEAT_MINUTES: u64 = 30;
pub(crate) const MAX_RUN_RECORDS: usize = 200;
pub(crate) const MAX_GOAL_PREVIEW_CHARS: usize = 120;
pub(crate) const MAX_RESULT_CHARS: usize = 24_000;


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    /// 桥 ↔ 本机 API 的共享令牌
    pub token: String,
    /// 本机 axum API 端口
    #[serde(default = "default_api_port")]
    pub port: u16,
    /// Node 桥 HTTP 端口
    #[serde(default = "default_bridge_port")]
    pub bridge_port: u16,
    /// 未绑定任务的工作区落点
    #[serde(default)]
    pub scratch_dir: String,
    /// 创建会话时登记的模型（也是默认路由基线）
    #[serde(default)]
    pub model_id: String,
    /// 路由三槽：pro / flash / vision（模型 UUID，空则沿用会话模型）
    #[serde(default)]
    pub pro: String,
    #[serde(default)]
    pub flash: String,
    #[serde(default)]
    pub vision: String,
    /// 心跳汇报间隔（分钟）
    #[serde(default = "default_heartbeat_minutes")]
    pub heartbeat_minutes: u64,
    /// 首次联系推送的帮助文案
    #[serde(default = "default_help_text")]
    pub help_text: String,
    /// 应用启动时自动拉起微信桥
    #[serde(default = "default_true")]
    pub auto_start_bridge: bool,
    /// 微信发送者白名单（wxid 列表）；空表 = 自动锁定第一条消息的发送者为主人
    #[serde(default)]
    pub allowlist: Vec<String>,
    /// 微信任务的瞬时 LLM 请求重试次数；None = 跟随系统默认（3，上限 100）。
    /// API 不稳定时调高：评测路径基线为 20。
    #[serde(default)]
    pub llm_retries: Option<u32>,
    /// 动作里程碑汇报步长：每 N 个动作推一条"正在干啥"；0 = 关闭
    #[serde(default = "default_action_milestone")]
    pub action_milestone_steps: u32,
    /// 阶段里程碑（环境就绪/失败恢复/救援/交付）是否推送
    #[serde(default = "default_true")]
    pub phase_pushes: bool,
    /// reviewer 僵尸行巡检宽限（秒）。异构大家族模型长上下文推理较慢时可调高；
    /// None = 默认 600s，上限 3600s。
    #[serde(default)]
    pub reviewer_reap_grace_secs: Option<u64>,
    /// 门禁档位预设：L1 轻量（仅收尾审查+机械门禁）/ L2 标准（方案+阶段+收尾，生产默认）/
    /// L3 全量（L2 + 每次写入必审）。环境变量 FREE_API_GATE_PROFILE 优先于本字段；
    /// None = L2。L3 的双审查员会签/红队预演为未建增强项，当前 L3 = L2 + 最高强度参数。
    #[serde(default)]
    pub gate_profile: Option<String>,
    /// 阶段级写入审查开关（仅交叉审查/审批模式生效；None = 开启）
    #[serde(default)]
    pub stage_review_enabled: Option<bool>,
    /// 行为兜底触发阈值：距上次阶段审查累计写入 N 个文件即触发（防 todo 记账博弈）。
    /// None = 默认 6。
    #[serde(default)]
    pub stage_review_max_files: Option<u32>,
    /// reviewer 只读取证步数预算（read_file/grep_search/git_diff/git_log/list_dir）。
    /// 0 = 关闭（回到"瞎子"模式，只评估证据包）；None = 默认 5。
    #[serde(default)]
    pub reviewer_tool_steps: Option<u32>,
    /// 宿主执行验证命令（如 "cargo test"）：收尾审查前宿主亲自执行，
    /// 真实退出码与输出尾部注入 reviewer 证据包。空 = 关闭。
    #[serde(default)]
    pub verify_test_command: Option<String>,
    /// 宿主执行验证超时（秒）。None = 默认 120，上限 600。
    #[serde(default)]
    pub verify_timeout_secs: Option<u64>,
    /// 网页控制台对外访问基础地址（内网穿透域名，如 https://xxx.example.com）；
    /// 空 = 自动探测局域网 IPv4。用于生成扫码直达的二维码
    #[serde(default)]
    pub console_base_url: Option<String>,
    /// 单个任务允许向用户发起信息类提问的最大配额（防琐碎问题没完没了；None = 默认 5）
    #[serde(default)]
    pub max_human_inquiry_budget: Option<u32>,
}

fn default_action_milestone() -> u32 {
    15
}

fn default_api_port() -> u16 {
    DEFAULT_API_PORT
}
fn default_bridge_port() -> u16 {
    DEFAULT_BRIDGE_PORT
}
fn default_heartbeat_minutes() -> u64 {
    DEFAULT_HEARTBEAT_MINUTES
}
fn default_help_text() -> String {
    // 完整指令面板由桥内置（COMMAND_PANEL，随桥能力同步），此处只是可自定义的开场白
    "🤖 SandwichAgent 微信通道已就绪。".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            token: uuid::Uuid::new_v4().to_string(),
            port: default_api_port(),
            bridge_port: default_bridge_port(),
            scratch_dir: String::new(),
            model_id: String::new(),
            pro: String::new(),
            flash: String::new(),
            vision: String::new(),
            heartbeat_minutes: default_heartbeat_minutes(),
            help_text: default_help_text(),
            auto_start_bridge: true,
            reviewer_reap_grace_secs: None,
            gate_profile: None,
            stage_review_enabled: None,
            stage_review_max_files: None,
            reviewer_tool_steps: None,
            verify_test_command: None,
            verify_timeout_secs: None,
            allowlist: Vec::new(),
            llm_retries: None,
            action_milestone_steps: default_action_milestone(),
            phase_pushes: true,
            console_base_url: None,
            max_human_inquiry_budget: None,
        }
    }
}

pub fn channel_config_path() -> PathBuf {
    // 与 Node 桥一致：CHANNEL_CONFIG_PATH 可指向独立配置文件（多实例隔离/测试）
    if let Ok(path) = std::env::var("CHANNEL_CONFIG_PATH") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    let home = dirs_next::data_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join("free-api-chat").join("channel-config.json")
}

pub fn load_channel_config() -> Result<ChannelConfig, String> {
    let path = channel_config_path();
    if !path.exists() {
        let config = ChannelConfig::default();
        save_channel_config(&config)?;
        return Ok(config);
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut config: ChannelConfig =
        serde_json::from_str(&raw).map_err(|e| format!("channel-config.json 解析失败: {e}"))?;
    if config.token.trim().is_empty() {
        config.token = uuid::Uuid::new_v4().to_string();
        save_channel_config(&config)?;
    }
    Ok(config)
}

pub fn save_channel_config(config: &ChannelConfig) -> Result<(), String> {
    let path = channel_config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let raw = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(&path, raw).map_err(|e| e.to_string())
}

fn default_scratch_dir() -> PathBuf {
    let home = dirs_next::data_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join("free-api-chat").join("wechat-scratch")
}

pub(crate) fn effective_scratch_dir(config: &ChannelConfig) -> PathBuf {
    let raw = config.scratch_dir.trim();
    if raw.is_empty() {
        return default_scratch_dir();
    }
    let expanded = if let Some(stripped) = raw.strip_prefix("~/") {
        dirs_next::home_dir().unwrap_or_default().join(stripped)
    } else {
        PathBuf::from(raw)
    };
    expanded
        .canonicalize()
        .unwrap_or_else(|_| {
            let _ = std::fs::create_dir_all(&expanded);
            expanded
        })
}

/// 获取全局配置中的信息类提问配额（默认 5 次）
pub fn max_human_inquiry_budget() -> usize {
    crate::agent::channel::channel_config_shared()
        .read()
        .ok()
        .and_then(|c| c.max_human_inquiry_budget)
        .map(|v| v as usize)
        .unwrap_or(crate::agent::checkpoint::DEFAULT_MAX_HUMAN_INQUIRY_BUDGET)
}


