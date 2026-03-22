use crate::db::{DbState, Platform, Model, ChatSession, ChatMessage};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{State, AppHandle, Emitter};
use uuid::Uuid;
use chrono::Utc;
use futures_util::StreamExt;

// ==================== PLATFORM COMMANDS ====================

#[tauri::command]
pub async fn get_platforms(state: State<'_, DbState>) -> Result<Vec<Platform>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare(
        "SELECT id, name, base_url, api_key, created_at FROM platforms ORDER BY created_at DESC"
    ).map_err(|e| e.to_string())?;
    let list = stmt.query_map([], |row| {
        Ok(Platform {
            id: row.get(0)?,
            name: row.get(1)?,
            base_url: row.get(2)?,
            api_key: row.get(3)?,
            created_at: row.get(4)?,
        })
    }).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    Ok(list)
}

#[tauri::command]
pub async fn add_platform(
    state: State<'_, DbState>,
    name: String,
    base_url: String,
    api_key: String,
) -> Result<Platform, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO platforms (id, name, base_url, api_key, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, name, base_url, api_key, now],
    ).map_err(|e| e.to_string())?;
    Ok(Platform { id, name, base_url, api_key, created_at: now })
}

#[tauri::command]
pub async fn update_platform(
    state: State<'_, DbState>,
    id: String,
    name: String,
    base_url: String,
    api_key: String,
) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE platforms SET name=?1, base_url=?2, api_key=?3 WHERE id=?4",
        params![name, base_url, api_key, id],
    ).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn delete_platform(state: State<'_, DbState>, id: String) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM platforms WHERE id=?1", params![id]).map_err(|e| e.to_string())?;
    Ok(())
}

// ==================== MODEL COMMANDS ====================

#[tauri::command]
pub async fn get_models(state: State<'_, DbState>, platform_id: Option<String>) -> Result<Vec<Model>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let (query, p): (String, Vec<String>) = if let Some(pid) = platform_id {
        ("SELECT id, platform_id, name, display_name, max_tokens, temperature, enabled, status, latency_ms FROM models WHERE platform_id = ?1 ORDER BY created_at DESC".into(), vec![pid])
    } else {
        ("SELECT id, platform_id, name, display_name, max_tokens, temperature, enabled, status, latency_ms FROM models ORDER BY created_at DESC".into(), vec![])
    };
    let mut stmt = conn.prepare(&query).map_err(|e| e.to_string())?;
    let list = stmt.query_map(rusqlite::params_from_iter(p), |row| {
        Ok(Model {
            id: row.get(0)?,
            platform_id: row.get(1)?,
            name: row.get(2)?,
            display_name: row.get(3)?,
            max_tokens: row.get(4)?,
            temperature: row.get(5)?,
            enabled: row.get::<_, i64>(6)? == 1,
            status: row.get(7)?,
            latency_ms: row.get(8)?,
        })
    }).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    Ok(list)
}

/// 获取所有模型 + 携带平台信息（用于对话页面的选择器）
#[tauri::command]
pub async fn get_all_models_with_platform(state: State<'_, DbState>) -> Result<Vec<serde_json::Value>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare(
        "SELECT m.id, m.name, m.display_name, m.platform_id, p.name as platform_name, m.enabled
         FROM models m JOIN platforms p ON p.id = m.platform_id
         WHERE m.enabled = 1
         ORDER BY p.name, m.name"
    ).map_err(|e| e.to_string())?;
    let list = stmt.query_map([], |row| {
        Ok(json!({
            "id": row.get::<_, String>(0)?,
            "name": row.get::<_, String>(1)?,
            "display_name": row.get::<_, String>(2)?,
            "platform_id": row.get::<_, String>(3)?,
            "platform_name": row.get::<_, String>(4)?,
            "enabled": row.get::<_, i64>(5)? == 1,
        }))
    }).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    Ok(list)
}

#[tauri::command]
pub async fn add_model(
    state: State<'_, DbState>,
    platform_id: String,
    name: String,
    display_name: String,
) -> Result<Model, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO models (id, platform_id, name, display_name, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, platform_id, name, display_name, now],
    ).map_err(|e| e.to_string())?;
    Ok(Model {
        id, platform_id, name, display_name,
        max_tokens: 4096, temperature: 0.7, enabled: true,
        status: "unknown".to_string(), latency_ms: 0,
    })
}

#[tauri::command]
pub async fn delete_model(state: State<'_, DbState>, id: String) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM models WHERE id=?1", params![id]).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn test_model(state: State<'_, DbState>, model_id: String) -> Result<serde_json::Value, String> {
    let (platform, model_name) = {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn.prepare(
            "SELECT p.base_url, p.api_key, m.name FROM models m JOIN platforms p ON p.id = m.platform_id WHERE m.id = ?1"
        ).map_err(|e| e.to_string())?;
        stmt.query_row(params![model_id], |row| {
            Ok((
                (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                row.get::<_, String>(2)?,
            ))
        }).map_err(|e| e.to_string())?
    };

    let start = std::time::Instant::now();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build().map_err(|e| e.to_string())?;

    let url = format!("{}/chat/completions", platform.0.trim_end_matches('/'));
    let body = json!({
        "model": model_name,
        "messages": [{"role": "user", "content": "Hi"}],
        "max_tokens": 5
    });

    let resp = client.post(&url)
        .header("Authorization", format!("Bearer {}", platform.1))
        .header("HTTP-Referer", "https://free-api-chat.app")
        .header("X-OpenRouter-Title", "Free API Chat")
        .json(&body).send().await;

    let latency = start.elapsed().as_millis() as i64;
    let (status, msg) = match resp {
        Ok(r) if r.status().is_success() => ("online", "连接正常".to_string()),
        Ok(r) => {
            let s = r.status();
            let body = r.text().await.unwrap_or_default();
            ("offline", format!("HTTP {} - {}", s, body.chars().take(200).collect::<String>()))
        }
        Err(e) => ("offline", e.to_string()),
    };

    // 更新数据库
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE models SET status=?1, latency_ms=?2 WHERE id=?3",
        params![status, latency, model_id],
    ).map_err(|e| e.to_string())?;

    Ok(json!({ "status": status, "latency_ms": latency, "message": msg }))
}

// ==================== SESSION COMMANDS ====================

#[tauri::command]
pub async fn get_sessions(state: State<'_, DbState>) -> Result<Vec<ChatSession>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare(
        "SELECT id, title, model_id, created_at, updated_at, message_count FROM chat_sessions ORDER BY updated_at DESC LIMIT 100"
    ).map_err(|e| e.to_string())?;
    let list = stmt.query_map([], |row| {
        Ok(ChatSession {
            id: row.get(0)?, title: row.get(1)?, model_id: row.get(2)?,
            created_at: row.get(3)?, updated_at: row.get(4)?, message_count: row.get(5)?,
        })
    }).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    Ok(list)
}

#[tauri::command]
pub async fn create_session(
    state: State<'_, DbState>,
    title: String,
    model_id: String,
) -> Result<ChatSession, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO chat_sessions (id, title, model_id, created_at, updated_at, message_count) VALUES (?1, ?2, ?3, ?4, ?5, 0)",
        params![id, title, model_id, now, now],
    ).map_err(|e| e.to_string())?;
    Ok(ChatSession { id, title, model_id, created_at: now.clone(), updated_at: now, message_count: 0 })
}

#[tauri::command]
pub async fn delete_session(state: State<'_, DbState>, id: String) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM chat_sessions WHERE id=?1", params![id]).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn rename_session(state: State<'_, DbState>, id: String, title: String) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute("UPDATE chat_sessions SET title=?1 WHERE id=?2", params![title, id]).map_err(|e| e.to_string())?;
    Ok(())
}

// ==================== MESSAGE COMMANDS ====================

#[tauri::command]
pub async fn get_messages(state: State<'_, DbState>, session_id: String) -> Result<Vec<ChatMessage>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare(
        "SELECT id, session_id, role, content, attachments, created_at, model_id FROM chat_messages WHERE session_id=?1 ORDER BY created_at ASC"
    ).map_err(|e| e.to_string())?;
    let list = stmt.query_map(params![session_id], |row| {
        Ok(ChatMessage {
            id: row.get(0)?, session_id: row.get(1)?, role: row.get(2)?,
            content: row.get(3)?, attachments: row.get(4)?, created_at: row.get(5)?,
            model_id: row.get(6)?,
        })
    }).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    Ok(list)
}

#[tauri::command]
pub async fn save_message(
    state: State<'_, DbState>,
    session_id: String,
    role: String,
    content: String,
    attachments: Option<String>,
    model_id: Option<String>,
) -> Result<ChatMessage, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO chat_messages (id, session_id, role, content, attachments, created_at, model_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, session_id, role, content, attachments, now, model_id],
    ).map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE chat_sessions SET updated_at=?1, message_count=message_count+1 WHERE id=?2",
        params![now, session_id],
    ).map_err(|e| e.to_string())?;
    Ok(ChatMessage { id, session_id, role, content, attachments, created_at: now, model_id })
}

// ==================== STREAM CHAT ====================

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AttachmentInfo {
    pub name: String,
    pub mime_type: String,
    pub data_base64: String,
}

#[tauri::command]
pub async fn send_chat(
    app: AppHandle,
    state: State<'_, DbState>,
    session_id: String,
    model_id: String,
    messages: Vec<serde_json::Value>,
    attachments: Option<Vec<AttachmentInfo>>,
) -> Result<String, String> {
    // 获取平台+模型信息
    let (base_url, api_key, model_name, max_tokens, temperature) = {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn.prepare(
            "SELECT p.base_url, p.api_key, m.name, m.max_tokens, m.temperature
             FROM models m JOIN platforms p ON p.id = m.platform_id
             WHERE m.id = ?1"
        ).map_err(|e| e.to_string())?;
        stmt.query_row(params![model_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, f64>(4)?,
            ))
        }).map_err(|e| format!("模型不存在: {}", e))?
    };

    // 构建消息（处理附件/图片）
    let mut api_messages = messages.clone();
    if let Some(ref files) = attachments {
        if !files.is_empty() {
            if let Some(last) = api_messages.last_mut() {
                if last.get("role").and_then(|r| r.as_str()) == Some("user") {
                    let text = last["content"].as_str().unwrap_or("").to_string();
                    let mut parts: Vec<serde_json::Value> = vec![json!({"type": "text", "text": text})];
                    for file in files {
                        if file.mime_type.starts_with("image/") {
                            parts.push(json!({
                                "type": "image_url",
                                "image_url": { "url": format!("data:{};base64,{}", file.mime_type, file.data_base64) }
                            }));
                        }
                    }
                    *last = json!({"role": "user", "content": parts});
                }
            }
        }
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build().map_err(|e| e.to_string())?;

    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let body = json!({
        "model": model_name,
        "messages": api_messages,
        "max_tokens": max_tokens,
        "temperature": temperature,
        "stream": true
    });

    let resp = client.post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("HTTP-Referer", "https://free-api-chat.app")
        .header("X-OpenRouter-Title", "Free API Chat")
        .json(&body).send().await
        .map_err(|e| format!("请求失败: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("API 错误 {}: {}", status, body));
    }

    let mut stream = resp.bytes_stream();
    let mut full_text = String::new();
    let mut buffer = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&chunk);
        buffer.push_str(&text);

        while let Some(pos) = buffer.find('\n') {
            let line = buffer[..pos].trim().to_string();
            buffer = buffer[pos + 1..].to_string();

            if line.starts_with("data: ") {
                let data = &line[6..];
                if data == "[DONE]" { break; }
                if let Ok(j) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(content) = j["choices"][0]["delta"]["content"].as_str() {
                        full_text.push_str(content);
                        app.emit("chat-stream", json!({
                            "session_id": session_id,
                            "content": content,
                            "done": false
                        })).ok();
                    }
                }
            }
        }
    }

    app.emit("chat-stream", json!({
        "session_id": session_id,
        "content": "",
        "done": true
    })).ok();

    Ok(full_text)
}
