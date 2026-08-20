use crate::agent::checkpoint::AgentCheckpoint;
use crate::agent::context::ChatMessage;
use crate::db::DbState;
use chrono::Utc;
use rusqlite::params;
use serde_json::json;
use uuid::Uuid;

const HISTORY_LIMIT: usize = 24;

fn title_from_goal(goal: &str) -> String {
    let normalized = goal.split_whitespace().collect::<Vec<_>>().join(" ");
    let candidate = if normalized.is_empty() {
        "新 Agent 对话"
    } else {
        normalized.as_str()
    };
    let clipped: String = candidate.chars().take(48).collect();
    if candidate.chars().count() > 48 {
        format!("{}…", clipped)
    } else {
        clipped
    }
}

pub fn ensure_conversation(
    db: &DbState,
    session_id: &str,
    model_id: &str,
    first_goal: &str,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO chat_sessions (id, title, model_id, created_at, updated_at, message_count)
         VALUES (?1, ?2, ?3, ?4, ?5, 0)
         ON CONFLICT(id) DO UPDATE SET model_id=excluded.model_id, updated_at=excluded.updated_at",
        params![session_id, title_from_goal(first_goal), model_id, now, now],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn append_message(
    db: &DbState,
    session_id: &str,
    role: &str,
    content: &str,
    model_id: Option<&str>,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO chat_messages (id, session_id, role, content, attachments, created_at, model_id)
         VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6)",
        params![Uuid::new_v4().to_string(), session_id, role, content, now, model_id],
    )
    .map_err(|error| error.to_string())?;
    conn.execute(
        "UPDATE chat_sessions SET updated_at=?1, message_count=message_count+1 WHERE id=?2",
        params![now, session_id],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

/// 只要 checkpoint 存在且未达终态 (Done/Failed)，一律属于【任务未结束的续接】，不开启新回合。
pub fn should_start_new_turn(existing: Option<&AgentCheckpoint>) -> bool {
    !matches!(existing, Some(checkpoint) if !checkpoint.phase.is_terminal())
}

pub fn begin_turn(
    db: &DbState,
    session_id: &str,
    model_id: &str,
    user_message: &str,
) -> Result<(), String> {
    ensure_conversation(db, session_id, model_id, user_message)?;
    append_message(db, session_id, "user", user_message, Some(model_id))
}

pub fn complete_turn(
    db: &DbState,
    session_id: &str,
    model_id: &str,
    assistant_message: &str,
) -> Result<(), String> {
    append_message(
        db,
        session_id,
        "assistant",
        assistant_message,
        Some(model_id),
    )
}

pub fn append_human_intervention(
    db: &DbState,
    session_id: &str,
    message: &str,
) -> Result<(), String> {
    append_message(db, session_id, "user", message, None)
}

pub fn recent_history(db: &DbState, session_id: &str) -> Result<Vec<ChatMessage>, String> {
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT role, content FROM (
                SELECT role, content, created_at
                FROM chat_messages
                WHERE session_id=?1
                ORDER BY created_at DESC, id DESC
                LIMIT ?2
             ) ORDER BY created_at ASC",
        )
        .map_err(|error| error.to_string())?;
    let rows = stmt
        .query_map(params![session_id, HISTORY_LIMIT], |row| {
            let role: String = row.get(0)?;
            let content: String = row.get(1)?;
            Ok(ChatMessage {
                role,
                content: json!(content),
            })
        })
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn test_db() -> DbState {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE chat_sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL, model_id TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, message_count INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE chat_messages (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, role TEXT NOT NULL, content TEXT NOT NULL, attachments TEXT, created_at TEXT NOT NULL, model_id TEXT);",
        ).unwrap();
        DbState(Arc::new(Mutex::new(conn)))
    }

    #[test]
    fn turn_persists_messages_and_loads_in_order() {
        let db = test_db();
        begin_turn(&db, "session-a", "model-a", "先分析项目").unwrap();
        complete_turn(&db, "session-a", "model-a", "已完成初步分析").unwrap();
        let history = recent_history(&db, "session-a").unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, "user");
        assert_eq!(history[1].role, "assistant");
    }

    #[test]
    fn continuation_and_live_intervention_share_one_history() {
        let db = test_db();
        begin_turn(&db, "session-b", "model-a", "创建第一份报告").unwrap();
        complete_turn(&db, "session-b", "model-a", "报告已生成").unwrap();
        begin_turn(&db, "session-b", "model-a", "把报告改成英文").unwrap();
        append_human_intervention(&db, "session-b", "暂停，先保留中文版本").unwrap();
        let history = recent_history(&db, "session-b").unwrap();
        assert_eq!(history.len(), 4);
        assert_eq!(
            history
                .iter()
                .map(|item| item.role.as_str())
                .collect::<Vec<_>>(),
            vec!["user", "assistant", "user", "user"]
        );
        assert_eq!(history[2].content.as_str(), Some("把报告改成英文"));
        assert_eq!(history[3].content.as_str(), Some("暂停，先保留中文版本"));
    }

    #[test]
    fn unfinished_checkpoint_resumes_without_duplicate_user_turn() {
        let context = crate::agent::context::SandwichContext::new(
            "system".to_string(),
            "继续整理报告".to_string(),
        );
        let mut unfinished = AgentCheckpoint::new("run-a".to_string(), &context);
        assert!(!should_start_new_turn(Some(&unfinished)));
        unfinished.mark_done(&context);
        assert!(should_start_new_turn(Some(&unfinished)));
        assert!(should_start_new_turn(None));
    }
}
