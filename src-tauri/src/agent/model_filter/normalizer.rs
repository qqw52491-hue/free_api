use serde_json::{json, Value};
use super::family::ModelFamily;

/// 对请求体根据模型家族特征执行过滤与规范化清洗
pub fn sanitize_and_normalize_body(
    body: &mut Value,
    family: ModelFamily,
    reasoning_effort: &str,
    effective_max_tokens: i64,
) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };

    let effort_pref = if reasoning_effort.trim().is_empty() {
        "auto"
    } else {
        reasoning_effort.trim()
    };

    match family {
        ModelFamily::KimiReasoning => {
            // 🚨 Kimi 严格要求：
            // - temperature 严禁非 1（直接剔除，由服务端默认 1）
            // - top_p 严禁非 0.95（直接剔除，由服务端默认 0.95）
            // - penalty 严禁非 0
            obj.remove("temperature");
            obj.remove("top_p");
            obj.remove("presence_penalty");
            obj.remove("frequency_penalty");

            // 映射 reasoning_effort: low / medium / high / max
            let mapped_effort = match effort_pref {
                "low" | "extra-low" => "low",
                "medium" => "medium",
                "high" => "high",
                "max" | "auto" | _ => "max",
            };
            obj.insert("reasoning_effort".to_string(), json!(mapped_effort));

            // Agent 规范输出：支持 JSON 模式
            if !obj.contains_key("response_format") {
                obj.insert("response_format".to_string(), json!({ "type": "json_object" }));
            }
        }

        ModelFamily::OpenAiReasoning => {
            // 🚨 OpenAI o系列 / GPT-5系列 (5.1/5.6 Sol/Terra) 严格要求：
            // - 严禁自定义采样参数 temperature / top_p / penalty
            // - 强制使用 max_completion_tokens 代替 max_tokens
            // - 严禁 logprobs / top_logprobs
            obj.remove("temperature");
            obj.remove("top_p");
            obj.remove("presence_penalty");
            obj.remove("frequency_penalty");
            obj.remove("logprobs");
            obj.remove("top_logprobs");

            let token_val = obj
                .remove("max_tokens")
                .unwrap_or_else(|| json!(effective_max_tokens));
            obj.insert("max_completion_tokens".to_string(), token_val);

            // 映射 reasoning_effort: 支持 none / minimal / low / medium / high / xhigh / max (覆盖 GPT-5.1/5.6 与 o系列)
            let mapped_effort = match effort_pref {
                "off" | "none" => "none",
                "minimal" | "extra-low" => "minimal",
                "low" => "low",
                "medium" => "medium",
                "xhigh" => "xhigh",
                "max" => "max",
                "high" | "auto" | _ => "high",
            };
            obj.insert("reasoning_effort".to_string(), json!(mapped_effort));

            if !obj.contains_key("response_format") {
                obj.insert("response_format".to_string(), json!({ "type": "json_object" }));
            }
        }

        ModelFamily::DeepSeekReasoning => {
            // 🚨 DeepSeek R系列 / QwQ 严格要求：
            // - 剔除采样参数 temperature / top_p / penalty
            // - 剔除 logprobs (触发 400 错误)
            obj.remove("temperature");
            obj.remove("top_p");
            obj.remove("presence_penalty");
            obj.remove("frequency_penalty");
            obj.remove("logprobs");
            obj.remove("top_logprobs");

            // 🚨 DeepSeek 规范：如果输入 messages 含有 reasoning_content，必须移除避免 400
            if let Some(messages) = obj.get_mut("messages").and_then(|m| m.as_array_mut()) {
                for msg in messages.iter_mut() {
                    if let Some(msg_obj) = msg.as_object_mut() {
                        msg_obj.remove("reasoning_content");
                    }
                }
            }

            if !obj.contains_key("response_format") {
                obj.insert("response_format".to_string(), json!({ "type": "json_object" }));
            }
        }

        ModelFamily::ClaudeThinking => {
            // 🚨 Claude 启用 Extended Thinking 严格要求：
            // - temperature 必须显式设为 1.0（不能为 0.7 或其他）
            // - 使用 budget_tokens 控制推理预算
            // - max_tokens 必须严格大于 budget_tokens（否则报 400）
            obj.insert("temperature".to_string(), json!(1.0));
            obj.remove("top_p");

            let budget = match effort_pref {
                "low" | "extra-low" => 2048,
                "medium" => 8192,
                "high" => 32768,
                "max" | "auto" | _ => 64000,
            };
            obj.insert(
                "thinking".to_string(),
                json!({
                    "type": "enabled",
                    "budget_tokens": budget
                }),
            );

            // 确保 max_tokens > budget_tokens
            let current_max = obj
                .get("max_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(effective_max_tokens);
            if current_max <= budget {
                obj.insert("max_tokens".to_string(), json!(budget + 4096));
            }
        }

        ModelFamily::GoogleGemini | ModelFamily::StandardChat => {
            // 标准通用对话模型与 Gemini 模型：保留既有参数，确保 JSON 模式
            if !obj.contains_key("response_format") {
                obj.insert("response_format".to_string(), json!({ "type": "json_object" }));
            }
        }
    }
}
