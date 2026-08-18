use crate::db::DbState;
use chrono::Utc;
use rusqlite::params;
use serde_json::{json, Value};

pub fn upsert_snapshot(
    db: &DbState,
    run_id: &str,
    session_id: &str,
    goal: &str,
    status: &str,
    steps: &[Value],
    completion: Option<&Value>,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    let now = Utc::now().to_rfc3339();
    let steps_json = serde_json::to_string(steps).map_err(|error| error.to_string())?;
    let completion_json = completion
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| error.to_string())?;
    conn.execute(
        "INSERT INTO agent_run_traces (id, session_id, goal, status, steps_json, completion_json, started_at, updated_at, completed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, CASE WHEN ?4 IN ('succeeded', 'failed', 'cancelled') THEN ?7 ELSE NULL END)
         ON CONFLICT(id) DO UPDATE SET
            status=excluded.status,
            steps_json=excluded.steps_json,
            completion_json=COALESCE(excluded.completion_json, agent_run_traces.completion_json),
            updated_at=excluded.updated_at,
            completed_at=CASE WHEN excluded.status IN ('succeeded', 'failed', 'cancelled') THEN excluded.updated_at ELSE agent_run_traces.completed_at END",
        params![run_id, session_id, goal, status, steps_json, completion_json, now],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

pub fn list_session_traces(db: &DbState, session_id: &str) -> Result<Vec<Value>, String> {
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    let mut statement = conn
        .prepare(
            "SELECT id, goal, status, steps_json, completion_json, started_at, updated_at, completed_at
             FROM agent_run_traces WHERE session_id=?1 ORDER BY started_at ASC, id ASC",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![session_id], |row| {
            let steps_json: String = row.get(3)?;
            let completion_json: Option<String> = row.get(4)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                steps_json,
                completion_json,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    rows.map(|row| {
        let (id, goal, status, steps_json, completion_json, started_at, updated_at, completed_at) =
            row.map_err(|error| error.to_string())?;
        let steps = serde_json::from_str::<Value>(&steps_json).unwrap_or_else(|_| json!([]));
        let completion = completion_json
            .as_deref()
            .and_then(|value| serde_json::from_str::<Value>(value).ok())
            .unwrap_or(Value::Null);
        Ok(json!({
            "id": id,
            "goal": goal,
            "status": status,
            "steps": steps,
            "completion": completion,
            "started_at": started_at,
            "updated_at": updated_at,
            "completed_at": completed_at,
        }))
    })
    .collect()
}

/// 超过该字符阈值（约 2,000 字符 / ~500 Tokens）时自动溢出落盘到本地独立文件
pub const DISK_SPILL_THRESHOLD_CHARS: usize = 2_000;

pub fn get_outputs_dir() -> std::path::PathBuf {
    dirs_next::data_dir()
        .map(|d| d.join("free-api-chat").join("outputs"))
        .unwrap_or_else(|| std::path::PathBuf::from("./data/outputs"))
}

pub fn save_step_output_with_meta(
    db: &DbState,
    session_id: &str,
    step_id: usize,
    tool: &str,
    action: &str,
    outcome: &str,
    raw_output: &str,
) -> Result<String, String> {
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    let output_id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    let sqlite_content = if raw_output.chars().count() > DISK_SPILL_THRESHOLD_CHARS {
        let outputs_dir = get_outputs_dir();
        let _ = std::fs::create_dir_all(&outputs_dir);
        let log_file = outputs_dir.join(format!("{}.log", output_id));
        if std::fs::write(&log_file, raw_output).is_ok() {
            format!("[[spilled_to_disk:{}]]", log_file.display())
        } else {
            raw_output.to_string()
        }
    } else {
        raw_output.to_string()
    };

    conn.execute(
        "INSERT INTO agent_step_logs (output_id, session_id, step_id, raw_output, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![output_id, session_id, step_id, sqlite_content, now],
    )
    .map_err(|error| error.to_string())?;

    let _ = crate::agent::retrieval::index_step_fts(
        &conn,
        &output_id,
        session_id,
        step_id,
        tool,
        action,
        outcome,
        raw_output,
    );

    // 写入三级记忆模型的 Level 0 原始事件节点 (带语义关键词)
    let node = crate::agent::retrieval::MemoryNode {
        id: format!("step-{}", step_id),
        parent_id: None,
        level: 0,
        kind: "event".to_string(),
        session_id: session_id.to_string(),
        plan_id: None,
        step_index: Some(step_id),
        content: format!("工具: {} | 动作: {} | 状态: {}\n输出: {}", tool, action, outcome, raw_output.chars().take(500).collect::<String>()),
        metadata_json: serde_json::json!({
            "output_id": output_id,
            "tool": tool,
            "action": action,
            "outcome": outcome,
        }).to_string(),
        confidence_score: 100,
        hit_count: 0,
        created_at: now,
    };
    let trigger_keywords = format!("{}, {}, {}", tool, action, outcome);
    let _ = crate::agent::retrieval::insert_memory_node(&conn, &node, &trigger_keywords);

    Ok(output_id)
}

pub fn get_step_output(
    db: &DbState,
    output_id: &str,
) -> Result<String, String> {
    // 先尝试从磁盘文件读（直接命中，最快）
    let log_file = get_outputs_dir().join(format!("{}.log", output_id));
    if log_file.exists() {
        if let Ok(content) = std::fs::read_to_string(&log_file) {
            return Ok(content);
        }
    }

    // 从 SQLite 读取
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    let mut stmt = conn
        .prepare("SELECT raw_output FROM agent_step_logs WHERE output_id=?1")
        .map_err(|error| error.to_string())?;
    let mut rows = stmt.query(params![output_id]).map_err(|error| error.to_string())?;
    if let Some(row) = rows.next().map_err(|error| error.to_string())? {
        let raw: String = row.get(0).map_err(|error| error.to_string())?;

        // 处理溢出标记：[[spilled_to_disk:/path/to/file.log]]
        if let Some(path_str) = raw.strip_prefix("[[spilled_to_disk:").and_then(|s| s.strip_suffix("]]")) {
            return Ok(std::fs::read_to_string(path_str).unwrap_or_else(|_| {
                format!(
                    "[⚠️ 原始日志落盘文件已缺失或已移动 (output_id: {}, path: {})]",
                    output_id, path_str
                )
            }));
        }

        Ok(raw)
    } else {
        Err(format!("output_id {} not found", output_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn test_db() -> DbState {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE agent_run_traces (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                goal TEXT NOT NULL,
                status TEXT NOT NULL,
                steps_json TEXT NOT NULL,
                completion_json TEXT,
                started_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                completed_at TEXT
            );",
            )
            .unwrap();
        DbState(Arc::new(Mutex::new(connection)))
    }

    #[test]
    fn saves_and_loads_steps_in_session_order() {
        let db = test_db();
        upsert_snapshot(
            &db,
            "run-a",
            "session-1",
            "创建成绩单",
            "running",
            &[json!({"id": 0, "description": "创建工作簿"})],
            None,
        )
        .unwrap();
        upsert_snapshot(
            &db,
            "run-a",
            "session-1",
            "创建成绩单",
            "succeeded",
            &[json!({"id": 0, "status": "succeeded"})],
            Some(&json!({"summary": "完成"})),
        )
        .unwrap();
        let traces = list_session_traces(&db, "session-1").unwrap();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0]["status"], "succeeded");
        assert_eq!(traces[0]["completion"]["summary"], "完成");
    }
}
