use crate::db::{
    delete_model_spec_record, get_all_model_specs, upsert_model_spec_record, DbState, Model,
    ModelSpec, Platform,
};
use chrono::Utc;
use rusqlite::params;
use serde_json::{json, Value};
use tauri::State;
use uuid::Uuid;

// ==================== PLATFORM COMMANDS ====================

#[tauri::command]
pub async fn get_platforms(state: State<'_, DbState>) -> Result<Vec<Platform>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare(
        "SELECT id, name, base_url, api_key, created_at FROM platforms ORDER BY created_at DESC"
    ).map_err(|e| e.to_string())?;
    let list = stmt
        .query_map([], |row| {
            Ok(Platform {
                id: row.get(0)?,
                name: row.get(1)?,
                base_url: row.get(2)?,
                api_key: row.get(3)?,
                created_at: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
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
    Ok(Platform {
        id,
        name,
        base_url,
        api_key,
        created_at: now,
    })
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
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn delete_platform(state: State<'_, DbState>, id: String) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM platforms WHERE id=?1", params![id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ==================== MODEL COMMANDS ====================

fn normalize_provider_capabilities(raw: String) -> Result<String, String> {
    let normalized = if raw.trim().is_empty() {
        "{}".to_string()
    } else {
        raw
    };
    let value: Value = serde_json::from_str(&normalized)
        .map_err(|error| format!("Provider 能力配置必须是合法 JSON: {}", error))?;
    if !value.is_object() {
        return Err("Provider 能力配置必须是 JSON 对象".to_string());
    }
    Ok(value.to_string())
}

#[tauri::command]
pub async fn get_models(
    state: State<'_, DbState>,
    platform_id: Option<String>,
) -> Result<Vec<Model>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let (query, p): (String, Vec<String>) = if let Some(pid) = platform_id {
        ("SELECT id, platform_id, name, display_name, max_tokens, temperature, enabled, status, latency_ms, context_window, provider_capabilities FROM models WHERE platform_id = ?1 ORDER BY created_at DESC".into(), vec![pid])
    } else {
        ("SELECT id, platform_id, name, display_name, max_tokens, temperature, enabled, status, latency_ms, context_window, provider_capabilities FROM models ORDER BY created_at DESC".into(), vec![])
    };
    let mut stmt = conn.prepare(&query).map_err(|e| e.to_string())?;
    let list = stmt
        .query_map(rusqlite::params_from_iter(p), |row| {
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
                context_window: row.get(9)?,
                provider_capabilities: row.get(10)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(list)
}

/// 获取所有模型 + 携带平台信息（用于对话页面的选择器）
#[tauri::command]
pub async fn get_all_models_with_platform(
    state: State<'_, DbState>,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT m.id, m.name, m.display_name, m.platform_id, p.name as platform_name, m.enabled
         FROM models m JOIN platforms p ON p.id = m.platform_id
         WHERE m.enabled = 1
         ORDER BY p.name, m.name",
        )
        .map_err(|e| e.to_string())?;
    let list = stmt
        .query_map([], |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "display_name": row.get::<_, String>(2)?,
                "platform_id": row.get::<_, String>(3)?,
                "platform_name": row.get::<_, String>(4)?,
                "enabled": row.get::<_, i64>(5)? == 1,
            }))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
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
        id,
        platform_id,
        name,
        display_name,
        max_tokens: 4096,
        temperature: 0.7,
        enabled: true,
        status: "unknown".to_string(),
        latency_ms: 0,
        context_window: 0,
        provider_capabilities: "{}".to_string(),
    })
}

/// 更新模型参数（输出/窗口与 provider capability profile）。
#[tauri::command]
pub async fn update_model(
    state: State<'_, DbState>,
    id: String,
    max_tokens: Option<i64>,
    context_window: Option<i64>,
    display_name: Option<String>,
    provider_capabilities: Option<String>,
) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    if let Some(v) = max_tokens {
        conn.execute(
            "UPDATE models SET max_tokens = ?1 WHERE id = ?2",
            params![v, id],
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(v) = context_window {
        conn.execute(
            "UPDATE models SET context_window = ?1 WHERE id = ?2",
            params![v, id],
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(v) = display_name {
        conn.execute(
            "UPDATE models SET display_name = ?1 WHERE id = ?2",
            params![v, id],
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(v) = provider_capabilities {
        let normalized = normalize_provider_capabilities(v)?;
        conn.execute(
            "UPDATE models SET provider_capabilities = ?1 WHERE id = ?2",
            params![normalized, id],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn delete_model(state: State<'_, DbState>, id: String) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM models WHERE id=?1", params![id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ==================== MODEL SPECS COMMANDS ====================

#[tauri::command]
pub async fn get_model_specs(state: State<'_, DbState>) -> Result<Vec<ModelSpec>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    get_all_model_specs(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn upsert_model_spec(
    state: State<'_, DbState>,
    id: Option<String>,
    name: String,
    context_window: i64,
    max_tokens: i64,
    supports_thinking: bool,
    default_temperature: f64,
    description: Option<String>,
) -> Result<ModelSpec, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let final_id = id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let now = Utc::now().to_rfc3339();
    let spec = ModelSpec {
        id: final_id,
        name,
        context_window,
        max_tokens,
        supports_thinking,
        default_temperature,
        description: description.unwrap_or_default(),
        created_at: now,
    };
    upsert_model_spec_record(&conn, &spec).map_err(|e| e.to_string())?;
    Ok(spec)
}

#[tauri::command]
pub async fn delete_model_spec(
    state: State<'_, DbState>,
    id_or_name: String,
) -> Result<usize, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    delete_model_spec_record(&conn, &id_or_name).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn add_model_with_spec(
    state: State<'_, DbState>,
    platform_id: String,
    name: String,
    display_name: String,
    max_tokens: i64,
    temperature: f64,
    context_window: i64,
) -> Result<Model, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO models (id, platform_id, name, display_name, max_tokens, temperature, enabled, status, latency_ms, context_window, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 'unknown', 0, ?7, ?8)",
        params![id, platform_id, name, display_name, max_tokens, temperature, context_window, now],
    ).map_err(|e| e.to_string())?;
    Ok(Model {
        id,
        platform_id,
        name,
        display_name,
        max_tokens,
        temperature,
        enabled: true,
        status: "unknown".to_string(),
        latency_ms: 0,
        context_window,
        provider_capabilities: "{}".to_string(),
    })
}

#[tauri::command]
pub async fn test_model(
    state: State<'_, DbState>,
    model_id: String,
) -> Result<serde_json::Value, String> {
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
        })
        .map_err(|e| e.to_string())?
    };

    let clean_base = platform.0.trim().trim_end_matches('/');
    let clean_key = platform.1.trim();
    let auth_header = if clean_key.starts_with("Bearer ") {
        clean_key.to_string()
    } else {
        format!("Bearer {}", clean_key)
    };

    let key_chars: Vec<char> = clean_key.chars().collect();
    let masked_key = if key_chars.len() > 10 {
        let prefix: String = key_chars[..6].iter().collect();
        let suffix: String = key_chars[key_chars.len() - 4..].iter().collect();
        format!("{}...{}", prefix, suffix)
    } else {
        "***".to_string()
    };
    let masked_auth = format!("Bearer {}", masked_key);

    let start = std::time::Instant::now();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .connect_timeout(std::time::Duration::from_secs(10))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36")
        .build()
        .map_err(|e| e.to_string())?;

    let url = format!("{}/chat/completions", clean_base);
    let body = json!({
        "model": model_name,
        "messages": [{"role": "user", "content": "Hi"}],
        "max_tokens": 16
    });

    let resp = client
        .post(&url)
        .header("Authorization", &auth_header)
        .header("HTTP-Referer", "https://free-api-chat.app")
        .header("X-OpenRouter-Title", "Free API Chat")
        .json(&body)
        .send()
        .await;

    let latency = start.elapsed().as_millis() as i64;
    let (status, msg, http_code, resp_body, raw_err) = match resp {
        Ok(r) if r.status().is_success() => {
            let s = r.status().as_u16();
            let b = r.text().await.unwrap_or_default();
            ("online", "连接正常".to_string(), Some(s), b, None)
        }
        Ok(r) => {
            let s = r.status().as_u16();
            let b = r.text().await.unwrap_or_default();
            ("offline", format!("HTTP {} - {}", s, b), Some(s), b, None)
        }
        Err(e) => {
            let err_str = e.to_string();
            (
                "offline",
                err_str.clone(),
                None,
                String::new(),
                Some(err_str),
            )
        }
    };

    let diagnostics = json!({
        "endpoint_url": url,
        "method": "POST",
        "headers": {
            "Authorization": masked_auth,
            "Content-Type": "application/json",
            "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/128.0.0.0",
            "HTTP-Referer": "https://free-api-chat.app",
            "X-OpenRouter-Title": "Free API Chat"
        },
        "payload": body,
        "http_status": http_code,
        "response_body": resp_body,
        "raw_error": raw_err,
        "latency_ms": latency
    });

    // 更新数据库
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE models SET status=?1, latency_ms=?2 WHERE id=?3",
        params![status, latency, model_id],
    )
    .map_err(|e| e.to_string())?;

    Ok(json!({
        "status": status,
        "latency_ms": latency,
        "message": msg,
        "diagnostics": diagnostics
    }))
}
