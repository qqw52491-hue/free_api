//! 流式聊天发送与取消。

use crate::db::{ChatMessage, ChatSession, DbState};
use crate::llm::{self, ModelConfig, StreamEvent};
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

use super::*;

// ==================== STREAM CHAT & UTILS ====================

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
