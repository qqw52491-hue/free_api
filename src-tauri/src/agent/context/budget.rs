/// 零外部依赖的 Token 估算与安全预算推导层

/// 精确估算中英混合与代码文本的 Token 消耗（考虑中文字符与代码符号的权重）
pub fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }

    let mut cjk_count = 0usize;
    let mut ascii_count = 0usize;
    let mut whitespace_count = 0usize;
    let mut other_count = 0usize;

    for ch in text.chars() {
        if ch.is_ascii_whitespace() {
            whitespace_count += 1;
        } else if ch.is_ascii() {
            ascii_count += 1;
        } else if is_cjk_character(ch) {
            cjk_count += 1;
        } else {
            other_count += 1;
        }
    }

    // 中文字符: ~0.75 tokens / char (即 1.33 chars / token)
    let cjk_tokens = (cjk_count as f64 * 0.75).ceil();
    // 英文/代码: ~0.28 tokens / char (即 3.6 chars / token)
    let ascii_tokens = (ascii_count as f64 * 0.28).ceil();
    // 空白符: ~0.25 tokens / char
    let ws_tokens = (whitespace_count as f64 * 0.25).ceil();
    // 其他 Emoji/特殊符号: ~1.0 tokens / char
    let other_tokens = other_count as f64;

    let base_estimate = cjk_tokens + ascii_tokens + ws_tokens + other_tokens;
    // 叠加 15% 安全冗余量，防止边缘爆窗
    (base_estimate * 1.15).ceil() as usize
}

fn is_cjk_character(ch: char) -> bool {
    matches!(ch as u32,
        0x4E00..=0x9FFF |   // CJK Unified Ideographs
        0x3400..=0x4DBF |   // CJK Extension A
        0x20000..=0x2A6DF | // CJK Extension B
        0xF900..=0xFAFF |   // CJK Compatibility
        0x3000..=0x303F     // CJK Symbols and Punctuation
    )
}

/// 悲观安全防御型 Token 估算器（专用于上下文硬不爆窗防御）
/// 针对转义密集的 JSON、长代码、特殊符号与 Base64，叠加 byte_floor (bytes / 2.8)，取两者最大值
pub fn conservative_token_estimate(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let regular = estimate_tokens(text);
    let byte_floor = (text.len() as f64 / 2.8).ceil() as usize;
    regular.max(byte_floor)
}

use serde::{Deserialize, Serialize};

/// 上下文预算与水位配置模型
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ContextBudgetConfig {
    /// 活跃历史工作集占总窗口的比例（例如 0.75 表示允许历史占 75% 窗口）
    pub active_history_ratio: f64,
    /// 活跃历史容量硬上限（tokens），防止超大窗口（如 1M）无休止膨胀
    pub active_history_cap_max: usize,
    /// 活跃历史容量保底（tokens）
    pub active_history_cap_min: usize,
    /// 触发压缩的高水位比例（占最大允许历史的比例，例如 0.85）
    pub high_watermark_ratio: f64,
    /// 压缩后回落的低水位目标比例（例如 0.50）
    pub low_watermark_ratio: f64,
    /// 近期严格保护区比例（例如 0.25）
    pub recent_protected_ratio: f64,
    /// 防爆窗安全防御余量比例（例如 0.10）
    pub safety_margin_ratio: f64,
    /// 尾部状态卡档位（旧配置无此字段 → 默认 Full 保持原行为）
    #[serde(default = "default_state_card_full")]
    pub state_card_level: StateCardLevel,
}

const fn default_state_card_full() -> StateCardLevel {
    StateCardLevel::Full
}

/// 尾部状态卡渲染档位。
/// 门禁面向段（计划/目标块/todo/契约/验证/plan_gate）任何档位都必须渲染——
/// 不显示模型就无从闭合义务，finish 会死锁；裁剪只作用于定向辅助段
/// （失败记录/台账/取证/记忆/遥测等弱模型防打转脚手架，强模型可省缓存 miss）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StateCardLevel {
    /// 仅门禁面向段——强模型/省钱模式
    Minimal,
    /// 门禁段 + 全部定向辅助段——弱模型默认
    Full,
}

impl Default for ContextBudgetConfig {
    /// 默认长程编码模式（Deep Coding Preset）：对齐 Claude Code / Cursor，最大化长上下文记忆
    fn default() -> Self {
        Self::deep_coding()
    }
}

impl ContextBudgetConfig {
    /// 预设 1：长程深度编码模式（Deep Coding，推荐默认）
    /// 允许使用 75% 窗口，85% 高水位触发，50% 低水位回落，充分保留数十步的完整代码上下文
    pub const fn deep_coding() -> Self {
        Self {
            active_history_ratio: 0.75,
            active_history_cap_max: 300_000,
            active_history_cap_min: 24_000,
            high_watermark_ratio: 0.85,
            low_watermark_ratio: 0.50,
            recent_protected_ratio: 0.25,
            safety_margin_ratio: 0.10,
            state_card_level: StateCardLevel::Full,
        }
    }

    /// 预设 2：省流均衡模式（Cost Saving）
    /// 适合按 Token 计费昂贵的模型，更积极地归档压缩以节省费用
    pub const fn cost_saving() -> Self {
        Self {
            active_history_ratio: 0.50,
            active_history_cap_max: 128_000,
            active_history_cap_min: 16_000,
            high_watermark_ratio: 0.75,
            low_watermark_ratio: 0.35,
            recent_protected_ratio: 0.20,
            safety_margin_ratio: 0.15,
            state_card_level: StateCardLevel::Minimal,
        }
    }

    /// 预设 3：极致响应模式（Ultra Fast）
    /// 维持紧凑上下文，追求极快首字延迟（TTFT）
    pub const fn ultra_fast() -> Self {
        Self {
            active_history_ratio: 0.35,
            active_history_cap_max: 64_000,
            active_history_cap_min: 12_000,
            high_watermark_ratio: 0.65,
            low_watermark_ratio: 0.25,
            recent_protected_ratio: 0.15,
            safety_margin_ratio: 0.15,
            state_card_level: StateCardLevel::Minimal,
        }
    }
}

/// 动态上下文预算推导
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    pub context_window_tokens: usize,
    pub max_output_tokens: usize,
    pub static_prefix_tokens: usize,
    pub tail_state_tokens: usize,
    pub max_allowed_history_tokens: usize,
    pub high_watermark_tokens: usize,
    pub low_watermark_tokens: usize,
}

impl ContextBudget {
    pub const DEFAULT_WINDOW_TOKENS: usize = 32_768;
    pub const DEFAULT_MAX_OUTPUT_TOKENS: usize = 4_096;
    pub const DEFAULT_TAIL_STATE_TOKENS: usize = 1_000;
    pub const SINGLE_OBSERVATION_INGRESS_CAP: usize = 4_000; // 单条观测入口硬截断 4k tokens
    pub const DEFAULT_HIGH_WATERMARK_RATIO: f64 = 0.85; // 85% 高水位触发
    pub const DEFAULT_LOW_WATERMARK_RATIO: f64 = 0.50; // 50% 目标回落水位
    pub const RECENT_PROTECTED_RATIO: f64 = 0.25; // 25% 近期保护区
    /// 批量脱水低水位（历史估算 tokens）：大窗口下 85% 高水位（deep_coding
    /// ≈255k）日常任务不可达，旧输出会被逐步全量携带；超过该阈值触发一次性
    /// 批量脱水——前缀缓存只在事件点断裂一次，摊薄到后续全部步数；区别于
    /// 每步滚动改写历史（会持续击穿缓存且模型认知不稳）。
    pub const DEHYDRATION_WATERMARK_TOKENS: usize = 45_000;
    /// 低水位脱水的固定尾部保护区（tokens）：不用 recent_protected_ratio
    ///（deep_coding 下 ≈75k > 45k 水位，会让触发空转），固定小额尾窗恰好
    /// 覆盖模型的即时消费窗口（≈最近 5~8 步大输出）。
    pub const DEHYDRATION_PROTECTED_TOKENS: usize = 15_000;
    /// 扫无可脱对象时的滞后重扫步距（tokens）：避免每步空扫。
    pub const DEHYDRATION_REARM_DELTA_TOKENS: usize = 15_000;

    /// 脱水水位支持 env 覆盖（FREE_API_DEHYDRATE_WATERMARK_TOKENS），
    /// 便于按模型窗口与任务画像现场调参；非法值回落默认 45k。
    pub fn dehydration_watermark_tokens() -> usize {
        std::env::var("FREE_API_DEHYDRATE_WATERMARK_TOKENS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(Self::DEHYDRATION_WATERMARK_TOKENS)
    }

    /// 按模型上下文窗口推导活跃历史工作集上限（使用默认配置）。
    pub fn active_history_cap_for(window: usize) -> usize {
        Self::active_history_cap_with_config(window, &ContextBudgetConfig::default())
    }

    /// 根据自定义配置推导活跃历史工作集上限。
    pub fn active_history_cap_with_config(window: usize, config: &ContextBudgetConfig) -> usize {
        ((window as f64 * config.active_history_ratio) as usize)
            .clamp(config.active_history_cap_min, config.active_history_cap_max)
    }

    /// 根据当前模型的真实窗口和静态区大小，动态推导出绝对预算（使用默认 Deep Coding 模式）
    pub fn derive(context_window: usize, max_output: usize, static_prefix_tokens: usize) -> Self {
        Self::derive_with_config(
            context_window,
            max_output,
            static_prefix_tokens,
            &ContextBudgetConfig::default(),
        )
    }

    /// 支持自定义压缩深度的推导入口
    pub fn derive_with_depth(
        context_window: usize,
        max_output: usize,
        static_prefix_tokens: usize,
        low_watermark_ratio: f64,
    ) -> Self {
        let mut config = ContextBudgetConfig::default();
        config.low_watermark_ratio = low_watermark_ratio;
        Self::derive_with_config(context_window, max_output, static_prefix_tokens, &config)
    }

    /// 支持传入完整结构化配置的推导入口
    pub fn derive_with_config(
        context_window: usize,
        max_output: usize,
        static_prefix_tokens: usize,
        config: &ContextBudgetConfig,
    ) -> Self {
        let window = if context_window > 0 {
            context_window
        } else {
            Self::DEFAULT_WINDOW_TOKENS
        };
        let max_out = if max_output > 0 {
            max_output
        } else {
            Self::DEFAULT_MAX_OUTPUT_TOKENS
        };

        let tail_state_tokens = Self::DEFAULT_TAIL_STATE_TOKENS;

        let safety_margin = (window as f64 * config.safety_margin_ratio).ceil() as usize;
        let reserved = max_out + static_prefix_tokens + tail_state_tokens + safety_margin;
        let max_allowed_history = window
            .saturating_sub(reserved)
            .min(Self::active_history_cap_with_config(window, config))
            .max(2_048);

        let min_safe_low_ratio = config.recent_protected_ratio + 0.15;
        let safe_low_ratio = config.low_watermark_ratio.clamp(min_safe_low_ratio, 0.75);

        let high_watermark = (max_allowed_history as f64 * config.high_watermark_ratio) as usize;
        let low_watermark = (max_allowed_history as f64 * safe_low_ratio) as usize;

        Self {
            context_window_tokens: window,
            max_output_tokens: max_out,
            static_prefix_tokens,
            tail_state_tokens,
            max_allowed_history_tokens: max_allowed_history,
            high_watermark_tokens: high_watermark,
            low_watermark_tokens: low_watermark,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens_chinese_and_code() {
        let chinese = "这是一个用来测试大模型上下文长度的中文句子。";
        let tokens = estimate_tokens(chinese);
        assert!(tokens >= 15 && tokens <= 35);

        let code = "fn main() { println!(\"Hello, world!\"); }";
        let code_tokens = estimate_tokens(code);
        assert!(code_tokens >= 10 && code_tokens <= 25);
    }

    #[test]
    fn test_budget_derivation_deterministic() {
        let budget = ContextBudget::derive(128_000, 4_096, 5_000);
        assert_eq!(
            budget.max_allowed_history_tokens,
            ContextBudget::active_history_cap_for(128_000)
        );
        // 动态上限：窗口 × 75%，夹在 [24K, 300K]
        assert_eq!(ContextBudget::active_history_cap_for(200_000), 150_000);
        assert_eq!(ContextBudget::active_history_cap_for(1_000_000), 300_000);
        assert_eq!(ContextBudget::active_history_cap_for(32_000), 24_000);
        assert!(budget.high_watermark_tokens > budget.low_watermark_tokens);

        let small_budget = ContextBudget::derive(16_000, 2_048, 1_000);
        assert!(small_budget.max_allowed_history_tokens < 16_000);
        assert!(small_budget.high_watermark_tokens > small_budget.low_watermark_tokens);
    }

    #[test]
    fn test_conservative_estimate_beats_regular_for_dense_content() {
        // 密集 JSON/base64 场景：保守估算必须 >= 普通估算
        let dense_json =
            r#"{"key":"aGVsbG8gd29ybGQgdGhpcyBpcyBiYXNlNjQ=","foo":1234567890,"bar":true}"#;
        let conservative = conservative_token_estimate(dense_json);
        let regular = estimate_tokens(dense_json);
        // 保守估算必须始终 >= 常规估算
        assert!(
            conservative >= regular,
            "conservative={} should be >= regular={}",
            conservative,
            regular
        );
        // 对于此类 ASCII 密集 JSON，byte_floor 应该生效并超出 weighted 估算
        let byte_floor = (dense_json.len() as f64 / 2.8).ceil() as usize;
        assert_eq!(conservative, regular.max(byte_floor));
    }
}
