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
    let keys = crate::llm::parse_api_keys(&platform.1);
    let available_keys = if keys.is_empty() {
        vec![platform.1.trim().to_string()]
    } else {
        keys
    };

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

    let mut status = "offline";
    let mut msg = String::new();
    let mut http_code: Option<u16> = None;
    let mut resp_body = String::new();
    let mut raw_err: Option<String> = None;
    let mut latency: i64 = 0;
    let mut active_key_used = available_keys[0].clone();

    for (k_idx, key) in available_keys.iter().enumerate() {
        active_key_used = key.clone();
        let clean_key = key.trim();
        let auth_header = if clean_key.is_empty() {
            "".to_string()
        } else if clean_key.starts_with("Bearer ") {
            clean_key.to_string()
        } else {
            format!("Bearer {}", clean_key)
        };

        let start = std::time::Instant::now();
        let mut req = client
            .post(&url)
            .header("HTTP-Referer", "https://free-api-chat.app")
            .header("X-OpenRouter-Title", "Free API Chat")
            .json(&body);

        if !auth_header.is_empty() {
            req = req.header("Authorization", &auth_header);
        }

        let resp = req.send().await;
        latency = start.elapsed().as_millis() as i64;

        match resp {
            Ok(r) if r.status().is_success() => {
                let s = r.status().as_u16();
                let b = r.text().await.unwrap_or_default();
                http_code = Some(s);
                resp_body = b;
                raw_err = None;
                status = "online";
                if available_keys.len() > 1 && k_idx > 0 {
                    msg = format!("连接正常 (已自动故障转移至第 {} 个 Key: {})", k_idx + 1, crate::llm::mask_api_key(key));
                } else {
                    msg = "连接正常".to_string();
                }
                break;
            }
            Ok(r) => {
                let s = r.status().as_u16();
                let b = r.text().await.unwrap_or_default();
                http_code = Some(s);
                resp_body = b.clone();
                raw_err = None;
                status = "offline";
                msg = format!("HTTP {} - {}", s, b);

                let is_rotatable = crate::llm::is_key_rotatable_error(Some(s), &b);
                if is_rotatable && k_idx + 1 < available_keys.len() {
                    println!(
                        "🔄 [检测故障转移] 模型 [{}] 的 API Key [{}] 失败 ({}), 尝试下一个 Key...",
                        model_name,
                        crate::llm::mask_api_key(key),
                        s
                    );
                    continue;
                }
            }
            Err(e) => {
                let err_str = e.to_string();
                http_code = None;
                resp_body = String::new();
                raw_err = Some(err_str.clone());
                status = "offline";
                msg = err_str.clone();

                if k_idx + 1 < available_keys.len() {
                    continue;
                }
            }
        }
    }

    let masked_auth = if active_key_used.is_empty() {
        "None".to_string()
    } else {
        format!("Bearer {}", crate::llm::mask_api_key(&active_key_used))
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
        "latency_ms": latency,
        "total_keys_configured": available_keys.len(),
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

// ==================== REMOTE MODEL SYNC & IMPORT ====================

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RemoteModelItem {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub exists: bool,
    pub existing_id: Option<String>,
    pub matched_spec_id: Option<String>,
    pub matched_spec_name: Option<String>,
    pub context_window: i64,
    pub max_tokens: i64,
    pub temperature: f64,
    pub supports_thinking: bool,
    pub description: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FetchRemoteModelsResult {
    pub endpoint_url: String,
    pub total: usize,
    pub new_count: usize,
    pub models: Vec<RemoteModelItem>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ImportRemoteModelPayload {
    pub name: String,
    pub display_name: String,
    pub context_window: i64,
    pub max_tokens: i64,
    pub temperature: f64,
    pub provider_capabilities: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportRemoteModelsResult {
    pub added_count: usize,
    pub updated_count: usize,
    pub total_processed: usize,
}

fn extract_model_ids_from_json(val: &Value) -> Vec<String> {
    let mut ids = Vec::new();

    if let Some(arr) = val.get("data").and_then(|d| d.as_array()) {
        for item in arr {
            if let Some(s) = item.as_str() {
                ids.push(s.trim().to_string());
            } else if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                ids.push(id.trim().to_string());
            } else if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                ids.push(name.trim().to_string());
            } else if let Some(model) = item.get("model").and_then(|v| v.as_str()) {
                ids.push(model.trim().to_string());
            }
        }
    } else if let Some(arr) = val.get("models").and_then(|d| d.as_array()) {
        for item in arr {
            if let Some(s) = item.as_str() {
                ids.push(s.trim().to_string());
            } else if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                ids.push(name.trim().to_string());
            } else if let Some(model) = item.get("model").and_then(|v| v.as_str()) {
                ids.push(model.trim().to_string());
            } else if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                ids.push(id.trim().to_string());
            }
        }
    } else if let Some(arr) = val.as_array() {
        for item in arr {
            if let Some(s) = item.as_str() {
                ids.push(s.trim().to_string());
            } else if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                ids.push(id.trim().to_string());
            } else if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                ids.push(name.trim().to_string());
            }
        }
    }

    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for id in ids {
        if !id.is_empty() && seen.insert(id.clone()) {
            result.push(id);
        }
    }
    result
}

fn find_best_spec_match<'a>(model_id: &str, specs: &'a [ModelSpec]) -> Option<&'a ModelSpec> {
    let lower_id = model_id.to_lowercase();
    let normalized = lower_id
        .replace('_', "-")
        .replace('.', "-")
        .replace(':', "-");

    // 1. 优先根据模型特征匹配标准容量档位
    if normalized.contains("gemini-1-5-pro")
        || normalized.contains("gemini-1.5-pro")
        || normalized.contains("gemini-pro")
        || normalized.contains("2m")
    {
        if let Some(s) = specs.iter().find(|s| s.name.contains("2M")) {
            return Some(s);
        }
    }

    if normalized.contains("gemini") || normalized.contains("1m") || normalized.contains("minimax")
    {
        if let Some(s) = specs.iter().find(|s| s.name.contains("1M")) {
            return Some(s);
        }
    }

    if normalized.contains("claude")
        || normalized.contains("kimi")
        || normalized.contains("moonshot")
        || normalized.contains("200k")
    {
        if let Some(s) = specs.iter().find(|s| s.name.contains("200k")) {
            return Some(s);
        }
    }

    if normalized.contains("r1")
        || normalized.contains("reasoner")
        || normalized.contains("o1")
        || normalized.contains("o3")
        || normalized.contains("thinking")
    {
        if let Some(s) = specs.iter().find(|s| {
            s.name.contains("深度思考") || (s.supports_thinking && s.context_window >= 128000)
        }) {
            return Some(s);
        }
    }

    if normalized.contains("7b")
        || normalized.contains("8b")
        || normalized.contains("32k")
        || normalized.contains("ollama")
        || normalized.contains("small")
    {
        if let Some(s) = specs.iter().find(|s| s.name.contains("32k")) {
            return Some(s);
        }
    }

    // 2. 兜底匹配最常见的主流 128k 标配档
    if let Some(s) = specs
        .iter()
        .find(|s| s.name.contains("128k") && !s.supports_thinking)
    {
        return Some(s);
    }

    specs.first()
}

/// 从远程 OpenAI 兼容接口拉取模型列表，并自动对照规格词典进行参数解析与匹配
#[tauri::command]
pub async fn fetch_remote_models(
    state: State<'_, DbState>,
    platform_id: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
) -> Result<FetchRemoteModelsResult, String> {
    let (eff_base_url, eff_api_key, existing_models, specs) = {
        let conn = state.0.lock().map_err(|e| e.to_string())?;

        let (b_url, a_key) = if let Some(ref pid) = platform_id {
            let mut stmt = conn
                .prepare("SELECT base_url, api_key FROM platforms WHERE id = ?1")
                .map_err(|e| e.to_string())?;
            stmt.query_row(params![pid], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("未找到指定平台: {}", e))?
        } else {
            let b = base_url.ok_or_else(|| "缺少 base_url".to_string())?;
            let k = api_key.unwrap_or_default();
            (b, k)
        };

        let mut existing = std::collections::HashMap::new();
        if let Some(ref pid) = platform_id {
            let mut stmt = conn
                .prepare("SELECT id, name FROM models WHERE platform_id = ?1")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![pid], |row| {
                    Ok((row.get::<_, String>(1)?, row.get::<_, String>(0)?))
                })
                .map_err(|e| e.to_string())?;
            for r in rows.flatten() {
                existing.insert(r.0, r.1);
            }
        }

        let specs = get_all_model_specs(&conn).unwrap_or_default();
        (b_url, a_key, existing, specs)
    };

    let clean_base = eff_base_url.trim().trim_end_matches('/');
    let keys = crate::llm::parse_api_keys(&eff_api_key);
    let available_keys = if keys.is_empty() {
        vec![eff_api_key.trim().to_string()]
    } else {
        keys
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .connect_timeout(std::time::Duration::from_secs(8))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/128.0.0.0")
        .build()
        .map_err(|e| e.to_string())?;

    let base_trimmed = if clean_base.ends_with("/chat/completions") {
        clean_base.trim_end_matches("/chat/completions")
    } else if clean_base.ends_with("/models") {
        clean_base.trim_end_matches("/models")
    } else {
        clean_base
    };

    let target_url = format!("{}/models", base_trimmed);

    let mut last_fetch_err = String::new();
    let mut json_val: Option<Value> = None;

    for key in &available_keys {
        let clean_key = key.trim();
        let auth_header = if clean_key.is_empty() {
            "".to_string()
        } else if clean_key.starts_with("Bearer ") {
            clean_key.to_string()
        } else {
            format!("Bearer {}", clean_key)
        };

        let mut req = client
            .get(&target_url)
            .header("HTTP-Referer", "https://free-api-chat.app")
            .header("X-OpenRouter-Title", "Free API Chat");
        if !auth_header.is_empty() {
            req = req.header("Authorization", &auth_header);
        }

        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<Value>().await {
                    Ok(jv) => {
                        json_val = Some(jv);
                        break;
                    }
                    Err(e) => {
                        last_fetch_err = format!("端点 {} 响应解析 JSON 失败: {}", target_url, e);
                    }
                }
            }
            Ok(resp) => {
                let status = resp.status();
                let err_text = resp.text().await.unwrap_or_default();
                last_fetch_err = format!(
                    "无法从接口拉取模型列表。端点: {}，HTTP {} 错误: {}",
                    target_url,
                    status.as_u16(),
                    err_text
                );
                if !crate::llm::is_key_rotatable_error(Some(status.as_u16()), &err_text) {
                    break;
                }
            }
            Err(e) => {
                last_fetch_err = format!("请求 {} 失败: {}", target_url, e);
            }
        }
    }

    let json_val = match json_val {
        Some(v) => v,
        None => return Err(last_fetch_err),
    };

    let used_url = target_url;

    let model_ids = extract_model_ids_from_json(&json_val);
    if model_ids.is_empty() {
        return Err(format!(
            "从接口 {} 成功获取响应，但未能解析到任何有效模型标识。",
            used_url
        ));
    }

    let mut items = Vec::new();
    let mut new_count = 0;

    for id in model_ids {
        let exists = existing_models.contains_key(&id);
        let existing_id = existing_models.get(&id).cloned();
        if !exists {
            new_count += 1;
        }

        let matched_spec = find_best_spec_match(&id, &specs);
        let (spec_id, spec_name, ctx, max_tok, temp, thinking, desc) = if let Some(s) = matched_spec
        {
            (
                Some(s.id.clone()),
                Some(s.name.clone()),
                s.context_window,
                s.max_tokens,
                s.default_temperature,
                s.supports_thinking,
                Some(s.description.clone()),
            )
        } else {
            let lower = id.to_lowercase();
            let (ctx, max_tok, temp, thinking) = if lower.contains("deepseek-r1")
                || lower.contains("reasoner")
                || lower.contains("thinking")
                || lower.contains("r1")
            {
                (128000, 8192, 0.6, true)
            } else if lower.contains("deepseek") {
                (128000, 8192, 0.7, false)
            } else if lower.contains("gemini-2.0") || lower.contains("gemini-2") {
                (1048576, 8192, 0.7, false)
            } else if lower.contains("gemini-1.5-pro") {
                (2097152, 8192, 0.7, false)
            } else if lower.contains("gemini") {
                (1048576, 8192, 0.7, false)
            } else if lower.contains("claude-3-7") || lower.contains("claude-3.7") {
                (200000, 8192, 0.7, true)
            } else if lower.contains("claude-3-5") || lower.contains("claude-3.5") {
                (200000, 8192, 0.5, false)
            } else if lower.contains("claude") {
                (200000, 4096, 0.7, false)
            } else if lower.contains("gpt-4o-mini") {
                (128000, 16384, 0.7, false)
            } else if lower.contains("gpt-4o") || lower.contains("o1") || lower.contains("o3") {
                (128000, 16384, 0.7, true)
            } else if lower.contains("sensenova") {
                (128000, 8192, 0.7, false)
            } else if lower.contains("qwen") {
                (32768, 4096, 0.7, false)
            } else {
                (32768, 4096, 0.7, false)
            };
            (None, None, ctx, max_tok, temp, thinking, None)
        };

        let disp_name = id.clone();

        items.push(RemoteModelItem {
            id: id.clone(),
            name: id.clone(),
            display_name: disp_name,
            exists,
            existing_id,
            matched_spec_id: spec_id,
            matched_spec_name: spec_name,
            context_window: ctx,
            max_tokens: max_tok,
            temperature: temp,
            supports_thinking: thinking,
            description: desc,
        });
    }

    Ok(FetchRemoteModelsResult {
        endpoint_url: used_url,
        total: items.len(),
        new_count,
        models: items,
    })
}

/// 批量将远程拉取到的模型写入 / 更新至 SQLite 数据库
#[tauri::command]
pub async fn import_remote_models(
    state: State<'_, DbState>,
    platform_id: String,
    models: Vec<ImportRemoteModelPayload>,
) -> Result<ImportRemoteModelsResult, String> {
    if models.is_empty() {
        return Ok(ImportRemoteModelsResult {
            added_count: 0,
            updated_count: 0,
            total_processed: 0,
        });
    }

    let conn = state.0.lock().map_err(|e| e.to_string())?;

    let mut check_stmt = conn
        .prepare("SELECT id FROM platforms WHERE id = ?1")
        .map_err(|e| e.to_string())?;
    let platform_exists: bool = check_stmt
        .exists(params![platform_id])
        .map_err(|e| e.to_string())?;
    if !platform_exists {
        return Err("所选平台不存在，无法导入模型".to_string());
    }

    let mut added_count = 0;
    let mut updated_count = 0;
    let now = Utc::now().to_rfc3339();

    for m in &models {
        let name = m.name.trim();
        if name.is_empty() {
            continue;
        }
        let disp = if m.display_name.trim().is_empty() {
            name.to_string()
        } else {
            m.display_name.trim().to_string()
        };
        let prov_cap = m
            .provider_capabilities
            .clone()
            .unwrap_or_else(|| "{}".to_string());

        let mut find_stmt = conn
            .prepare("SELECT id FROM models WHERE platform_id = ?1 AND name = ?2")
            .map_err(|e| e.to_string())?;
        let existing_id: Option<String> = find_stmt
            .query_row(params![platform_id, name], |r| r.get(0))
            .ok();

        if let Some(id) = existing_id {
            conn.execute(
                "UPDATE models SET display_name = ?1, max_tokens = ?2, temperature = ?3, context_window = ?4, provider_capabilities = ?5 WHERE id = ?6",
                params![disp, m.max_tokens, m.temperature, m.context_window, prov_cap, id],
            ).map_err(|e| e.to_string())?;
            updated_count += 1;
        } else {
            let new_id = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO models (id, platform_id, name, display_name, max_tokens, temperature, enabled, status, latency_ms, context_window, provider_capabilities, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 'unknown', 0, ?7, ?8, ?9)",
                params![new_id, platform_id, name, disp, m.max_tokens, m.temperature, m.context_window, prov_cap, now],
            ).map_err(|e| e.to_string())?;
            added_count += 1;
        }
    }

    Ok(ImportRemoteModelsResult {
        added_count,
        updated_count,
        total_processed: added_count + updated_count,
    })
}
