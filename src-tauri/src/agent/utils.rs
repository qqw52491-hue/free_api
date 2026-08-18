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
use tokio::sync::watch;

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

/// 智能采样历史执行日志 (Head + Tail 采样 + 关键错误行保留)
/// 保证命令行与代码执行输出中的关键头部环境与尾部致命报错 (panic/error/exit code) 完整保留
pub fn sample_execution_log(raw: &str, head_chars: usize, tail_chars: usize) -> String {
    let char_count = raw.chars().count();
    let safe_threshold = head_chars + tail_chars + 60;
    if char_count <= safe_threshold {
        return raw.to_string();
    }

    let head: String = raw.chars().take(head_chars).collect();
    let tail: String = raw.chars().skip(char_count.saturating_sub(tail_chars)).collect();
    let middle_omitted = char_count.saturating_sub(head_chars + tail_chars);

    // 从中间被裁剪的文本中提取可能存在的关键错误行 (如 error:, panic:, fatal:, failed:)
    let mut critical_lines = Vec::new();
    let middle_text: String = raw.chars().skip(head_chars).take(middle_omitted).collect();
    for line in middle_text.lines() {
        let lower = line.to_lowercase();
        if lower.contains("error:") || lower.contains("panic:") || lower.contains("fatal:") || lower.contains("exception:") {
            let clean = line.trim();
            if !clean.is_empty() && clean.len() < 200 && critical_lines.len() < 3 {
                critical_lines.push(format!("  ⚠️ [捕获关键行] {}", clean));
            }
        }
    }

    let middle_note = if critical_lines.is_empty() {
        format!("\n\n[... 已折叠中间 {} 字符，核心首尾已完整保留 ...]\n\n", middle_omitted)
    } else {
        format!(
            "\n\n[... 已折叠中间 {} 字符 ...]\n{}\n[... 尾部诊断输出如下 ...]\n\n",
            middle_omitted,
            critical_lines.join("\n")
        )
    };

    format!("{}{}{}", head, middle_note, tail)
}

// ==================== JSON 提取与智能容错修复 ====================

/// 智能修复模型输出的不完整或截断的 JSON 结构 (如未闭合引号、括号或 EOF 截断)
pub fn repair_json(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return Some(trimmed.to_string());
    }

    let start = trimmed.find('{')?;
    let slice = &trimmed[start..];

    let mut candidate = slice.to_string();
    if serde_json::from_str::<serde_json::Value>(&candidate).is_ok() {
        return Some(candidate);
    }

    let mut in_str = false;
    let mut escape = false;
    let mut open_braces = 0i32;
    let mut open_brackets = 0i32;

    for ch in candidate.chars() {
        if escape {
            escape = false;
            continue;
        }
        if ch == '\\' && in_str {
            escape = true;
            continue;
        }
        if ch == '"' {
            in_str = !in_str;
            continue;
        }
        if !in_str {
            if ch == '{' {
                open_braces += 1;
            } else if ch == '}' {
                open_braces = (open_braces - 1).max(0);
            } else if ch == '[' {
                open_brackets += 1;
            } else if ch == ']' {
                open_brackets = (open_brackets - 1).max(0);
            }
        }
    }

    if in_str {
        candidate.push('"');
    }
    for _ in 0..open_brackets {
        candidate.push(']');
    }
    for _ in 0..open_braces {
        candidate.push('}');
    }

    if serde_json::from_str::<serde_json::Value>(&candidate).is_ok() {
        return Some(candidate);
    }

    // 终极兜底：如果是 finish 工具的超长 HTML / summary 产生截断或控制字符转义问题，直接救赎提取
    if candidate.contains(r#""finish""#) || candidate.contains(r#""summary""#) {
        if let Some(pos) = candidate.find(r#""summary""#) {
            let after = &candidate[pos + 9..];
            if let Some(colon_pos) = after.find(':') {
                let val_part = after[colon_pos + 1..].trim_start();
                if val_part.starts_with('"') {
                    let summary_content = &val_part[1..];
                    let clean_summary = summary_content
                        .trim_end_matches('}')
                        .trim_end_matches(']')
                        .trim_end_matches('"')
                        .trim();
                    let rescued = serde_json::json!({
                        "tool": "finish",
                        "command": {
                            "summary": clean_summary
                        }
                    });
                    return Some(rescued.to_string());
                }
            }
        }
    }

    None
}

/// Robustly extract JSON object from AI reply with automatic repair.
pub fn extract_json_from_text(text: &str) -> Option<String> {
    // 1. 过滤可能混入正文的 <think>...</think> 思考标签
    let without_think = if let Some(start_think) = text.find("<think>") {
        if let Some(end_think) = text.find("</think>") {
            format!("{}{}", &text[..start_think], &text[end_think + 8..])
        } else {
            text.to_string()
        }
    } else {
        text.to_string()
    };

    let cleaned = without_think.trim();

    // 2. 优先提取 ```json ... ``` 或 ``` ... ``` 代码块中的内容
    if let Some(start_code) = cleaned.find("```json") {
        let after_code = &cleaned[start_code + 7..];
        if let Some(end_code) = after_code.find("```") {
            let inner = after_code[..end_code].trim();
            if let Some(repaired) = repair_json(inner) {
                return Some(repaired);
            }
        }
    } else if let Some(start_code) = cleaned.find("```") {
        let after_code = &cleaned[start_code + 3..];
        if let Some(end_code) = after_code.find("```") {
            let inner = after_code[..end_code].trim();
            if let Some(repaired) = repair_json(inner) {
                return Some(repaired);
            }
        }
    }

    if let Some(repaired) = repair_json(cleaned) {
        return Some(repaired);
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

    if depth == 0 && end >= start {
        let json_str: String = chars[start..=end].iter().collect();
        if let Some(repaired) = repair_json(&json_str) {
            return Some(repaired);
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

// ==================== 流式 JSON 增量字段提取器 ====================

/// 从未闭合或已闭合的流式 JSON 字符串中，实时提取指定字段的文本内容（自动解码转义字符）
pub fn extract_streaming_json_field(json_text: &str, field_names: &[&str]) -> Option<String> {
    for field in field_names {
        let pattern = format!("\"{}\"", field);
        if let Some(field_idx) = json_text.find(&pattern) {
            let after_field = &json_text[field_idx + pattern.len()..];
            let mut colon_found = false;
            let mut start_quote_offset = None;

            for (idx, ch) in after_field.char_indices() {
                if !colon_found {
                    if ch == ':' {
                        colon_found = true;
                    }
                } else {
                    if ch == '"' {
                        start_quote_offset = Some(field_idx + pattern.len() + idx + 1);
                        break;
                    } else if !ch.is_whitespace() {
                        break;
                    }
                }
            }

            if let Some(start_pos) = start_quote_offset {
                if start_pos >= json_text.len() {
                    return Some(String::new());
                }
                let content_slice = &json_text[start_pos..];
                let mut decoded = String::with_capacity(content_slice.len());
                let mut is_escaped = false;
                let mut chars = content_slice.chars().peekable();

                while let Some(c) = chars.next() {
                    if is_escaped {
                        match c {
                            'n' => decoded.push('\n'),
                            'r' => decoded.push('\r'),
                            't' => decoded.push('\t'),
                            '\\' => decoded.push('\\'),
                            '"' => decoded.push('"'),
                            '/' => decoded.push('/'),
                            'u' => {
                                let mut hex = String::new();
                                for _ in 0..4 {
                                    if let Some(&hc) = chars.peek() {
                                        if hc.is_ascii_hexdigit() {
                                            hex.push(hc);
                                            chars.next();
                                        } else {
                                            break;
                                        }
                                    }
                                }
                                if hex.len() == 4 {
                                    if let Ok(code) = u32::from_str_radix(&hex, 16) {
                                        if let Some(unicode_char) = char::from_u32(code) {
                                            decoded.push(unicode_char);
                                        }
                                    }
                                }
                            }
                            other => decoded.push(other),
                        }
                        is_escaped = false;
                    } else if c == '\\' {
                        is_escaped = true;
                    } else if c == '"' {
                        return Some(decoded);
                    } else {
                        decoded.push(c);
                    }
                }

                return Some(decoded);
            }
        }
    }
    None
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
    reasoning_effort: &str,
    interrupt_rx: &mut watch::Receiver<u64>,
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

        let stream_start = std::time::Instant::now();
        let mut first_token_at: Option<std::time::Instant> = None;
        let mut total_chars: usize = 0;
        let mut last_thinking_emit_at = std::time::Instant::now();
        let mut last_content_emit_at = std::time::Instant::now();
        const EMIT_THROTTLE_MS: u128 = 50; // 20 FPS 平滑节流，防止超长 Thinking (数万字) 造成 IPC 消息轰炸与前端 CPU 100% 锁死

        // 流式接收事件，实时向前端推送 Thinking/Content 以及动态速率与 TTFT
        let mut on_event = |event: StreamEvent| {
            if let Some(app_handle) = app {
                use tauri::Emitter;
                if first_token_at.is_none() {
                    first_token_at = Some(std::time::Instant::now());
                }
                let ttft_ms = first_token_at.unwrap().duration_since(stream_start).as_millis() as u64;
                let chunk_len = match &event {
                    StreamEvent::Thinking { chunk, .. } => chunk.len(),
                    StreamEvent::Content { chunk, .. } => chunk.len(),
                };
                total_chars += chunk_len;
                let est_tokens = (total_chars as f64 / 3.5).ceil() as i64;
                let gen_secs = first_token_at.unwrap().elapsed().as_secs_f64();
                let live_speed = if gen_secs > 0.05 && est_tokens > 0 {
                    (est_tokens as f64) / gen_secs
                } else {
                    0.0
                };

                let now = std::time::Instant::now();
                match event {
                    StreamEvent::Thinking { accumulated, .. } => {
                        if now.duration_since(last_thinking_emit_at).as_millis() >= EMIT_THROTTLE_MS {
                            last_thinking_emit_at = now;
                            app_handle
                                .emit(
                                    &format!("agent-progress-{}", session_id),
                                    json!({
                                        "type": "thinking",
                                        "step_id": step_id,
                                        "content": accumulated,
                                        "done": false,
                                        "live_tokens": est_tokens,
                                        "live_speed": live_speed,
                                        "ttft_ms": ttft_ms
                                    }),
                                )
                                .ok();
                        }
                    }
                    StreamEvent::Content { chunk, accumulated_content, .. } => {
                        if now.duration_since(last_content_emit_at).as_millis() >= EMIT_THROTTLE_MS {
                            last_content_emit_at = now;
                            let streaming_summary = extract_streaming_json_field(accumulated_content, &["summary", "answer", "details"]);

                            app_handle
                                .emit(
                                    &format!("agent-progress-{}", session_id),
                                    json!({
                                        "type": "content_stream",
                                        "step_id": step_id,
                                        "chunk": chunk,
                                        "accumulated": accumulated_content,
                                        "streaming_summary": streaming_summary,
                                        "live_tokens": est_tokens,
                                        "live_speed": live_speed,
                                        "ttft_ms": ttft_ms
                                    }),
                                )
                                .ok();

                            if let Some(ref summary_text) = streaming_summary {
                                if !summary_text.is_empty() {
                                    app_handle
                                        .emit(
                                            &format!("agent-progress-{}", session_id),
                                            json!({
                                                "type": "summary_stream",
                                                "step_id": step_id,
                                                "content": summary_text,
                                                "live_tokens": est_tokens,
                                                "live_speed": live_speed,
                                                "ttft_ms": ttft_ms
                                            }),
                                        )
                                        .ok();
                                }
                            }
                        }
                    }
                }
            }
        };

        // 5. 调用 llm 层发送请求（含 HTTP 重试）
        let result = llm::stream_with_retry(
            &config,
            &api_values,
            reasoning_effort,
            interrupt_rx,
            3,
            &mut on_event,
        )
        .await?;

        // 6. Token 用量与速度指标统计
        let token_usage = match result.usage {
            Some(raw) => TokenUsage::new(
                raw.prompt_tokens,
                raw.completion_tokens,
                raw.cached_tokens,
                config.context_window,
                result.ttft_ms,
                result.total_duration_ms,
                result.tokens_per_second,
            ),
            None => {
                let (est_p, est_c) = llm::estimate_usage(
                    &api_values,
                    result.full_response.len(),
                    config.context_window,
                );
                let usage = TokenUsage::new(
                    est_p,
                    est_c,
                    0,
                    config.context_window,
                    result.ttft_ms,
                    result.total_duration_ms,
                    result.tokens_per_second,
                );
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

        // 8. 解析 AgentInstruction (带智能容错与修复)
        let parse_result = (|| -> Result<AgentInstruction, String> {
            let json_to_parse = repair_json(&final_json_str).unwrap_or_else(|| final_json_str.clone());
            let val: serde_json::Value = serde_json::from_str(&json_to_parse)
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
                         3. 如果在 JSON 字符串字段（如 summary/details）中包含 HTML 或双引号，必须严格转义为 `\\\"`，绝对不能输出未闭合或损坏的 JSON。\n\
                         4. 绝对禁止在 JSON 外输出任何解释性文本。\n\n\
                         Fix the syntax error and generate valid JSON again.",
                        e
                    )
                }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sample_execution_log_short() {
        let short = "Everything is ok! Exit code 0.";
        let sampled = sample_execution_log(short, 300, 500);
        assert_eq!(sampled, short);
    }

    #[test]
    fn test_sample_execution_log_long_with_errors() {
        let mut long_text = String::from("Compiling crate xyz v0.1.0...\n");
        for i in 0..50 {
            if i == 25 {
                long_text.push_str("error: failed to resolve type FooBar\n");
            } else if i == 30 {
                long_text.push_str("panic: index out of bounds at main.rs:42\n");
            } else {
                long_text.push_str(&format!("warning: unused variable var_{}\n", i));
            }
        }
        long_text.push_str("Finished with error code 101.\n");

        let sampled = sample_execution_log(&long_text, 50, 60);
        assert!(sampled.contains("Compiling crate"));
        assert!(sampled.contains("error: failed to resolve type FooBar"));
        assert!(sampled.contains("panic: index out of bounds"));
        assert!(sampled.contains("Finished with error code 101"));
        assert!(sampled.contains("折叠中间"));
    }

    #[test]
    fn test_extract_streaming_json_field() {
        let partial_json = "{\"tool\": \"finish\", \"command\": {\"summary\": \"### 报告\\n- 第一行\\n- 带有\\\"引号\\\"的内容";
        let extracted = extract_streaming_json_field(partial_json, &["summary"]);
        assert_eq!(
            extracted,
            Some("### 报告\n- 第一行\n- 带有\"引号\"的内容".to_string())
        );

        let closed_json = "{\"tool\": \"finish\", \"command\": {\"summary\": \"完成了！\", \"other\": 123}}";
        let extracted_closed = extract_streaming_json_field(closed_json, &["summary"]);
        assert_eq!(extracted_closed, Some("完成了！".to_string()));
    }
}
