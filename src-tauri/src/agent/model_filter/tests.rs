use super::*;
use serde_json::json;

#[test]
fn test_kimi_detection_and_sanitization() {
    let family = detect_model_family("kimi-k3", "https://token.sensenova.cn/v1");
    assert_eq!(family, ModelFamily::KimiReasoning);

    let mut body = json!({
        "model": "kimi-k3",
        "temperature": 0.7,
        "top_p": 1.0,
        "max_tokens": 8192
    });

    sanitize_and_normalize_body(&mut body, family, "high", 8192);

    assert!(body.get("temperature").is_none());
    assert!(body.get("top_p").is_none());
    assert_eq!(body["reasoning_effort"], "high");
    assert_eq!(body["response_format"]["type"], "json_object");
}

#[test]
fn test_openai_o3_detection_and_sanitization() {
    let family = detect_model_family("o3-mini", "https://api.openai.com/v1");
    assert_eq!(family, ModelFamily::OpenAiReasoning);

    let mut body = json!({
        "model": "o3-mini",
        "temperature": 0.7,
        "top_p": 1.0,
        "max_tokens": 4096
    });

    sanitize_and_normalize_body(&mut body, family, "medium", 4096);

    assert!(body.get("temperature").is_none());
    assert!(body.get("top_p").is_none());
    assert!(body.get("max_tokens").is_none());
    assert_eq!(body["max_completion_tokens"], 4096);
    assert_eq!(body["reasoning_effort"], "medium");
}

#[test]
fn test_proxy_station_brand_matching() {
    let proxy_url = "https://api.my-thirdparty-proxy.com/v1";

    // 1. Qwen 通用模型 (qwen-2.5-72b, qwen-max) -> StandardChat
    let qwen_fam = detect_model_family("qwen-2.5-72b-instruct", proxy_url);
    assert_eq!(qwen_fam, ModelFamily::StandardChat);

    // 2. QwQ 推理模型 (qwq-32b-preview) -> DeepSeekReasoning
    let qwq_fam = detect_model_family("qwq-32b-preview", proxy_url);
    assert_eq!(qwq_fam, ModelFamily::DeepSeekReasoning);

    // 3. ChatGPT / GPT-4o -> StandardChat
    let chatgpt_fam = detect_model_family("chatgpt-4o-latest", proxy_url);
    assert_eq!(chatgpt_fam, ModelFamily::StandardChat);

    // 4. Kimi 在任意中转站 -> KimiReasoning
    let kimi_fam = detect_model_family("kimi-k3-chat", proxy_url);
    assert_eq!(kimi_fam, ModelFamily::KimiReasoning);

    // 5. DeepSeek R1 在中转站 -> DeepSeekReasoning
    let r1_fam = detect_model_family("deepseek-r1-distill-qwen-32b", proxy_url);
    assert_eq!(r1_fam, ModelFamily::DeepSeekReasoning);
}

#[test]
fn test_claude_thinking_budget_and_max_tokens() {
    let family = detect_model_family("claude-3-7-sonnet-thinking", "https://api.anthropic.com/v1");
    assert_eq!(family, ModelFamily::ClaudeThinking);

    let mut body = json!({
        "model": "claude-3-7-sonnet",
        "temperature": 0.7,
        "max_tokens": 4096
    });

    // 设定高思考预算 (32768)，此时原 max_tokens (4096) 必须被自动上调至 > budget
    sanitize_and_normalize_body(&mut body, family, "high", 4096);

    assert_eq!(body["temperature"], 1.0);
    assert_eq!(body["thinking"]["budget_tokens"], 32768);
    assert!(body["max_tokens"].as_i64().unwrap() > 32768);
}

#[test]
fn test_deepseek_input_reasoning_content_removal() {
    let family = detect_model_family("deepseek-reasoner", "https://api.deepseek.com/v1");
    assert_eq!(family, ModelFamily::DeepSeekReasoning);

    let mut body = json!({
        "model": "deepseek-reasoner",
        "messages": [
            {
                "role": "assistant",
                "content": "回答内容",
                "reasoning_content": "历史思考链，发给API会报400"
            }
        ],
        "logprobs": true
    });

    sanitize_and_normalize_body(&mut body, family, "auto", 4096);

    // logprobs 必须被剔除
    assert!(body.get("logprobs").is_none());
    // assistant 历史消息中的 reasoning_content 必须被移除
    assert!(body["messages"][0].get("reasoning_content").is_none());
}

#[test]
fn test_self_healing_advanced_errors() {
    let mut body = json!({
        "model": "claude-3-7-sonnet",
        "thinking": { "budget_tokens": 8192 },
        "max_tokens": 4096,
        "logprobs": true
    });

    // 测试 budget_tokens 超过 max_tokens 报错自愈
    let err1 = "API 错误 400 Bad Request: max_tokens must be greater than budget_tokens";
    assert!(self_heal_bad_request_payload(&mut body, err1));
    assert!(body["max_tokens"].as_i64().unwrap() > 8192);

    // 测试 logprobs 不支持自愈
    let err2 = "API 错误 400 Bad Request: logprobs is not supported for this model";
    assert!(self_heal_bad_request_payload(&mut body, err2));
    assert!(body.get("logprobs").is_none());

    // 测试 reasoning_effort 档位不受支持报错自愈 (如把 max 降级到 high)
    body["reasoning_effort"] = json!("max");
    let err3 = "API 错误 400 Bad Request: reasoning_effort 'max' is not supported, allowed values: [low, medium, high]";
    assert!(self_heal_bad_request_payload(&mut body, err3));
    assert_eq!(body["reasoning_effort"], "high");
}

#[test]
fn test_2026_frontier_models_detection() {
    let proxy_url = "https://api.gateway-2026.io/v1";

    // GPT-5.1 / GPT-5.6 Sol / Terra
    assert_eq!(detect_model_family("gpt-5.1-codex-max", proxy_url), ModelFamily::OpenAiReasoning);
    assert_eq!(detect_model_family("gpt-5.6-sol", proxy_url), ModelFamily::OpenAiReasoning);

    // Claude 5 Fable / Opus 5
    assert_eq!(detect_model_family("claude-fable-5", proxy_url), ModelFamily::ClaudeThinking);
    assert_eq!(detect_model_family("claude-opus-5", proxy_url), ModelFamily::ClaudeThinking);

    // Claude Opus 4.x：上游默认开启 Extended Thinking，必须按思考家族清洗采样参数
    assert_eq!(detect_model_family("claude-opus-4.6", proxy_url), ModelFamily::ClaudeThinking);
    assert_eq!(detect_model_family("claude-opus-4-6-20260201", proxy_url), ModelFamily::ClaudeThinking);

    // DeepSeek R3
    assert_eq!(detect_model_family("deepseek-r3-preview", proxy_url), ModelFamily::DeepSeekReasoning);

    // Gemini 3.7
    assert_eq!(detect_model_family("gemini-3.7-flash", proxy_url), ModelFamily::GoogleGemini);
}

#[test]
fn test_universal_dynamic_unknown_parameter_self_healing() {
    let mut body = json!({
        "model": "future-model-2027",
        "messages": [{"role": "user", "content": "hi"}],
        "stream_options": {"include_usage": true},
        "custom_telemetry_flag": true,
        "seed": 42
    });

    // 模拟任意新厂商对 stream_options 报错
    let err1 = "API 错误 400 Bad Request: unknown parameter: 'stream_options' is not allowed";
    assert!(self_heal_bad_request_payload(&mut body, err1));
    assert!(body.get("stream_options").is_none());

    // 模拟未来新框架 Pydantic 对 custom_telemetry_flag 报错
    let err2 = "API 错误 400 Bad Request: extra inputs are not permitted for field 'custom_telemetry_flag'";
    assert!(self_heal_bad_request_payload(&mut body, err2));
    assert!(body.get("custom_telemetry_flag").is_none());

    // 核心字段 messages 与 model 绝不会被误删
    assert!(body.get("messages").is_some());
    assert!(body.get("model").is_some());
}
