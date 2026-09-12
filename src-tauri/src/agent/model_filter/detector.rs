use super::family::ModelFamily;

/// 根据模型名称中固定的【品牌/基座根词】（如 kimi, qwen, qwq, gpt, claude, deepseek, gemini, grok 等）
/// 智能推导所属协议流派。全面适配最新的代际（如 GPT-5.1/5.6、Claude 4/5、DeepSeek R系列、Gemini 3.x、Kimi K3+ 等）。
pub fn detect_model_family(model_name: &str, base_url: &str) -> ModelFamily {
    let m = model_name.trim().to_lowercase();
    let u = base_url.trim().to_lowercase();

    // 1. Kimi / Moonshot 家族 (kimi-k1.5, k2, k3, k4, k5, moonshot 等)
    // 特性：严禁非 1 的 temperature 和非 0.95 的 top_p，采用 reasoning_effort 4档
    if m.contains("kimi")
        || m.contains("moonshot")
        || u.contains("sensenova.cn")
        || u.contains("moonshot.cn")
    {
        return ModelFamily::KimiReasoning;
    }

    // 2. OpenAI / ChatGPT 新一代推理家族
    // 覆盖:
    // - o系列: o1, o2, o3, o4, o5, o3-mini, o4-high 等
    // - GPT-5 系列: gpt-5, gpt-5.1, gpt-5.6, sol, terra, luna, codex-max 等深度推理架构
    // - 任何带 reason / think / codex-max 的 gpt 系列
    let has_o_series = (m.starts_with('o') && m.chars().nth(1).map(|c| c.is_ascii_digit()).unwrap_or(false))
        || m.contains("/o1") || m.contains("/o2") || m.contains("/o3") || m.contains("/o4") || m.contains("/o5")
        || m.contains("-o1") || m.contains("-o2") || m.contains("-o3") || m.contains("-o4") || m.contains("-o5");
    let has_gpt5_reasoning = m.contains("gpt-5")
        || m.contains("gpt-5.1")
        || m.contains("gpt-5.6")
        || m.contains("sol")
        || m.contains("terra")
        || m.contains("codex-max");
    if has_o_series
        || has_gpt5_reasoning
        || ((m.contains("gpt") || m.contains("chatgpt")) && (m.contains("reason") || m.contains("think")))
    {
        return ModelFamily::OpenAiReasoning;
    }

    // 3. Anthropic Claude 思考家族
    // 覆盖: Claude 3.7, Claude 4, Claude 4.5, Claude 5 (Fable, Opus 5) 以及带 thinking/reason 的模型
    if m.contains("claude")
        && (m.contains("think")
            || m.contains("reason")
            || m.contains("fable")
            || m.contains("opus-5")
            // opus-4.x 上游部署普遍默认开启 Extended Thinking，采样参数必须按思考家族清洗
            // （实测：claude-opus-4.6 带 top_p<0.95 请求直接 400 "`top_p` must be >= 0.95
            //   or unset when thinking is enabled"）
            || m.contains("opus-4")
            || m.contains("sonnet-4")
            || m.contains("sonnet-5")
            || m.contains("3-7")
            || m.contains("3.7"))
    {
        return ModelFamily::ClaudeThinking;
    }

    // 4. 开源深度推理家族 (DeepSeek R1, R2, R3, QwQ 通义推理, 以及各厂商 reasoner 模型)
    if m.contains("r1")
        || m.contains("r2")
        || m.contains("r3")
        || m.contains("reasoner")
        || m.contains("qwq")
        || m.contains("deepseek-r")
        || (m.contains("deepseek") && (m.contains("think") || m.contains("reason")))
        || (m.contains("qwen") && (m.contains("think") || m.contains("reason")))
    {
        return ModelFamily::DeepSeekReasoning;
    }

    // 5. Google Gemini 家族 (Gemini 2.5, 3.0, 3.1 Pro, 3.7 Flash 等)
    if m.contains("gemini") {
        return ModelFamily::GoogleGemini;
    }

    // 6. 通用对话家族：
    // 包括 Qwen (通义千问通用系列: qwen2.5, qwen3, qwen-max 等)、
    // ChatGPT / GPT-4o (chatgpt-4o, gpt-4o 等)、
    // DeepSeek-V3 / V4 通用 Chat、Grok (grok-3, grok-4)、
    // Doubao (豆包)、GLM (智谱) 等。
    // 统一遵循标准兼容协议：保留常规采样参数并确保 JSON 结构化输出。
    ModelFamily::StandardChat
}
