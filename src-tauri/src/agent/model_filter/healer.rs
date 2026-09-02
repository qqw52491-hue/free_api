use serde_json::{json, Value};

/// 针对 400 Bad Request 响应内容的参数自愈修正器
/// 支持具体敏感字段、模式匹配与终极通用动态未知参数提取
pub fn self_heal_bad_request_payload(body: &mut Value, error_text: &str) -> bool {
    let lower = error_text.to_lowercase();
    let mut healed = false;
    let Some(obj) = body.as_object_mut() else {
        return false;
    };

    // 1. temperature 非法自愈
    if lower.contains("temperature")
        && (lower.contains("invalid") || lower.contains("not allowed") || lower.contains("only 1"))
    {
        if obj.remove("temperature").is_some() {
            println!("🩹 [自愈修正] 检测到服务端拒绝 temperature 参数，已自动剔除该字段准备重试");
            healed = true;
        }
    }

    // 2. top_p 非法自愈
    if (lower.contains("topp") || lower.contains("top_p"))
        && (lower.contains("invalid") || lower.contains("not allowed") || lower.contains("only 0.95"))
    {
        if obj.remove("top_p").is_some() {
            println!("🩹 [自愈修正] 检测到服务端拒绝 top_p 参数，已自动剔除该字段准备重试");
            healed = true;
        }
    }

    // 3. penalty 非法自愈
    if lower.contains("penalty") && (lower.contains("invalid") || lower.contains("not allowed")) {
        let r1 = obj.remove("presence_penalty").is_some();
        let r2 = obj.remove("frequency_penalty").is_some();
        if r1 || r2 {
            println!("🩹 [自愈修正] 检测到服务端拒绝 penalty 参数，已自动剔除该字段准备重试");
            healed = true;
        }
    }

    // 4. max_tokens -> max_completion_tokens 自愈
    if lower.contains("max_tokens") && lower.contains("max_completion_tokens") {
        if let Some(tokens) = obj.remove("max_tokens") {
            obj.insert("max_completion_tokens".to_string(), tokens);
            println!("🩹 [自愈修正] 检测到服务端要求使用 max_completion_tokens，已自动转换准备重试");
            healed = true;
        }
    }

    // 5. logprobs 非法自愈
    if lower.contains("logprob") && (lower.contains("invalid") || lower.contains("not supported") || lower.contains("not allowed")) {
        let r1 = obj.remove("logprobs").is_some();
        let r2 = obj.remove("top_logprobs").is_some();
        if r1 || r2 {
            println!("🩹 [自愈修正] 检测到服务端拒绝 logprobs 参数，已自动剔除准备重试");
            healed = true;
        }
    }

    // 6. budget_tokens 超过 max_tokens 自愈
    if lower.contains("budget_tokens") && (lower.contains("max_tokens") || lower.contains("greater than")) {
        if let Some(thinking) = obj.get("thinking") {
            if let Some(budget) = thinking.get("budget_tokens").and_then(|v| v.as_i64()) {
                obj.insert("max_tokens".to_string(), json!(budget + 4096));
                println!("🩹 [自愈修正] 检测到 max_tokens 必须大于 budget_tokens，已自动调大准备重试");
                healed = true;
            }
        }
    }

    // 7. reasoning_content in input messages 自愈
    if lower.contains("reasoning_content") && (lower.contains("message") || lower.contains("not allowed") || lower.contains("invalid")) {
        if let Some(messages) = obj.get_mut("messages").and_then(|m| m.as_array_mut()) {
            let mut removed = false;
            for msg in messages.iter_mut() {
                if let Some(msg_obj) = msg.as_object_mut() {
                    if msg_obj.remove("reasoning_content").is_some() {
                        removed = true;
                    }
                }
            }
            if removed {
                println!("🩹 [自愈修正] 检测到服务端拒绝历史消息中的 reasoning_content，已自动清除准备重试");
                healed = true;
            }
        }
    }

    // 8. reasoning_effort 不受支持档位自愈（如在不支持 xhigh/max 的老端点上报 400）
    if lower.contains("reasoning_effort")
        && (lower.contains("invalid")
            || lower.contains("allowed")
            || lower.contains("not supported")
            || lower.contains("value")
            || lower.contains("only"))
    {
        if let Some(effort) = obj.get_mut("reasoning_effort") {
            let current = effort.as_str().unwrap_or_default().to_string();
            if current == "xhigh" || current == "max" {
                *effort = json!("high");
                println!("🩹 [自愈修正] 检测到服务端不支持超高档位 reasoning_effort，已自动降级为 high 并准备重试");
                healed = true;
            } else if current == "none" || current == "minimal" {
                *effort = json!("low");
                println!("🩹 [自愈修正] 检测到服务端不支持 none/minimal reasoning_effort，已自动调整为 low 并准备重试");
                healed = true;
            }
        }
    }

    // 9. 终极通用动态未知/被拒参数自愈（面向未来任意新模型，零代码修改免维护）
    // 动态扫描当前 payload 中的所有 key。只要服务端在 400 报错中明确提到了该字段非法、
    // 未知、不支持或额外输入（无论未来出现什么全新的参数名如 stream_options/seed/thinking_config 等），
    // 系统自动就地剔除该字段并原地发起重试！
    let current_keys: Vec<String> = obj.keys().cloned().collect();
    for key in current_keys {
        let key_lower = key.to_lowercase();
        // 保护核心字段不被误删
        if key_lower == "messages" || key_lower == "model" {
            continue;
        }
        if lower.contains(&key_lower)
            && (lower.contains("invalid")
                || lower.contains("not allowed")
                || lower.contains("unsupported")
                || lower.contains("unknown")
                || lower.contains("unexpected")
                || lower.contains("extra inputs")
                || lower.contains("not permitted")
                || lower.contains("disallowed"))
        {
            if obj.remove(&key).is_some() {
                println!("🩹 [动态通用自愈] 识别到服务端拒绝未知/冲突字段 [{}]，已自动剔除并重发", key);
                healed = true;
            }
        }
    }

    healed
}
