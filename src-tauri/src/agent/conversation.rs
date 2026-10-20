use crate::agent::checkpoint::AgentCheckpoint;
use crate::agent::context::ChatMessage;
use crate::db::DbState;
use chrono::Utc;
use rusqlite::params;
use serde_json::json;
use uuid::Uuid;

/// 对应 chat_messages 的稳定位置；与提示词正文及压缩结果无关。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ConversationCursor {
    pub created_at: String,
    pub id: String,
}

pub(crate) struct EntryHistory {
    pub messages: Vec<ChatMessage>,
    pub cursor: Option<ConversationCursor>,
    pub legacy_reference: Option<ChatMessage>,
}

/// 只导入检查点尚未接收的消息。旧检查点没有游标时，已运行区间内的聊天
/// 单独标记为历史参考（保留可能尚未消费的人工回答），不重新当作最新指令。
pub(crate) fn history_for_entry(
    db: &DbState,
    session_id: &str,
    checkpoint: Option<&AgentCheckpoint>,
) -> Result<EntryHistory, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let previous = checkpoint.and_then(|cp| cp.data.conversation_cursor.as_ref());
    let previous_rowid = previous
        .map(|last| {
            use rusqlite::OptionalExtension;
            conn.query_row(
                "SELECT rowid FROM chat_messages WHERE session_id=?1 AND id=?2",
                params![session_id, last.id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
        })
        .transpose()
        .map_err(|e| e.to_string())?
        .flatten();
    let mut stmt = conn
        .prepare(
            "SELECT id, created_at, role, content, rowid FROM chat_messages
         WHERE session_id=?1 AND (?2 IS NULL OR rowid>?2) ORDER BY rowid ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![session_id, previous_rowid], |row| {
            Ok((
                ConversationCursor {
                    id: row.get(0)?,
                    created_at: row.get(1)?,
                },
                ChatMessage {
                    role: row.get(2)?,
                    content: json!(row.get::<_, String>(3)?),
                },
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let legacy = checkpoint.filter(|_| previous.is_none());
    let mut messages = Vec::new();
    let mut references = Vec::new();
    let mut cursor = previous.cloned();
    for row in rows {
        let (position, message, rowid) = row.map_err(|e| e.to_string())?;
        cursor = Some(position.clone());
        if checkpoint.is_some_and(|cp| cp.data.acknowledged_chat_ids.contains(&position.id)) {
            continue;
        }
        if let Some(last_rowid) = previous_rowid {
            if rowid > last_rowid {
                messages.push(message);
            }
        } else if legacy.is_some() || previous.is_some() {
            // 按时间点比较，兼容旧库不同的小数精度/时区写法。
            let time = chrono::DateTime::parse_from_rfc3339(&position.created_at)
                .map_err(|e| format!("聊天时间无效: {e}"))?;
            let cp = checkpoint.unwrap();
            let cutoff = previous
                .map(|last| last.created_at.as_str())
                .unwrap_or(&cp.updated_at);
            let updated = chrono::DateTime::parse_from_rfc3339(cutoff)
                .map_err(|e| format!("检查点时间无效: {e}"))?;
            let started = chrono::DateTime::parse_from_rfc3339(&cp.created_at)
                .map_err(|e| format!("检查点起点无效: {e}"))?;
            if time > updated {
                messages.push(message);
            } else if previous.is_some() || time >= started {
                references.push(
                    json!({"created_at":position.created_at,"message_id":position.id,
                    "role":message.role,"content":message.content}),
                );
            }
        } else {
            messages.push(message);
        }
    }
    let legacy_reference = (!references.is_empty()).then(|| ChatMessage {
        role: "user".into(),
        content: json!(format!(
            "【历史聊天参考：按入库顺序，附原始时间】\n旧检查点未记录消息游标，或游标对应的消息已删除，以下记录的消费位置无法完全恢复，可能包含已处理或尚未处理的人工回答。它们是历史原文，不是本轮新增指令；当前新增消息见后续。\n{}",
            serde_json::to_string(&references).unwrap_or_default()
        )),
    });
    Ok(EntryHistory {
        messages,
        cursor,
        legacy_reference,
    })
}

/// 运行期只接入游标之后的用户消息；已通过 ask_human 消费的回答按身份跳过。
/// 缺失游标时不猜历史消费位置，留给既有恢复入口处理。
pub(crate) fn import_live_messages(
    db: &DbState,
    session_id: &str,
    context: &mut crate::agent::context::SandwichContext,
) -> Result<usize, String> {
    let Some(previous) = context.conversation_cursor.as_ref() else { return Ok(0) };
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare(
        "SELECT id, created_at, role, content FROM chat_messages
         WHERE session_id=?1 AND rowid>(SELECT rowid FROM chat_messages WHERE session_id=?1 AND id=?2)
         ORDER BY rowid ASC",
    ).map_err(|e| e.to_string())?;
    let rows = stmt.query_map(params![session_id, previous.id], |row| {
        Ok((ConversationCursor { id: row.get(0)?, created_at: row.get(1)? },
            row.get::<_, String>(2)?, row.get::<_, String>(3)?))
    }).map_err(|e| e.to_string())?;
    // 先完整读取成功，再推进内存游标，查询错误不能造成半导入。
    let rows = rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    let mut imported = 0;
    for (position, role, content) in rows {
        if role == "user" && !context.acknowledged_chat_ids.contains(&position.id) {
            append_live_goal(context, &content);
            context.turns_history.push(ChatMessage { role, content: json!(content) });
            imported += 1;
        }
        context.acknowledged_chat_ids.remove(&position.id);
        context.conversation_cursor = Some(position);
    }
    Ok(imported)
}

pub(crate) fn append_live_goal(context: &mut crate::agent::context::SandwichContext, message: &str) {
    // user_goal 是冻结的前缀锚点；仅更新当前目标，使后续审查与压缩也能看到补充。
    context.current_goal = format!("{}\n\n【本任务新增用户要求】\n{}", context.effective_goal(), message);
}

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
) -> Result<ConversationCursor, String> {
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    let now = Utc::now().to_rfc3339();
    let id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO chat_messages (id, session_id, role, content, attachments, created_at, model_id)
         VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6)",
        params![id, session_id, role, content, now, model_id],
    )
    .map_err(|error| error.to_string())?;
    conn.execute(
        "UPDATE chat_sessions SET updated_at=?1, message_count=message_count+1 WHERE id=?2",
        params![now, session_id],
    )
    .map_err(|error| error.to_string())?;
    Ok(ConversationCursor {
        created_at: now,
        id,
    })
}

/// 任务意图独立于模型配置；缺省保持旧客户端的非终态恢复行为。
#[derive(Debug, Clone, Copy, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStartMode {
    Resume,
    New,
}

pub fn should_start_new_turn(
    existing: Option<&AgentCheckpoint>,
    task_start_mode: Option<TaskStartMode>,
) -> Result<bool, String> {
    match task_start_mode {
        Some(TaskStartMode::New) => Ok(true),
        Some(TaskStartMode::Resume) => match existing {
            Some(cp) if cp.phase != crate::agent::checkpoint::AgentPhase::Done => Ok(false),
            Some(_) => Err("上一任务已完成；请选择新任务，不能恢复已交付任务。".to_string()),
            None => Err("没有可恢复的任务检查点；请选择新任务。".to_string()),
        },
        None => Ok(!matches!(existing, Some(cp) if !cp.phase.is_terminal())),
    }
}

pub fn begin_turn(
    db: &DbState,
    session_id: &str,
    model_id: &str,
    user_message: &str,
) -> Result<(), String> {
    ensure_conversation(db, session_id, model_id, user_message)?;
    append_message(db, session_id, "user", user_message, Some(model_id)).map(|_| ())
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
    .map(|_| ())
}

pub fn append_human_intervention(
    db: &DbState,
    session_id: &str,
    message: &str,
) -> Result<ConversationCursor, String> {
    append_message(db, session_id, "user", message, None)
}

#[cfg(test)]
fn recent_history(db: &DbState, session_id: &str) -> Result<Vec<ChatMessage>, String> {
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT role, content
             FROM chat_messages
             WHERE session_id=?1
             ORDER BY created_at ASC, id ASC",
        )
        .map_err(|error| error.to_string())?;
    let rows = stmt
        .query_map(params![session_id], |row| {
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
#[path = "conversation_history_tests.rs"]
mod history_tests;

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
        assert!(!should_start_new_turn(Some(&unfinished), None).unwrap());
        unfinished.mark_done(&context);
        assert!(should_start_new_turn(Some(&unfinished), None).unwrap());
        assert!(should_start_new_turn(None, None).unwrap());
    }

    #[test]
    fn failed_task_requires_structured_resume_intent() {
        let ctx = crate::agent::context::SandwichContext::new("system".into(), "原复杂任务".into());
        let mut cp = AgentCheckpoint::new("session".into(), &ctx);
        cp.mark_failed("用户取消".into(), &ctx);
        assert!(
            should_start_new_turn(Some(&cp), None).unwrap(),
            "旧客户端保留旧行为"
        );
        assert!(!should_start_new_turn(Some(&cp), Some(TaskStartMode::Resume)).unwrap());
        assert!(should_start_new_turn(Some(&cp), Some(TaskStartMode::New)).unwrap());
    }

    #[test]
    fn explicit_new_task_can_replace_unfinished_task() {
        let ctx = crate::agent::context::SandwichContext::new("system".into(), "原任务".into());
        let cp = AgentCheckpoint::new("session".into(), &ctx);
        assert!(!should_start_new_turn(Some(&cp), None).unwrap());
        assert!(should_start_new_turn(Some(&cp), Some(TaskStartMode::New)).unwrap());
    }

    #[test]
    fn resume_never_revives_completed_or_missing_task() {
        let ctx = crate::agent::context::SandwichContext::new("system".into(), "目标".into());
        let mut cp = AgentCheckpoint::new("session".into(), &ctx);
        cp.mark_done(&ctx);
        assert!(should_start_new_turn(Some(&cp), Some(TaskStartMode::Resume)).is_err());
        assert!(should_start_new_turn(None, Some(TaskStartMode::Resume)).is_err());
        assert!(serde_json::from_str::<TaskStartMode>("\"continue\"").is_err());
    }
}
