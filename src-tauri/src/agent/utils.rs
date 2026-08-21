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

/// 智能采样历史执行日志 (Head + Tail 采样 + 关键错误行去重聚合保留)
/// 严格采用三段式预算分账：保证头部前置环境、中间关键去重报错、尾部致命诊断 100% 不超预算
pub fn sample_execution_log(raw: &str, head_chars: usize, tail_chars: usize) -> String {
    let char_count = raw.chars().count();
    let safe_threshold = head_chars + tail_chars + 60;
    if char_count <= safe_threshold {
        return raw.to_string();
    }

    let head: String = raw.chars().take(head_chars).collect();
    let tail: String = raw
        .chars()
        .skip(char_count.saturating_sub(tail_chars))
        .collect();
    let middle_omitted = char_count.saturating_sub(head_chars + tail_chars);

    // 零堆内存中间分配：通过行迭代器直接在原字符串切片上扫描中间被省略区间的关键错误行 (去重 + 计数 + 长度截断)
    let mut error_counts: Vec<(String, usize)> = Vec::new();
    let mut total_error_chars = 0usize;
    const MAX_ERROR_BUDGET_CHARS: usize = 800;
    const MAX_UNIQUE_ERRORS: usize = 15;

    let tail_start_char = char_count.saturating_sub(tail_chars);
    let mut current_char_idx = 0;

    for line in raw.lines() {
        let line_len = line.chars().count();
        let line_start = current_char_idx;
        let line_end = current_char_idx + line_len;
        current_char_idx = line_end + 1; // 计入换行符

        // 跳过完全落在头部已展示或尾部已展示区间的行
        if line_end <= head_chars || line_start >= tail_start_char {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_lowercase();
        let is_critical = lower.contains("error")
            || lower.contains("panic")
            || lower.contains("fatal")
            || lower.contains("exception")
            || lower.contains("traceback")
            || lower.contains("caused by")
            || lower.contains("failed")
            || lower.contains("failure")
            || lower.contains("assert")
            || lower.contains("segfault")
            || lower.contains("500 internal")
            || lower.contains("403 forbidden")
            || lower.contains("拒绝访问")
            || lower.contains("权限不足")
            || lower.contains("未找到")
            || lower.contains("命令不存在")
            || lower.contains("编译失败")
            || lower.contains("异常")
            || lower.contains("错误");

        if is_critical {
            // 单行截断至 140 字符，防止长 JSON 或巨型单行撑爆
            let capped_line: String = trimmed.chars().take(140).collect();
            if let Some(existing) = error_counts.iter_mut().find(|(k, _)| k == &capped_line) {
                existing.1 += 1;
            } else if error_counts.len() < MAX_UNIQUE_ERRORS
                && total_error_chars + capped_line.len() < MAX_ERROR_BUDGET_CHARS
            {
                total_error_chars += capped_line.len();
                error_counts.push((capped_line, 1));
            }
        }
    }

    let middle_note = if error_counts.is_empty() {
        format!(
            "\n\n[... 已折叠中间 {} 字符，首尾关键信息已保留 ...]\n\n",
            middle_omitted
        )
    } else {
        let formatted_errors: Vec<String> = error_counts
            .into_iter()
            .map(|(line, count)| {
                if count > 1 {
                    format!("  ⚠️ [捕获错误 ×{}] {}", count, line)
                } else {
                    format!("  ⚠️ [捕获错误] {}", line)
                }
            })
            .collect();

        format!(
            "\n\n[... 已折叠中间 {} 字符，提取关键异常行如下 ...]\n{}\n[... 尾部最新诊断输出如下 ...]\n\n",
            middle_omitted,
            formatted_errors.join("\n")
        )
    };

    format!("{}{}{}", head, middle_note, tail)
}

// ==================== JSON 提取与智能容错修复 ====================

/// 清洗单行/多行注释、Python 特殊常量，规范化单引号与未转义控制字符等
pub fn sanitize_json_tokens(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let chars: Vec<char> = raw.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut in_double_str = false;
    let mut in_single_str = false;
    let mut escape = false;

    while i < len {
        let ch = chars[i];

        if escape {
            escape = false;
            out.push(ch);
            i += 1;
            continue;
        }

        if ch == '\\' && (in_double_str || in_single_str) {
            escape = true;
            out.push(ch);
            i += 1;
            continue;
        }

        // 双引号字符串边界
        if ch == '"' && !in_single_str {
            in_double_str = !in_double_str;
            out.push(ch);
            i += 1;
            continue;
        }

        // 单引号字符串转换成双引号 (在非双引号内)
        if ch == '\'' && !in_double_str {
            in_single_str = !in_single_str;
            out.push('"');
            i += 1;
            continue;
        }

        // 字符串内部的原始控制字符与换行符自动转义
        if in_double_str || in_single_str {
            if ch == '\n' {
                out.push_str("\\n");
            } else if ch == '\r' {
                out.push_str("\\r");
            } else if ch == '\t' {
                out.push_str("\\t");
            } else if (ch as u32) < 0x20 {
                // 忽略非标准低位控制字符
            } else {
                out.push(ch);
            }
            i += 1;
            continue;
        }

        // 非字符串区域：剥离 // 单行注释
        if ch == '/' && i + 1 < len && chars[i + 1] == '/' {
            i += 2;
            while i < len && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        // 非字符串区域：剥离 /* 多行注释 */
        if ch == '/' && i + 1 < len && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < len && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(len);
            continue;
        }

        // 非字符串区域：替换 Python 式布尔与 None
        if ch.is_alphabetic() {
            let start_word = i;
            while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start_word..i].iter().collect();
            match word.as_str() {
                "True" => out.push_str("true"),
                "False" => out.push_str("false"),
                "None" => out.push_str("null"),
                other => out.push_str(other),
            }
            continue;
        }

        out.push(ch);
        i += 1;
    }

    out
}

/// 消除对象或数组尾部多余的逗号 (Trailing Comma), 如 `{"a": 1,}` -> `{"a": 1}`
pub fn sanitize_trailing_commas(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut in_str = false;
    let mut escape = false;

    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if escape {
            escape = false;
            result.push(ch);
            i += 1;
            continue;
        }
        if ch == '\\' && in_str {
            escape = true;
            result.push(ch);
            i += 1;
            continue;
        }
        if ch == '"' {
            in_str = !in_str;
            result.push(ch);
            i += 1;
            continue;
        }
        if !in_str && ch == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                i += 1;
                continue;
            }
        }
        result.push(ch);
        i += 1;
    }
    result
}

/// 智能修复模型输出的不完整或截断的 JSON 结构 (如未闭合引号、括号、Trailing Comma 或 EOF 截断)
pub fn repair_json(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return Some(trimmed.to_string());
    }

    // 1. 深度清洗 Token (注释、换行转义、单引号、Python 关键字)
    let sanitized = sanitize_json_tokens(trimmed);
    let clean_trailing = sanitize_trailing_commas(&sanitized);
    if serde_json::from_str::<serde_json::Value>(&clean_trailing).is_ok() {
        return Some(clean_trailing);
    }

    // 2. 截取最外层 '{'
    let start = match clean_trailing.find('{') {
        Some(s) => s,
        None => return None,
    };
    let slice = &clean_trailing[start..];
    if serde_json::from_str::<serde_json::Value>(slice).is_ok() {
        return Some(slice.to_string());
    }

    // 3. 处理括号与字符串未闭合的截断情况
    let mut candidate = slice.to_string();
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
        if candidate.ends_with('\\') && !candidate.ends_with("\\\\") {
            candidate.pop();
        }
        candidate.push('"');
    }

    let mut closed = sanitize_trailing_commas(&candidate);
    for _ in 0..open_brackets {
        closed.push(']');
    }
    for _ in 0..open_braces {
        closed.push('}');
    }
    let final_candidate = sanitize_trailing_commas(&closed);

    if serde_json::from_str::<serde_json::Value>(&final_candidate).is_ok() {
        return Some(final_candidate);
    }

    // 4. 终极兜底：针对 finish 工具 summary 内容被截断时的救赎逻辑
    if final_candidate.contains(r#""finish""#) || final_candidate.contains(r#""summary""#) {
        if let Some(pos) = final_candidate.find(r#""summary""#) {
            let after = &final_candidate[pos + 9..];
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

/// 从大模型回复中稳健提取 JSON 纯文本（过滤思考标签与 Markdown 代码块，支持环绕说明文本）
pub fn extract_json_from_text(text: &str) -> Option<String> {
    // 1. 过滤可能混入正文的 <think>...</think> 思考标签或未闭合的前置 <think>
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

    // 3. 寻找最外层匹配的 '{' 与 '}'
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

    // 4. 尝试从第一个 '{' 到字符串结尾整体修复
    let from_first_brace: String = chars[start..].iter().collect();
    repair_json(&from_first_brace)
}

/// 智能规范化大模型生成的未整理 Value 对象
pub fn normalize_raw_instruction_value(val: &mut serde_json::Value) {
    if let serde_json::Value::Object(map) = val {
        // 1. 如果 command 是字符串形式的 JSON，尝试解析成 Object
        if let Some(serde_json::Value::String(cmd_str)) = map.get("command") {
            if let Ok(parsed_cmd) = serde_json::from_str::<serde_json::Value>(cmd_str) {
                map.insert("command".to_string(), parsed_cmd);
            }
        }

        // 2. 如果 command 缺失但 params 存在，提升 params 到 command
        if !map.contains_key("command") || map.get("command") == Some(&serde_json::Value::Null) {
            if let Some(params_val) = map.get("params") {
                if !params_val.is_null() {
                    map.insert("command".to_string(), params_val.clone());
                }
            }
        }

        // 3. 如果 todo_update 是字符串形式（如 "[]"），解析或转为空数组
        if let Some(serde_json::Value::String(todo_str)) = map.get("todo_update") {
            if let Ok(parsed_todo) = serde_json::from_str::<serde_json::Value>(todo_str) {
                map.insert("todo_update".to_string(), parsed_todo);
            } else {
                map.insert("todo_update".to_string(), serde_json::json!([]));
            }
        }

        // 4. 如果 applied_lessons 是单个字符串，转为数组
        if let Some(serde_json::Value::String(lesson_str)) = map.get("applied_lessons") {
            map.insert(
                "applied_lessons".to_string(),
                serde_json::json!([lesson_str]),
            );
        }
    }
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

/// 将上下文与 LLM 交互记录写入文件（包含完整 Prompt、思考过程、原始返回、Token 真实用量与报错原因，方便离线精准复盘）
fn log_llm_exchange_to_file(
    session_id: &str,
    step_id: usize,
    try_index: usize,
    model_name: &str,
    api_messages: &[serde_json::Value],
    thinking_text: &str,
    raw_response: &str,
    token_usage: Option<&TokenUsage>,
    parse_error: Option<&str>,
) {
    let log_dir = std::env::current_dir()
        .map(|p| p.join("agent_logs"))
        .unwrap_or_else(|_| std::path::PathBuf::from("agent_logs"));
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join(format!("{}.json", session_id));

    let mut logs_array = if let Ok(content) = std::fs::read_to_string(&log_path) {
        serde_json::from_str::<Vec<serde_json::Value>>(&content).unwrap_or_else(|_| vec![])
    } else {
        vec![]
    };

    let usage_val = token_usage.map(|u| {
        serde_json::json!({
            "prompt_tokens": u.prompt_tokens,
            "completion_tokens": u.completion_tokens,
            "total_tokens": u.total_tokens,
            "cached_tokens": u.cached_tokens,
            "cache_hit_rate": u.cache_hit_rate,
            "context_window": u.context_window,
            "usage_percent": u.usage_percent,
            "ttft_ms": u.ttft_ms,
            "total_duration_ms": u.total_duration_ms,
            "tokens_per_second": u.tokens_per_second,
            "summary": u.summary(),
        })
    });

    let log_data = serde_json::json!({
        "step_id": step_id,
        "try_index": try_index,
        "model": model_name,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "usage": usage_val,
        "parse_status": if parse_error.is_none() { "success" } else { "parse_error" },
        "parse_error": parse_error,
        "thinking_text": thinking_text,
        "raw_response": raw_response,
        "messages_sent_to_model": api_messages,
    });

    logs_array.push(log_data);

    if let Ok(s) = serde_json::to_string_pretty(&logs_array) {
        let _ = std::fs::write(&log_path, s);
        if let Some(err) = parse_error {
            println!(
                "📁 [模型异常已记录] 步骤 #{} (尝试 {}) 报错已归档到 {}: {}",
                step_id,
                try_index,
                log_path.display(),
                err
            );
        } else {
            println!(
                "📁 [步骤日志已记录] 步骤 #{} 交互已归档到 {}",
                step_id,
                log_path.display()
            );
        }
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
        let last_content_emit_at = std::time::Instant::now();
        const EMIT_THROTTLE_MS: u128 = 50; // 20 FPS 平滑节流，防止超长 Thinking (数万字) 造成 IPC 消息轰炸与前端 CPU 100% 锁死

        // 流式接收事件，实时向前端推送 Thinking/Content 以及动态速率与 TTFT
        let mut on_event = |event: StreamEvent| {
            if let Some(app_handle) = app {
                use tauri::Emitter;
                if first_token_at.is_none() {
                    first_token_at = Some(std::time::Instant::now());
                }
                let ttft_ms = first_token_at
                    .unwrap()
                    .duration_since(stream_start)
                    .as_millis() as u64;
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
                        if now.duration_since(last_thinking_emit_at).as_millis() >= EMIT_THROTTLE_MS
                        {
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
                    StreamEvent::Content {
                        chunk,
                        accumulated_content,
                        ..
                    } => {
                        if now.duration_since(last_content_emit_at).as_millis() >= EMIT_THROTTLE_MS
                        {
                            // 仅当大模型在输出 finish 交付指令或纯文本总结时，才提取 streaming_summary 进行终态流式呈现；
                            // 避免中间普通工具调用（如 task_store、save_file 的参数里恰好也有 summary 字段）被误识别为终态成果流式
                            let is_intermediate_tool = accumulated_content.contains("\"tool\"")
                                && !accumulated_content.contains("\"finish\"");
                            let streaming_summary = if is_intermediate_tool {
                                None
                            } else {
                                extract_streaming_json_field(
                                    accumulated_content,
                                    &["summary", "answer", "details"],
                                )
                            };

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
            let json_to_parse =
                repair_json(&final_json_str).unwrap_or_else(|| final_json_str.clone());
            let mut val: serde_json::Value = serde_json::from_str(&json_to_parse)
                .map_err(|e| format!("基础 JSON 语法错误: {} (处理前: {})", e, final_json_str))?;
            normalize_raw_instruction_value(&mut val);
            let inst: AgentInstruction =
                serde_json::from_value(val).map_err(|e| format!("数据结构转换失败: {}", e))?;
            Ok(inst)
        })();

        match parse_result {
            Ok(inst) => {
                if log_context {
                    log_llm_exchange_to_file(
                        session_id,
                        step_id,
                        current_try,
                        &config.model_name,
                        &api_values,
                        &result.thinking_text,
                        &final_json_str,
                        Some(&token_usage),
                        None,
                    );
                }
                return Ok((inst, token_usage, result.thinking_text));
            }
            Err(e) => {
                // 无论是否开启调试模式，模型格式崩溃时强制落盘归档，确保排查证据 100% 保留
                log_llm_exchange_to_file(
                    session_id,
                    step_id,
                    current_try,
                    &config.model_name,
                    &api_values,
                    &result.thinking_text,
                    &final_json_str,
                    Some(&token_usage),
                    Some(&e),
                );

                if let Some(app_handle) = app {
                    use tauri::Emitter;
                    let _ = app_handle.emit(
                        &format!("agent-progress-{}", session_id),
                        json!({
                            "type": "llm_parse_error",
                            "step_id": step_id,
                            "try_index": current_try,
                            "error": &e,
                            "raw_response": &final_json_str,
                            "thinking_text": &result.thinking_text,
                        }),
                    );
                    let _ = app_handle.emit(
                        &format!("agent-log-{}", session_id),
                        format!(
                            "⚠️ [模型格式异常·第{}次尝试] 报错: {}\n📄 模型原始输出:\n{}",
                            current_try, e, final_json_str
                        ),
                    );
                }

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
    fn test_repair_json_trailing_commas_and_comments() {
        let raw = r#"{
            // 这是一个测试动作
            "tool": "ask_human",
            "command": {
                "message": "hello",
            },
        }"#;
        let repaired = repair_json(raw).expect("should repair trailing commas and comments");
        let val: serde_json::Value =
            serde_json::from_str(&repaired).expect("must parse valid JSON");
        assert_eq!(val["tool"], "ask_human");
        assert_eq!(val["command"]["message"], "hello");
    }

    #[test]
    fn test_repair_json_unescaped_newlines_in_strings() {
        let raw = "{\n\"tool\": \"ask_human\",\n\"command\": {\n\"message\": \"Line 1\nLine 2 with \ttab\"\n}\n}";
        let repaired = repair_json(raw).expect("should repair unescaped newlines in strings");
        let val: serde_json::Value =
            serde_json::from_str(&repaired).expect("must parse valid JSON");
        assert_eq!(val["tool"], "ask_human");
        assert!(val["command"]["message"]
            .as_str()
            .unwrap()
            .contains("Line 1"));
    }

    #[test]
    fn test_repair_json_python_literals_and_single_quotes() {
        let raw = "{'tool': 'model_manager', 'command': {'confirm': True, 'null_field': None, 'flag': False}}";
        let repaired = repair_json(raw).expect("should repair single quotes and python literals");
        let val: serde_json::Value =
            serde_json::from_str(&repaired).expect("must parse valid JSON");
        assert_eq!(val["tool"], "model_manager");
        assert_eq!(val["command"]["confirm"], true);
        assert_eq!(val["command"]["flag"], false);
        assert!(val["command"]["null_field"].is_null());
    }

    #[test]
    fn test_repair_json_eof_truncation() {
        let raw =
            r#"{"tool": "ask_human", "command": {"message": "Incomplete text that was cut off"#;
        let repaired = repair_json(raw).expect("should repair truncated JSON");
        let val: serde_json::Value =
            serde_json::from_str(&repaired).expect("must parse valid JSON");
        assert_eq!(val["tool"], "ask_human");
        assert!(val["command"]["message"]
            .as_str()
            .unwrap()
            .contains("Incomplete text"));
    }

    #[test]
    fn test_extract_json_from_markdown_with_extra_text() {
        let raw = "Here is my response:\n```json\n{\n  \"tool\": \"web_search\",\n  \"command\": {\"query\": \"rust\"}\n}\n```\nHope that helps!";
        let extracted = extract_json_from_text(raw).expect("should extract json from markdown");
        let val: serde_json::Value =
            serde_json::from_str(&extracted).expect("must parse valid JSON");
        assert_eq!(val["tool"], "web_search");
        assert_eq!(val["command"]["query"], "rust");
    }

    #[test]
    fn test_normalize_raw_instruction_value() {
        let mut val = serde_json::json!({
            "tool": "web_search",
            "command": "{\"query\": \"rust agent\"}",
            "todo_update": "[{\"id\": \"1\", \"task\": \"Search web\", \"status\": \"done\"}]",
            "applied_lessons": "lesson_cache_1"
        });
        normalize_raw_instruction_value(&mut val);
        let inst: AgentInstruction =
            serde_json::from_value(val).expect("must deserialize AgentInstruction");
        assert_eq!(inst.tool, Some("web_search".to_string()));
        assert_eq!(inst.command.unwrap()["query"], "rust agent");
        assert_eq!(inst.todo_update.len(), 1);
        assert_eq!(inst.todo_update[0].id, 1);
        assert_eq!(inst.todo_update[0].description, "Search web");
        assert_eq!(
            inst.applied_lessons,
            Some(vec!["lesson_cache_1".to_string()])
        );
    }

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
    fn test_sample_execution_log_deduplication() {
        let mut text = String::from("Start log...\n");
        for _ in 0..100 {
            text.push_str("error: connection reset by peer\n");
        }
        text.push_str("End log with exit status 1\n");
        let sampled = sample_execution_log(&text, 20, 30);
        assert!(sampled.contains("×100") || sampled.contains("connection reset by peer"));
    }

    #[test]
    fn test_extract_streaming_json_field() {
        let partial_json = "{\"tool\": \"finish\", \"command\": {\"summary\": \"### 报告\\n- 第一行\\n- 带有\\\"引号\\\"的内容";
        let extracted = extract_streaming_json_field(partial_json, &["summary"]);
        assert_eq!(
            extracted,
            Some("### 报告\n- 第一行\n- 带有\"引号\"的内容".to_string())
        );

        let closed_json =
            "{\"tool\": \"finish\", \"command\": {\"summary\": \"完成了！\", \"other\": 123}}";
        let extracted_closed = extract_streaming_json_field(closed_json, &["summary"]);
        assert_eq!(extracted_closed, Some("完成了！".to_string()));
    }
}
