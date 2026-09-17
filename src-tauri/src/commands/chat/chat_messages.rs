//! 消息读写命令。

use crate::db::{ChatMessage, ChatSession, DbState};
use crate::llm::{self, ModelConfig, StreamEvent};
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

use super::*;

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
