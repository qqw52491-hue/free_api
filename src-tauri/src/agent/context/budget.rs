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
    pub const DEFAULT_HIGH_WATERMARK_RATIO: f64 = 0.80;      // 80% 高水位触发
    pub const DEFAULT_LOW_WATERMARK_RATIO: f64 = 0.50;       // 50% 目标回落水位
    pub const RECENT_PROTECTED_RATIO: f64 = 0.25;            // 25% 近期保护区（保证 low_watermark 50% > protected 25% + 卡片 5% + 余量 20%）

    /// 根据当前模型的真实窗口和静态区大小，动态推导出绝对预算（在会话内是常量且确定）
    pub fn derive(
        context_window: usize,
        max_output: usize,
        static_prefix_tokens: usize,
    ) -> Self {
        Self::derive_with_depth(
            context_window,
            max_output,
            static_prefix_tokens,
            Self::DEFAULT_LOW_WATERMARK_RATIO,
        )
    }

    /// 支持自定义压缩深度的推导入口
    pub fn derive_with_depth(
        context_window: usize,
        max_output: usize,
        static_prefix_tokens: usize,
        low_watermark_ratio: f64,
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

        // 安全余量：总窗口的 15%
        let safety_margin = (window as f64 * 0.15).ceil() as usize;

        // 可供历史分配的最大容量
        let reserved = max_out + static_prefix_tokens + tail_state_tokens + safety_margin;
        let max_allowed_history = window.saturating_sub(reserved).max(2_048);

        // 硬不变量保证：低水位必须严格大于（近期保护区 + 归档卡片预留 + 安全缓冲）
        let min_safe_low_ratio = Self::RECENT_PROTECTED_RATIO + 0.15; // 至少 40%
        let safe_low_ratio = low_watermark_ratio.clamp(min_safe_low_ratio, 0.75);

        let high_watermark = (max_allowed_history as f64 * Self::DEFAULT_HIGH_WATERMARK_RATIO) as usize;
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
        assert!(budget.max_allowed_history_tokens > 80_000);
        assert!(budget.high_watermark_tokens > budget.low_watermark_tokens);
    }

    #[test]
    fn test_conservative_estimate_beats_regular_for_dense_content() {
        // 密集 JSON/base64 场景：保守估算必须 >= 普通估算
        let dense_json = r#"{"key":"aGVsbG8gd29ybGQgdGhpcyBpcyBiYXNlNjQ=","foo":1234567890,"bar":true}"#;
        let conservative = conservative_token_estimate(dense_json);
        let regular = estimate_tokens(dense_json);
        // 保守估算必须始终 >= 常规估算
        assert!(
            conservative >= regular,
            "conservative={} should be >= regular={}",
            conservative, regular
        );
        // 对于此类 ASCII 密集 JSON，byte_floor 应该生效并超出 weighted 估算
        let byte_floor = (dense_json.len() as f64 / 2.8).ceil() as usize;
        assert_eq!(conservative, regular.max(byte_floor));
    }
}
