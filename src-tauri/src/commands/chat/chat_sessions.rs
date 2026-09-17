//! 会话 CRUD 命令。

use crate::db::{ChatMessage, ChatSession, DbState};
use crate::llm::{self, ModelConfig, StreamEvent};
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

use super::*;

// ==================== SESSION COMMANDS ====================

#[tauri::command]
pub async fn get_sessions(state: State<'_, DbState>) -> Result<Vec<ChatSession>, String> {
    let conn = state.reader()?;
    let mut stmt = conn.prepare(
        "SELECT id, title, model_id, created_at, updated_at, message_count FROM chat_sessions ORDER BY updated_at DESC LIMIT 1000"
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

/// 会话分叉：整会话克隆（均不传）或从某条消息/时间点分叉。
/// 复制消息历史 + checkpoint（分叉点取回合历史快照）+ 全部会话级 Agent
/// 表（id 全量重映射）+ 会话 tmp 目录物理拷贝。
#[tauri::command]
pub async fn fork_agent_session(
    state: State<'_, DbState>,
    session_id: String,
    before_message_id: Option<String>,
    before_timestamp_ms: Option<i64>,
    up_to_message_id: Option<String>,
    up_to_timestamp_ms: Option<i64>,
    prompt_seed: Option<String>,
) -> Result<crate::agent::session_fork::ForkReport, String> {
    let mut conn = state.0.lock().map_err(|e| e.to_string())?;
    let message_exists = |conn: &rusqlite::Connection, sid: &str, mid: &str| -> bool {
        conn.query_row(
            "SELECT 1 FROM chat_messages WHERE session_id=?1 AND id=?2 LIMIT 1",
            rusqlite::params![sid, mid],
            |_| Ok(()),
        )
        .is_ok()
    };

    let fork_point = if let Some(ts) = before_timestamp_ms {
        // 时间戳分叉：直接根据时间戳截断，prompt_seed 直接由前端传递的用户原话回填
        crate::agent::session_fork::ForkPoint::BeforeTimestamp {
            timestamp_ms: ts,
            prompt_seed,
        }
    } else if let Some(id) = before_message_id {
        if message_exists(&conn, &session_id, &id) {
            crate::agent::session_fork::ForkPoint::BeforeMessage(id)
        } else {
            crate::agent::session_fork::ForkPoint::Full
        }
    } else if let Some(id) = up_to_message_id {
        if message_exists(&conn, &session_id, &id) {
            crate::agent::session_fork::ForkPoint::UpToMessage(id)
        } else if let Some(ts) = up_to_timestamp_ms {
            let resolved = crate::agent::session_fork::resolve_boundary_message(&conn, &session_id, ts)?;
            match resolved {
                Some(i) => crate::agent::session_fork::ForkPoint::UpToMessage(i),
                None => crate::agent::session_fork::ForkPoint::Full,
            }
        } else {
            crate::agent::session_fork::ForkPoint::UpToMessage(id)
        }
    } else if let Some(ts) = up_to_timestamp_ms {
        let id = crate::agent::session_fork::resolve_boundary_message(&conn, &session_id, ts)?;
        match id {
            Some(i) => crate::agent::session_fork::ForkPoint::UpToMessage(i),
            None => crate::agent::session_fork::ForkPoint::Full,
        }
    } else {
        crate::agent::session_fork::ForkPoint::Full
    };
    crate::agent::session_fork::fork_session(&mut conn, &session_id, fork_point)
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
