use std::process::Command;

use crate::{
    agent::{
        context::SandwichContext,
        types::{AgentInstruction, TokenUsage},
    },
    db::DbState,
};
use futures::Stream;
use futures::StreamExt;
use rusqlite::params;
use serde_json::json;
use tauri::State;

pub fn run_shell(cmd: &str) -> (String, String, bool) {
    let result = Command::new("bash").arg("-c").arg(cmd).output();
    match result {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            (stdout, stderr, out.status.success())
        }
        Err(e) => (String::new(), e.to_string(), false),
    }
}

pub fn run_osascript(script: &str) -> (String, String, bool) {
    let result = Command::new("osascript").arg("-e").arg(script).output();
    match result {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            (stdout, stderr, out.status.success())
        }
        Err(e) => (String::new(), e.to_string(), false),
    }
}

pub fn chars_preview(s: &str, limit: usize) -> String {
    let s = s.replace("\n", " ");
    if s.chars().count() > limit {
        let truncated: String = s.chars().take(limit).collect();
        format!("{}...", truncated)
    } else {
        s.to_string()
    }
}

/// Robustly extract JSON object from AI reply.
pub fn extract_json_from_text(text: &str) -> Option<String> {
    let cleaned = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    if serde_json::from_str::<serde_json::Value>(cleaned).is_ok() {
        return Some(cleaned.to_string());
    }

    let chars: Vec<char> = cleaned.chars().collect();
    let start = chars.iter().position(|c| *c == '{')?;

    let mut depth = 0i32;
    let mut end = start;
    let mut in_string = false;
    let mut escape_next = false;

    for i in start..chars.len() {
        let ch = chars[i];
        if escape_next {
            escape_next = false;
            continue;
        }
        if ch == '\\' && in_string {
            escape_next = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if !in_string {
            if ch == '{' {
                depth += 1;
            }
            if ch == '}' {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
        }
    }

    if depth == 0 && end > start {
        let json_str: String = chars[start..=end].iter().collect();
        if serde_json::from_str::<serde_json::Value>(&json_str).is_ok() {
            return Some(json_str);
        }
    }

    None
}

/// 从 SSE 流中解析 usage 信息
fn parse_usage_from_json(json: &serde_json::Value) -> Option<(i64, i64)> {
    let usage = json.get("usage")?;
    let prompt = usage
        .get("prompt_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let completion = usage
        .get("completion_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if prompt > 0 || completion > 0 {
        Some((prompt, completion))
    } else {
        None
    }
}

pub async fn call_llm(
    messages: &SandwichContext,
    state: &State<'_, DbState>,
    model_id: String,
    app: Option<&tauri::AppHandle>,
    step_id: usize,
) -> Result<(AgentInstruction, TokenUsage, String), String> {
    // 获取具体数据库信息
    let (base_url, api_key, model_name, max_tokens, temperature) = {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT p.base_url, p.api_key, m.name, m.max_tokens, m.temperature
             FROM models m JOIN platforms p ON p.id = m.platform_id
             WHERE m.id = ?1",
            )
            .map_err(|e| e.to_string())?;
        stmt.query_row(params![model_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, f64>(4)?,
            ))
        })
        .map_err(|e| format!("模型不存在: {}", e))?
    };

    // 构建具体client
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())?;

    // 拼接路径
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

    // 拼接body
    let mut api_messages = messages.assemble_messages();
    let is_ollama = base_url.contains("localhost:11434") || base_url.contains("127.0.0.1:11434");

    // 确定上下文窗口大小
    let context_window: i64 = if is_ollama { 65536 } else { 128000 };

    if is_ollama {
        println!(
            "🔓 检测到 Ollama 本地模型，已自动注入 num_ctx: {}",
            context_window
        );
    }

    // 🔍 调试：打印发送给模型的消息概况
    println!("═══════════════════════════════════════════");
    println!("📤 [DEBUG] 发送给模型的消息列表 (共 {} 条):", api_messages.len());
    for (i, msg) in api_messages.iter().enumerate() {
        let content_str = match &msg.content {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let char_count = content_str.len();
        let preview: String = content_str.chars().take(120).collect();
        println!("  [{}] role={}, chars={}, preview: {}...", i, msg.role, char_count, preview);
    }
    println!("═══════════════════════════════════════════");

    let mut current_agent_try = 0;
    let max_agent_retries = 3;

    loop {
        current_agent_try += 1;

        let mut body = json!({
            "model": model_name,
            "messages": api_messages,
            "max_tokens": max_tokens,
            "temperature": if is_ollama { 0.0 } else { temperature },
            "top_p": if is_ollama { 0.9 } else { 1.0 },
            "stream": true,
            "stream_options": { "include_usage": true }
        });

        if is_ollama {
            body["options"] = json!({
                "num_ctx": context_window,
                "temperature": 0.0,
                "top_p": 0.9
            });
        }

        // --- 定义重试逻辑 (最多 3 次) ---
        let mut last_error = String::new();
        let mut final_resp: Option<reqwest::Response> = None;

        for i in 0..3 {
            let resp_result = client
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("HTTP-Referer", "https://free-api-chat.app")
                .header("X-OpenRouter-Title", "Free API Chat")
                .json(&body)
                .send()
                .await;

            match resp_result {
                Ok(resp) => {
                    if resp.status().is_success() {
                        final_resp = Some(resp);
                        break; // 成功，退出循环
                    } else {
                        let status = resp.status();
                        let error_body = resp.text().await.unwrap_or_default();
                        last_error = format!("API 错误 {}: {}", status, error_body);
                        println!("⚠️ LLM 请求失败 (第 {} 次尝试): {}", i + 1, last_error);
                    }
                }
                Err(e) => {
                    last_error = format!("网络请求失败: {}", e);
                    println!("⚠️ LLM 网络错误 (第 {} 次尝试): {}", i + 1, last_error);
                }
            }

            if i < 2 {
                tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
            }
        }

        let resp =
            final_resp.ok_or_else(|| format!("LLM 请求最终失败 (3次尝试): {}", last_error))?;
        let mut stream = resp.bytes_stream();
        let mut full_response = String::new();
        let mut thinking_text = String::new();
        let mut line_buf = String::new(); // 缓冲不完整的行
        let mut usage_data: Option<(i64, i64)> = None;

        while let Some(item) = stream.next().await {
            let chunk = item.map_err(|e| e.to_string())?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            line_buf.push_str(&chunk_str);

            // 只处理以 '\n' 结尾的完整行，剩余的留在 line_buf 里
            while let Some(pos) = line_buf.find('\n') {
                let line = line_buf[..pos].trim().to_string();
                line_buf = line_buf[pos + 1..].to_string();

                if line.is_empty() {
                    continue;
                }
                if line == "data: [DONE]" {
                    continue;
                }
                if let Some(data_str) = line.strip_prefix("data: ") {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(data_str) {
                        let delta = &json["choices"][0]["delta"];

                        // 🧠 解析 thinking/reasoning 内容（兼容多种字段名）
                        let reasoning = delta
                            .get("reasoning_content")
                            .and_then(|v| v.as_str())
                            .or_else(|| delta.get("reasoning").and_then(|v| v.as_str()))
                            .or_else(|| {
                                json.get("message")
                                    .and_then(|m| m.get("thinking"))
                                    .and_then(|v| v.as_str())
                            });
                        if let Some(think_chunk) = reasoning {
                            if !think_chunk.is_empty() {
                                thinking_text.push_str(think_chunk);
                                // 🚀 实时推送到前端，让页面立即显示！
                                if let Some(app_handle) = app {
                                    use tauri::Emitter;
                                    app_handle
                                        .emit(
                                            "agent-progress",
                                            json!({
                                                "type": "thinking",
                                                "step_id": step_id,
                                                "content": &thinking_text,
                                                "done": false
                                            }),
                                        )
                                        .ok();
                                }
                            }
                        }

                        // 📝 解析正常内容
                        if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                            if !content.is_empty() {
                                full_response.push_str(content);
                            }
                        }
                        // 解析 usage 信息（通常在最后一个 chunk 中）
                        if let Some(u) = parse_usage_from_json(&json) {
                            usage_data = Some(u);
                        }
                    }
                }
            }
        }

        // 处理最后可能残留在缓冲区中的数据
        let remaining = line_buf.trim();
        if !remaining.is_empty() && remaining != "data: [DONE]" {
            if let Some(data_str) = remaining.strip_prefix("data: ") {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(data_str) {
                    let delta = &json["choices"][0]["delta"];
                    let reasoning = delta
                        .get("reasoning_content")
                        .and_then(|v| v.as_str())
                        .or_else(|| delta.get("reasoning").and_then(|v| v.as_str()))
                        .or_else(|| {
                            json.get("message")
                                .and_then(|m| m.get("thinking"))
                                .and_then(|v| v.as_str())
                        });
                    if let Some(think_chunk) = reasoning {
                        if !think_chunk.is_empty() {
                            thinking_text.push_str(think_chunk);
                        }
                    }
                    if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                        if !content.is_empty() {
                            full_response.push_str(content);
                        }
                    }
                    if let Some(u) = parse_usage_from_json(&json) {
                        usage_data = Some(u);
                    }
                }
            }
        }

        // --- 构建 Token 用量统计 ---
        let token_usage = if let Some((prompt, completion)) = usage_data {
            TokenUsage::new(prompt, completion, context_window)
        } else {
            // 如果 API 没返回 usage，做一个粗略估算 (1 token ≈ 3 chars for 中文)
            let est_prompt = (api_messages
                .iter()
                .map(|m| m.content.to_string().len())
                .sum::<usize>()
                / 3) as i64;
            let est_completion = (full_response.len() / 3) as i64;
            let mut usage = TokenUsage::new(est_prompt, est_completion, context_window);
            println!("⚠️ API 未返回 usage，使用估算值: {}", usage.summary());
            usage
        };

        println!("{}", token_usage.summary());
        if !thinking_text.is_empty() {
            println!(
                "🧠 Agent 思考过程 ({} 字符): {}...",
                thinking_text.len(),
                thinking_text.chars().take(300).collect::<String>()
            );
        }

        // --- 解析 LLM 完整响应 ---
        let final_json_str =
            extract_json_from_text(&full_response).unwrap_or_else(|| full_response.clone());

        let parse_result = (|| -> Result<AgentInstruction, String> {
            let val: serde_json::Value = serde_json::from_str(&final_json_str)
                .map_err(|e| format!("基础 JSON 语法错误: {}", e))?;
            let inst: AgentInstruction =
                serde_json::from_value(val).map_err(|e| format!("数据结构转换失败: {}", e))?;
            Ok(inst)
        })();

        match parse_result {
            Ok(inst) => {
                return Ok((inst, token_usage, thinking_text));
            }
            Err(e) => {
                if current_agent_try >= max_agent_retries {
                    return Err(format!("{} \n原文: {}", e, final_json_str));
                }
                println!(
                    "⚠️ 大模型输出了错误的 JSON 格式，开始自动拦截并重试 (第 {} 次 / 共 {} 次): {}",
                    current_agent_try, max_agent_retries, e
                );

                // 将助手的错误输出加入上下文
                api_messages.push(crate::agent::context::ChatMessage {
                    role: "assistant".to_string(),
                    content: json!(full_response),
                });

                // 将报错信息丢回给模型纠正
                api_messages.push(crate::agent::context::ChatMessage {
                role: "user".to_string(),
                content: json!(format!("YOUR LAST OUTPUT TRIGGERED A PARSE ERROR: \n{}\n\nRULES:\n1. 遇到问题必须且只能在 <think> 标签内完成逻辑推导。\n2. <think> 结束后的最终输出，必须且只能是一个有效的 JSON 对象来调用对应的工具函数。\n3. 绝对禁止在 JSON 外输出任何解释性文本。违反此规则将导致系统崩溃。\n\nFix the syntax error and generate again.", e)),
            });
            }
        }
    } // end of loop
}
