use crate::db::{ChatMessage, ChatSession, DbState};
use crate::llm::{self, ModelConfig, StreamEvent};
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

// ==================== SESSION COMMANDS ====================

#[tauri::command]
pub async fn get_sessions(state: State<'_, DbState>) -> Result<Vec<ChatSession>, String> {
    let conn = state.reader()?;
    let mut stmt = conn.prepare(
        "SELECT id, title, model_id, created_at, updated_at, message_count FROM chat_sessions ORDER BY updated_at DESC LIMIT 100"
    ).map_err(|e| e.to_string())?;
    let list = stmt
        .query_map([], |row| {
            Ok(ChatSession {
                id: row.get(0)?,
                title: row.get(1)?,
                model_id: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
                message_count: row.get(5)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
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
    Ok(ChatSession {
        id,
        title,
        model_id,
        created_at: now.clone(),
        updated_at: now,
        message_count: 0,
    })
}

#[tauri::command]
pub async fn delete_session(
    app: AppHandle,
    state: State<'_, DbState>,
    id: String,
) -> Result<(), String> {
    let app_dir = app.path().app_data_dir().ok();
    {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        crate::db::delete_session_completely(&conn, &id, app_dir.as_deref())?;
    }
    crate::agent::retrieval::clear_session_search_index(&id);
    Ok(())
}

#[tauri::command]
pub async fn vacuum_database(state: State<'_, DbState>) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    crate::db::vacuum_database(&conn)
}

#[tauri::command]
pub async fn rename_session(
    state: State<'_, DbState>,
    id: String,
    title: String,
) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE chat_sessions SET title=?1 WHERE id=?2",
        params![title, id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

// ==================== MESSAGE COMMANDS ====================

#[tauri::command]
pub async fn get_messages(
    state: State<'_, DbState>,
    session_id: String,
) -> Result<Vec<ChatMessage>, String> {
    let conn = state.reader()?;
    let mut stmt = conn.prepare(
        "SELECT id, session_id, role, content, attachments, created_at, model_id FROM chat_messages WHERE session_id=?1 ORDER BY created_at ASC"
    ).map_err(|e| e.to_string())?;
    let list = stmt
        .query_map(params![session_id], |row| {
            Ok(ChatMessage {
                id: row.get(0)?,
                session_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                attachments: row.get(4)?,
                created_at: row.get(5)?,
                model_id: row.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
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
    )
    .map_err(|e| e.to_string())?;
    Ok(ChatMessage {
        id,
        session_id,
        role,
        content,
        attachments,
        created_at: now,
        model_id,
    })
}

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::{watch, Mutex};

static CHAT_INTERRUPT_CHANNELS: OnceLock<Arc<Mutex<HashMap<String, watch::Sender<u64>>>>> =
    OnceLock::new();

fn chat_interrupt_channels() -> &'static Arc<Mutex<HashMap<String, watch::Sender<u64>>>> {
    CHAT_INTERRUPT_CHANNELS.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

#[tauri::command]
pub async fn cancel_chat(session_id: String) -> Result<(), String> {
    if let Some(sender) = chat_interrupt_channels()
        .lock()
        .await
        .get(&session_id)
        .cloned()
    {
        sender.send_modify(|generation| *generation = generation.wrapping_add(1));
        Ok(())
    } else {
        Err("对话未在进行中或已结束".to_string())
    }
}

// ==================== STREAM CHAT & UTILS ====================

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
    let config = ModelConfig::from_db(&state, &model_id)?;

    // 处理附件/图片
    let mut api_messages = messages.clone();
    if let Some(ref files) = attachments {
        if !files.is_empty() {
            if let Some(last) = api_messages.last_mut() {
                if last.get("role").and_then(|r| r.as_str()) == Some("user") {
                    let text = last["content"].as_str().unwrap_or("").to_string();
                    let mut parts: Vec<serde_json::Value> =
                        vec![json!({"type": "text", "text": text})];
                    for file in files {
                        if file.mime_type.starts_with("image/") {
                            parts.push(json!({
                                "type": "image_url",
                                "image_url": {
                                    "url": format!("data:{};base64,{}", file.mime_type, file.data_base64)
                                }
                            }));
                        }
                    }
                    *last = json!({"role": "user", "content": parts});
                }
            }
        }
    }

    // 实时事件回调
    let mut is_thinking = false;
    let mut on_event = |event: StreamEvent| {
        match event {
            StreamEvent::Thinking {
                chunk,
                accumulated: _,
            } => {
                if !is_thinking {
                    is_thinking = true;
                    app.emit(
                        "chat-thinking",
                        json!({"session_id": session_id, "status": "start"}),
                    )
                    .ok();
                }
                app.emit(
                    "chat-thinking",
                    json!({
                        "session_id": session_id,
                        "content": chunk,
                        "status": "streaming"
                    }),
                )
                .ok();
            }
            StreamEvent::Content {
                chunk,
                full_thinking,
                ..
            } => {
                if is_thinking {
                    is_thinking = false;
                    app.emit(
                        "chat-thinking",
                        json!({
                            "session_id": session_id,
                            "status": "done",
                            "full_thinking": full_thinking
                        }),
                    )
                    .ok();
                }
                app.emit(
                    "chat-stream",
                    json!({
                        "session_id": session_id,
                        "content": chunk,
                        "done": false
                    }),
                )
                .ok();
            }
        }
        true
    };

    let (interrupt_tx, mut interrupt_rx) = tokio::sync::watch::channel(0u64);
    chat_interrupt_channels()
        .lock()
        .await
        .insert(session_id.clone(), interrupt_tx);

    let stream_result = llm::stream_with_retry(
        &config,
        &api_messages,
        "high",
        &mut interrupt_rx,
        3,
        &mut on_event,
    )
    .await;

    chat_interrupt_channels().lock().await.remove(&session_id);

    let result = match stream_result {
        Ok(r) => r,
        Err(e) if e == llm::STREAM_INTERRUPTED_ERROR => {
            if is_thinking {
                app.emit(
                    "chat-thinking",
                    json!({
                        "session_id": session_id,
                        "status": "done",
                    }),
                )
                .ok();
            }
            app.emit(
                "chat-stream",
                json!({
                    "session_id": session_id,
                    "content": "",
                    "done": true,
                    "interrupted": true
                }),
            )
            .ok();
            return Err("用户已中断生成".to_string());
        }
        Err(e) => return Err(e),
    };

    // 思考结束通知
    if is_thinking && !result.thinking_text.is_empty() {
        app.emit(
            "chat-thinking",
            json!({
                "session_id": session_id,
                "status": "done",
                "full_thinking": &result.thinking_text
            }),
        )
        .ok();
    }

    if !result.thinking_text.is_empty() {
        println!(
            "🧠 思考过程 ({} 字符): {}...",
            result.thinking_text.len(),
            result.thinking_text.chars().take(200).collect::<String>()
        );
    }

    // 流结束通知
    app.emit(
        "chat-stream",
        json!({
            "session_id": session_id,
            "content": "",
            "done": true,
            "thinking": if result.thinking_text.is_empty() { None } else { Some(&result.thinking_text) }
        }),
    )
    .ok();

    // Token 用量统计
    let raw_usage = result.usage.unwrap_or_else(|| {
        let (p, c) = llm::estimate_usage(
            &api_messages,
            result.full_response.len(),
            config.context_window,
        );
        llm::RawUsage {
            prompt_tokens: p,
            completion_tokens: c,
            cached_tokens: 0,
        }
    });
    let final_prompt = raw_usage.prompt_tokens;
    let final_completion = raw_usage.completion_tokens;
    let final_cached = raw_usage.cached_tokens;
    let final_total = final_prompt + final_completion;
    let final_percent = if config.context_window > 0 {
        (final_total as f64 / config.context_window as f64) * 100.0
    } else {
        0.0
    };
    let cache_hit_rate = if final_prompt > 0 {
        (final_cached as f64 / final_prompt as f64) * 100.0
    } else {
        0.0
    };

    app.emit(
        "chat-token-usage",
        json!({
            "session_id": session_id,
            "prompt_tokens": final_prompt,
            "completion_tokens": final_completion,
            "total_tokens": final_total,
            "cached_tokens": final_cached,
            "cache_hit_rate": cache_hit_rate,
            "context_window": config.context_window,
            "usage_percent": final_percent,
            "ttft_ms": result.ttft_ms,
            "total_duration_ms": result.total_duration_ms,
            "tokens_per_second": result.tokens_per_second
        }),
    )
    .ok();

    println!(
        "📊 Chat Token: 输入={}, 输出={}, 合计={} | 上下文: {}/{} ({:.1}%)",
        final_prompt,
        final_completion,
        final_total,
        final_total,
        config.context_window,
        final_percent
    );

    Ok(result.full_response)
}

/// 微信式原生独立 WebView 窗口预览
#[tauri::command]
pub async fn open_in_webview_window(
    app: tauri::AppHandle,
    url: String,
    title: Option<String>,
) -> Result<(), String> {
    use tauri::{WebviewUrl, WebviewWindowBuilder};

    let parsed_url = tauri::Url::parse(&url).map_err(|e| format!("无效 URL: {}", e))?;
    let short_uuid = uuid::Uuid::new_v4().to_string().replace('-', "")[..12].to_string();
    let window_label = format!("preview-{}", short_uuid);
    let window_title = title.unwrap_or_else(|| "网页预览".to_string());

    let init_script = r#"
        (function() {
            // 修复 Webview 中 target="_blank" 和 window.open 点不动的问题（百度/Google/B站 搜索结果默认都是 _blank）
            document.addEventListener('click', function(e) {
                var a = e.target.closest('a');
                if (a && (a.target === '_blank' || a.getAttribute('target') === '_blank')) {
                    a.target = '_self';
                }
            }, true);

            var origOpen = window.open;
            window.open = function(url) {
                if (url && typeof url === 'string') {
                    window.location.href = url;
                    return window;
                }
                return origOpen ? origOpen.apply(window, arguments) : window;
            };
        })();
    "#;

    WebviewWindowBuilder::new(&app, &window_label, WebviewUrl::External(parsed_url))
        .title(&window_title)
        .inner_size(1120.0, 780.0)
        .center()
        .resizable(true)
        .focused(true)
        .initialization_script(init_script)
        .build()
        .map_err(|e| format!("创建原生 WebView 窗口失败: {}", e))?;

    Ok(())
}

/// 原生聚合互联网搜索
#[tauri::command]
pub async fn search_web(
    query: String,
    max_results: Option<usize>,
) -> Result<Vec<crate::agent::web_search::WebSearchResultItem>, String> {
    crate::agent::web_search::execute_web_search(&query, max_results.unwrap_or(5)).await
}

/// 原生抓取并提取网页 Markdown
#[tauri::command]
pub async fn fetch_url_markdown(url: String) -> Result<String, String> {
    crate::agent::web_search::fetch_url_markdown(&url).await
}

pub fn resolve_safe_artifact_path(raw: &str) -> Result<std::path::PathBuf, String> {
    let trimmed = raw.trim().trim_matches('"').trim_matches('\'');
    let clean = trimmed.strip_prefix("file://").unwrap_or(trimmed).trim();
    if clean.is_empty() || clean == "[object Object]" {
        return Err("未提供有效的文件路径".to_string());
    }

    let path_buf = std::path::PathBuf::from(clean);
    if path_buf.exists() {
        return path_buf
            .canonicalize()
            .map_err(|e| format!("无法解析路径: {}", e));
    }

    // 1. 优先在应用数据目录的全局会话临时受管目录中查找 (解决 tmp/ 相对快捷路径或纯文件名)
    let global_tmp = dirs_next::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("free-api-chat")
        .join("tmp");

    if global_tmp.exists() {
        let stripped = clean.strip_prefix("./").unwrap_or(clean);
        let sub_path = stripped.strip_prefix("tmp/").unwrap_or(stripped);

        // 1.1 直接检查全局临时根目录下的 sub_path
        let direct_candidate = global_tmp.join(sub_path);
        if direct_candidate.exists() {
            return direct_candidate
                .canonicalize()
                .map_err(|e| format!("无法解析路径: {}", e));
        }

        // 1.2 遍历所有会话子目录，查找最新的匹配文件
        if let Ok(entries) = std::fs::read_dir(&global_tmp) {
            let mut candidates: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
            for entry in entries.flatten() {
                let session_dir = entry.path();
                if session_dir.is_dir() {
                    let candidate = session_dir.join(sub_path);
                    if candidate.exists() {
                        let mtime = candidate
                            .metadata()
                            .and_then(|m| m.modified())
                            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                        candidates.push((mtime, candidate));
                    }
                }
            }
            if !candidates.is_empty() {
                candidates.sort_by(|a, b| b.0.cmp(&a.0));
                return candidates[0]
                    .1
                    .canonicalize()
                    .map_err(|e| format!("无法解析路径: {}", e));
            }
        }
    }

    // 2. 尝试基于当前工作目录以及父级工作区目录查找相对路径
    if let Ok(current_dir) = std::env::current_dir() {
        let candidate = current_dir.join(clean);
        if candidate.exists() {
            return candidate
                .canonicalize()
                .map_err(|e| format!("无法解析路径: {}", e));
        }
        if let Some(parent) = current_dir.parent() {
            let candidate_parent = parent.join(clean);
            if candidate_parent.exists() {
                return candidate_parent
                    .canonicalize()
                    .map_err(|e| format!("无法解析路径: {}", e));
            }
        }
    }

    // 3. 尝试在操作系统系统临时目录下查找
    let sys_tmp = std::env::temp_dir();
    let sys_candidate = sys_tmp.join(clean);
    if sys_candidate.exists() {
        return sys_candidate
            .canonicalize()
            .map_err(|e| format!("无法解析路径: {}", e));
    }
    let stripped = clean.strip_prefix("./").unwrap_or(clean);
    if let Some(sub) = stripped.strip_prefix("tmp/") {
        let sys_sub_candidate = sys_tmp.join(sub);
        if sys_sub_candidate.exists() {
            return sys_sub_candidate
                .canonicalize()
                .map_err(|e| format!("无法解析路径: {}", e));
        }
    }

    Err(format!("文件不存在: {}", clean))
}

/// 原生安全在系统中打开交付物文件 (使用系统关联默认程序如 PDF 查看器/Excel/代码编辑器)
#[tauri::command]
pub async fn open_artifact_in_system(app: tauri::AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;

    let canonical = resolve_safe_artifact_path(&path)?;
    let canonical_str = canonical.to_string_lossy().to_string();

    app.opener()
        .open_path(&canonical_str, None::<&str>)
        .map_err(|e| format!("无法在系统默认应用中打开文件: {}", e))?;

    Ok(())
}

/// 在系统文件管理器中高亮定位交付物文件
#[tauri::command]
pub async fn show_artifact_in_folder(app: tauri::AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;

    let canonical = resolve_safe_artifact_path(&path)?;
    let canonical_str = canonical.to_string_lossy().to_string();

    app.opener()
        .reveal_item_in_dir(&canonical_str)
        .map_err(|e| format!("无法在文件夹中定位文件: {}", e))?;

    Ok(())
}

/// 安全读取本地文本/Markdown/HTML/JSON 产物文件内容供前端直接在线预览
#[tauri::command]
pub async fn read_artifact_file(path: String) -> Result<String, String> {
    let canonical = resolve_safe_artifact_path(&path)?;

    // 限制读取文本类文件，防止误读超大二进制导致内存溢出 (上限 10MB)
    let metadata = std::fs::metadata(&canonical).map_err(|e| format!("无法获取文件信息: {}", e))?;
    if metadata.len() > 10 * 1024 * 1024 {
        return Err("文件过大（超过 10MB），建议直接在系统默认程序中打开".to_string());
    }

    std::fs::read_to_string(&canonical).map_err(|e| format!("无法以文本形式读取此文件: {}", e))
}

/// 解析本地路径的真实物理绝对路径
#[tauri::command]
pub async fn resolve_artifact_absolute_path(path: String) -> Result<String, String> {
    let canonical = resolve_safe_artifact_path(&path)?;
    Ok(canonical.to_string_lossy().to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedAttachmentResult {
    pub file_name: String,
    pub file_path: String,
    pub size_bytes: u64,
    pub content_type: String,
    pub prompt_text: String,
    pub image_data_url: Option<String>,
    pub raw_preview: Option<String>,
}

/// 解析用户选择或拖拽的本地文件内容
#[tauri::command]
pub async fn parse_attachment_file(path: String) -> Result<ParsedAttachmentResult, String> {
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(format!("文件不存在: {}", path));
    }
    let metadata = std::fs::metadata(p).map_err(|e| format!("获取文件属性失败: {}", e))?;
    let file_name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    let extracted = crate::agent::file_parser::parse_file(p, None)?;
    let prompt_text = crate::agent::file_parser::format_for_prompt(&extracted, &file_name);

    let (content_type, image_data_url, raw_preview) = match &extracted {
        crate::agent::file_parser::ExtractedContent::Text { text, .. } => (
            "text".to_string(),
            None,
            Some(text.chars().take(500).collect()),
        ),
        crate::agent::file_parser::ExtractedContent::Spreadsheet { summary, .. } => {
            ("spreadsheet".to_string(), None, Some(summary.clone()))
        }
        crate::agent::file_parser::ExtractedContent::Document { text, .. } => (
            "document".to_string(),
            None,
            Some(text.chars().take(500).collect()),
        ),
        crate::agent::file_parser::ExtractedContent::Pdf { text, .. } => {
            ("pdf".to_string(), None, Some(text.chars().take(500).collect()))
        }
        crate::agent::file_parser::ExtractedContent::Image {
            mime_type,
            data_url,
            ..
        } => (mime_type.clone(), Some(data_url.clone()), None),
        crate::agent::file_parser::ExtractedContent::Archive { files, .. } => (
            "archive".to_string(),
            None,
            Some(
                files
                    .iter()
                    .take(10)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        ),
        crate::agent::file_parser::ExtractedContent::Binary { mime_guess, .. } => {
            (mime_guess.clone(), None, None)
        }
    };

    Ok(ParsedAttachmentResult {
        file_name,
        file_path: path,
        size_bytes: metadata.len(),
        content_type,
        prompt_text,
        image_data_url,
        raw_preview,
    })
}

/// 保存并解析从前端上传/拖拽的 Base64 文件
#[tauri::command]
pub async fn save_and_parse_uploaded_attachment(
    app: AppHandle,
    state: State<'_, DbState>,
    session_id: String,
    file_name: String,
    base64_data: String,
) -> Result<ParsedAttachmentResult, String> {
    use base64::Engine;
    use tauri::Manager;

    let clean_b64 = if let Some(pos) = base64_data.find("base64,") {
        &base64_data[pos + 7..]
    } else {
        &base64_data
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(clean_b64.trim())
        .map_err(|e| format!("Base64 解码失败: {}", e))?;

    let app_dir = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let attach_dir = app_dir.join("attachments").join(&session_id);
    std::fs::create_dir_all(&attach_dir).map_err(|e| format!("创建附件目录失败: {}", e))?;

    let target_path = attach_dir.join(&file_name);
    std::fs::write(&target_path, &bytes).map_err(|e| format!("保存附件文件失败: {}", e))?;

    let path_str = target_path.to_string_lossy().to_string();
    let mut parsed = parse_attachment_file(path_str.clone()).await?;

    // 📎 附件全文句柄：项目会话自动登记为受管资源，模型后续可按句柄回读全文/结构化摘要
    let handle_line = {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        match crate::commands::projects::register_attachment_for_session(
            &conn,
            &session_id,
            &target_path,
            &file_name,
        ) {
            Ok(Some(resource_id)) => Some(resource_id),
            _ => None,
        }
    };
    if let Some(resource_id) = handle_line {
        parsed.prompt_text = format!(
            "{}\n📎 附件全文句柄已登记：resource_id=`{}`。上方内容若为摘要或截断预览，如需完整内容请调用 project_resources.read_resource(resource_id=\"{}\")。",
            parsed.prompt_text, resource_id, resource_id
        );
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_safe_artifact_path_for_session_tmp() {
        // 测试解析真实存在的 tmp 相对路径
        let res = resolve_safe_artifact_path("tmp/beijing_weather_5days.xlsx");
        if res.is_ok() {
            let path = res.unwrap();
            assert!(path.is_absolute());
            assert!(path.exists());
            assert!(path.to_string_lossy().contains("beijing_weather_5days.xlsx"));
        }

        // 测试纯文件名
        let res_file = resolve_safe_artifact_path("beijing_weather_5days.xlsx");
        if res_file.is_ok() {
            let path = res_file.unwrap();
            assert!(path.is_absolute());
            assert!(path.exists());
        }

        // 测试不存在的虚拟路径
        let res_err = resolve_safe_artifact_path("tmp/absolutely_non_existent_file_12345.xyz");
        assert!(res_err.is_err());
        assert!(res_err.unwrap_err().contains("文件不存在"));
    }
}
