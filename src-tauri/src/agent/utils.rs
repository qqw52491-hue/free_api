use std::process::Command;

use crate::{
    agent::{context::SandwichContext, types::AgentInstruction},
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

pub async fn call_llm(
    messages: &SandwichContext,
    state: &State<'_, DbState>,
    model_id: String,
) -> Result<AgentInstruction, String> {
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
    let api_messages = messages.assemble_messages();
    let body = json!({
        "model": model_name,
        "messages": api_messages,
        "max_tokens": max_tokens,
        "temperature": temperature,
        "stream": true
    });
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

    let resp = final_resp.ok_or_else(|| format!("LLM 请求最终失败 (3次尝试): {}", last_error))?;
    let mut stream = resp.bytes_stream();
    let mut full_response = String::new();
    let mut line_buf = String::new(); // 缓冲不完整的行

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
                    if let Some(content) = json["choices"][0]["delta"]["content"].as_str() {
                        full_response.push_str(content);
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
                if let Some(content) = json["choices"][0]["delta"]["content"].as_str() {
                    full_response.push_str(content);
                }
            }
        }
    }

    // --- 解析 LLM 完整响应 ---
    let final_json_str = extract_json_from_text(&full_response).unwrap_or(full_response);

    // 先解析成 Value 以自动合并/忽略重复字段（Map 模式下重复 Key 会被覆盖）
    let val: serde_json::Value = serde_json::from_str(&final_json_str)
        .map_err(|e| format!("基础 JSON 语法错误: {}\n原文: {}", e, final_json_str))?;

    // 再从 Value 转为强类型结构体
    let inst: AgentInstruction = serde_json::from_value(val)
        .map_err(|e| format!("数据结构转换失败: {}\n原文: {}", e, final_json_str))?;

    Ok(inst)
}
