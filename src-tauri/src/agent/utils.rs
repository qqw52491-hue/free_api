use std::process::Command;

use crate::{
    agent::{
        context::{ChatMessage, SandwichContext},
        types::{AgentInstruction, TokenUsage},
    },
    db::DbState,
    llm::{self, ModelConfig, StreamEvent},
};
use serde_json::json;
use tauri::State;

// ==================== Shell 工具 ====================

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

// ==================== JSON 提取 ====================

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

// ==================== 调试与日志辅助函数 ====================

/// 打印发送给模型的消息概况（调试用）
fn print_debug_messages(api_messages: &[ChatMessage]) {
    println!("═══════════════════════════════════════════");
    println!(
        "📤 [DEBUG] 发送给模型的消息列表 (共 {} 条):",
        api_messages.len()
    );
    for (i, msg) in api_messages.iter().enumerate() {
        let content_str = match &msg.content {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let char_count = content_str.len();
        let preview: String = content_str.chars().take(120).collect();
        println!(
            "  [{}] role={}, chars={}, preview: {}...",
            i, msg.role, char_count, preview
        );
    }
    println!("═══════════════════════════════════════════");
}

/// 将上下文日志写入文件（用于离线分析模型决策）
fn log_context_to_file(
    session_id: &str,
    step_id: usize,
    model_name: &str,
    api_messages: &[ChatMessage],
) {
    let log_dir = std::path::PathBuf::from("../agent_logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join(format!("{}.json", session_id));

    let mut logs_array = if let Ok(content) = std::fs::read_to_string(&log_path) {
        serde_json::from_str::<Vec<serde_json::Value>>(&content).unwrap_or_else(|_| vec![])
    } else {
        vec![]
    };

    let log_data = serde_json::json!({
        "step_id": step_id,
        "model": model_name,
        "messages": api_messages.iter().map(|m| {
            serde_json::json!({
                "role": m.role,
                "content": m.content
            })
        }).collect::<Vec<_>>()
    });

    logs_array.push(log_data);

    if let Ok(s) = serde_json::to_string_pretty(&logs_array) {
        let _ = std::fs::write(&log_path, s);
        println!("📁 上下文已追加到: {}", log_path.display());
    }
}

// ==================== Agent LLM 调用 ====================

pub async fn call_llm(
    messages: &SandwichContext,
    state: &State<'_, DbState>,
    model_id: String,
    app: Option<&tauri::AppHandle>,
    step_id: usize,
    session_id: &str,
    log_context: bool,
) -> Result<(AgentInstruction, TokenUsage, String), String> {
    // 1. 从数据库解析模型配置
    let config = ModelConfig::from_db(state, &model_id)?;
    config.log_platform_info();

    // 2. 组装消息
    let api_messages = messages.assemble_messages();
    print_debug_messages(&api_messages);

    if log_context {
        log_context_to_file(session_id, step_id, &config.model_name, &api_messages);
    }

    // 3. 转为 serde_json::Value 供 llm 层使用
    let mut api_values: Vec<serde_json::Value> = api_messages
        .iter()
        .map(|m| json!({"role": m.role, "content": &m.content}))
        .collect();

    // 4. JSON 解析重试循环
    let mut current_try = 0;
    let max_retries = 3;

    loop {
        current_try += 1;

        // Thinking 实时推送到前端
        let mut on_event = |event: StreamEvent| {
            if let StreamEvent::Thinking { accumulated, .. } = event {
                if let Some(app_handle) = app {
                    use tauri::Emitter;
                    app_handle
                        .emit(
                            &format!("agent-progress-{}", session_id),
                            json!({
                                "type": "thinking",
                                "step_id": step_id,
                                "content": accumulated,
                                "done": false
                            }),
                        )
                        .ok();
                }
            }
        };

        // 5. 调用 llm 层发送请求（含 HTTP 重试）
        let result = llm::stream_with_retry(&config, &api_values, 3, &mut on_event).await?;

        // 6. Token 用量统计
        let token_usage = match result.usage {
            Some((prompt, completion)) => {
                TokenUsage::new(prompt, completion, config.context_window)
            }
            None => {
                let (est_p, est_c) = llm::estimate_usage(
                    &api_values,
                    result.full_response.len(),
                    config.context_window,
                );
                let usage = TokenUsage::new(est_p, est_c, config.context_window);
                println!("⚠️ API 未返回 usage，使用估算值: {}", usage.summary());
                usage
            }
        };

        println!("{}", token_usage.summary());
        if !result.thinking_text.is_empty() {
            println!(
                "🧠 Agent 思考过程 ({} 字符): {}...",
                result.thinking_text.len(),
                result.thinking_text.chars().take(300).collect::<String>()
            );
        }

        // 7. 尝试从响应中提取 JSON
        let mut final_json_str_opt = extract_json_from_text(&result.full_response);
        if final_json_str_opt.is_none() && result.full_response.trim().is_empty() {
            final_json_str_opt = extract_json_from_text(&result.thinking_text);
        }
        let final_json_str = final_json_str_opt.unwrap_or_else(|| {
            if result.full_response.trim().is_empty() && !result.thinking_text.trim().is_empty() {
                result.thinking_text.clone()
            } else {
                result.full_response.clone()
            }
        });

        // 8. 解析 AgentInstruction
        let parse_result = (|| -> Result<AgentInstruction, String> {
            let val: serde_json::Value = serde_json::from_str(&final_json_str)
                .map_err(|e| format!("基础 JSON 语法错误: {}", e))?;
            let inst: AgentInstruction =
                serde_json::from_value(val).map_err(|e| format!("数据结构转换失败: {}", e))?;
            Ok(inst)
        })();

        match parse_result {
            Ok(inst) => {
                return Ok((inst, token_usage, result.thinking_text));
            }
            Err(e) => {
                if current_try >= max_retries {
                    return Err(format!("{} \n原文: {}", e, final_json_str));
                }
                println!(
                    "⚠️ 大模型输出了错误的 JSON 格式，开始自动拦截并重试 (第 {} 次 / 共 {} 次): {}",
                    current_try, max_retries, e
                );

                // 将助手的错误输出加入上下文
                api_values.push(json!({"role": "assistant", "content": result.full_response}));

                // 将报错信息丢回给模型纠正
                api_values.push(json!({
                    "role": "user",
                    "content": format!(
                        "YOUR LAST OUTPUT TRIGGERED A PARSE ERROR: \n{}\n\n\
                         RULES:\n\
                         1. 遇到问题必须且只能在 <think> 标签内完成逻辑推导。\n\
                         2. <think> 结束后的最终输出，必须且只能是一个有效的 JSON 对象来调用对应的工具函数。\n\
                         3. 绝对禁止在 JSON 外输出任何解释性文本。违反此规则将导致系统崩溃。\n\n\
                         Fix the syntax error and generate again.",
                        e
                    )
                }));
            }
        }
    }
}
