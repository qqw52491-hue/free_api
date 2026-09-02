use serde::{Deserialize, Serialize};

/// 规范化模型家族分类
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelFamily {
    /// Kimi 原生深度推理模型 (如 kimi-k3, kimi-k1.5 等)
    /// 特性: 严禁非 1 的 temperature，严禁非 0.95 的 top_p，严禁非 0 的 penalty。
    /// 支持 reasoning_effort: low, medium, high, max (默认 max)
    KimiReasoning,

    /// OpenAI o 系列 / GPT-5 深度推理模型 (如 o1..o5, gpt-5.1, gpt-5.6 sol/terra 等)
    /// 特性: 严禁自定义采样参数 temperature/top_p/penalty，必须使用 max_completion_tokens 代替 max_tokens。
    /// 支持 reasoning_effort: none, minimal, low, medium, high, xhigh, max
    OpenAiReasoning,

    /// DeepSeek R 系列 / 开源深度推理模型 (如 deepseek-r1, r2, r3, qwq-32b 等)
    /// 特性: 严禁采样参数 (temperature/top_p) 与 logprobs，历史消息自动剔除 reasoning_content
    DeepSeekReasoning,

    /// Anthropic Claude 思考模型 (如 claude-3-7, claude-4, claude-5 fable/opus-5 等)
    /// 特性: 启用思考时 temperature 必须为 1.0，使用 budget_tokens 控制推理量，且 max_tokens > budget_tokens
    ClaudeThinking,

    /// Google Gemini 系列模型 (如 gemini-2.5, gemini-3.0, gemini-3.7 等)
    /// 特性: 遵循标准通用协议，完美支持结构化与常规采样
    GoogleGemini,

    /// 标准通用对话模型 (如 gpt-4o, deepseek-v3, qwen, grok, doubao 等)
    /// 特性: 支持常规 temperature, top_p, max_tokens, response_format
    StandardChat,
}

impl ModelFamily {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::KimiReasoning => "Kimi / Moonshot 推理协议",
            Self::OpenAiReasoning => "OpenAI 新一代推理协议",
            Self::DeepSeekReasoning => "DeepSeek / 开源推理协议",
            Self::ClaudeThinking => "Anthropic 思考预算协议",
            Self::GoogleGemini => "Google Gemini 标准协议",
            Self::StandardChat => "标准通用对话协议",
        }
    }
}
