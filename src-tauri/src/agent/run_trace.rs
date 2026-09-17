use crate::db::DbState;
use chrono::Utc;
use rusqlite::{params, OptionalExtension};
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
            steps_json=CASE WHEN excluded.steps_json = '[]' AND agent_run_traces.steps_json != '[]' AND agent_run_traces.steps_json IS NOT NULL THEN agent_run_traces.steps_json ELSE excluded.steps_json END,
            completion_json=COALESCE(excluded.completion_json, agent_run_traces.completion_json),
            updated_at=excluded.updated_at,
            completed_at=CASE WHEN excluded.status IN ('succeeded', 'failed', 'cancelled') THEN excluded.updated_at ELSE agent_run_traces.completed_at END",
        params![run_id, session_id, goal, status, steps_json, completion_json, now],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

/// 逐步追加：单步 INSERT，替代每步整写 steps_json 的 O(n) 序列化+整行重写。
/// seq 取 steps 数组下标（追加顺序单调递增）；同时轻触父行 updated_at 供监控排序。
pub fn append_step(
    db: &DbState,
    run_id: &str,
    seq: i64,
    step: &Value,
) -> Result<(), String> {
    let mut conn = db.0.lock().map_err(|error| error.to_string())?;
    let step_json = serde_json::to_string(step).map_err(|error| error.to_string())?;
    let now = Utc::now().to_rfc3339();
    let tx = conn.transaction().map_err(|error| error.to_string())?;
    tx.execute(
        "INSERT INTO agent_run_steps (run_id, seq, step_json) VALUES (?1, ?2, ?3)
         ON CONFLICT(run_id, seq) DO UPDATE SET step_json=excluded.step_json",
        params![run_id, seq, step_json],
    )
    .map_err(|error| error.to_string())?;
    tx.execute(
        "UPDATE agent_run_traces SET updated_at=?2 WHERE id=?1",
        params![run_id, now],
    )
    .map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?;
    Ok(())
}

/// 行优先读取：agent_run_steps 有数据时按 seq 顺序重组（增量路径）；
/// 为空时回退 steps_json 列（旧快照/测试直接播种/终态整写路径）。
pub fn load_run_steps(db: &DbState, run_id: &str) -> Result<Vec<Value>, String> {
    let conn = db.reader()?;
    let mut statement = conn
        .prepare("SELECT step_json FROM agent_run_steps WHERE run_id=?1 ORDER BY seq ASC")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![run_id], |r| r.get::<_, String>(0))
        .map_err(|error| error.to_string())?;
    let mut steps: Vec<Value> = Vec::new();
    for row in rows {
        let raw = row.map_err(|error| error.to_string())?;
        steps.push(serde_json::from_str::<Value>(&raw).unwrap_or(Value::Null));
    }
    drop(statement);
    if !steps.is_empty() {
        return Ok(steps);
    }
    let row: Option<String> = conn
        .query_row(
            "SELECT steps_json FROM agent_run_traces WHERE id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    if let Some(json_str) = row {
        let val: Value = serde_json::from_str(&json_str).unwrap_or_else(|_| json!([]));
        if let Some(arr) = val.as_array() {
            return Ok(arr.clone());
        }
    }
    Ok(Vec::new())
}

pub fn list_session_traces(db: &DbState, session_id: &str) -> Result<Vec<Value>, String> {
    let conn = db.reader()?;
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
        // 行优先：增量持久化后 steps_json 只在启动/终态整写
        let row_steps: Vec<Value> = conn
            .prepare("SELECT step_json FROM agent_run_steps WHERE run_id=?1 ORDER BY seq ASC")
            .and_then(|mut stmt| {
                stmt.query_map(params![id], |r| r.get::<_, String>(0))
                    .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
            })
            .unwrap_or_default()
            .into_iter()
            .filter_map(|raw| serde_json::from_str::<Value>(&raw).ok())
            .collect();
        let steps = if !row_steps.is_empty() {
            Value::Array(row_steps)
        } else {
            serde_json::from_str::<Value>(&steps_json).unwrap_or_else(|_| json!([]))
        };
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

/// 落库到步骤快照的 thinking 上限：reasoning 全文仅作取证留痕，超长截断保护快照体积。
pub(crate) const MAX_TRACE_THINKING_CHARS: usize = 64_000;

pub(crate) fn clip_thinking_for_trace(text: &str) -> String {
    text.chars().take(MAX_TRACE_THINKING_CHARS).collect()
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
        &conn, &output_id, session_id, step_id, tool, action, outcome, raw_output,
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
        content: format!(
            "工具: {} | 动作: {} | 状态: {}\n输出: {}",
            tool,
            action,
            outcome,
            raw_output.chars().take(500).collect::<String>()
        ),
        metadata_json: serde_json::json!({
            "output_id": output_id,
            "tool": tool,
            "action": action,
            "outcome": outcome,
        })
        .to_string(),
        confidence_score: 100,
        hit_count: 0,
        created_at: now,
    };
    let trigger_keywords = format!("{}, {}, {}", tool, action, outcome);
    let _ = crate::agent::retrieval::insert_memory_node(&conn, &node, &trigger_keywords);

    Ok(output_id)
}

pub fn get_step_output(db: &DbState, session_id: &str, output_id: &str) -> Result<String, String> {
    // 先通过 SQLite 校验 output_id 归属，再读取可能的溢出文件，避免跨 session 工件泄露。
    let raw: Option<String> = {
        let conn = db.0.lock().map_err(|error| error.to_string())?;
        conn.query_row(
            "SELECT raw_output FROM agent_step_logs WHERE output_id=?1 AND session_id=?2",
            params![output_id, session_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?
    };

    let raw = raw.ok_or_else(|| "当前 session 中未找到该 output_id".to_string())?;
    if let Some(path_str) = raw
        .strip_prefix("[[spilled_to_disk:")
        .and_then(|value| value.strip_suffix("]]"))
    {
        return std::fs::read_to_string(path_str).map_err(|error| {
            format!(
                "工件原文不可用 (output_id: {}, path: {}): {}",
                output_id, path_str, error
            )
        });
    }

    Ok(raw)
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
    fn clip_thinking_for_trace_truncates_at_cap() {
        let short = "先分析根因再动手";
        assert_eq!(clip_thinking_for_trace(short), short);
        let long = "思".repeat(MAX_TRACE_THINKING_CHARS + 500);
        let clipped = clip_thinking_for_trace(&long);
        assert_eq!(clipped.chars().count(), MAX_TRACE_THINKING_CHARS);
        // thinking 字段随步骤 JSON 原样往返（快照→行表→回放）
        let db = test_db();
        db.0.lock()
            .unwrap()
            .execute_batch(
                "CREATE TABLE agent_run_steps (
                    run_id TEXT NOT NULL, seq INTEGER NOT NULL, step_json TEXT NOT NULL,
                    PRIMARY KEY (run_id, seq));",
            )
            .unwrap();
        append_step(
            &db,
            "run-thinking",
            0,
            &json!({"id": 0, "thought": "简述", "thinking": clipped}),
        )
        .unwrap();
        let steps = load_run_steps(&db, "run-a-missing").unwrap_or_default();
        assert!(steps.is_empty());
        let loaded = load_run_steps(&db, "run-thinking").unwrap();
        assert_eq!(
            loaded[0]["thinking"].as_str().unwrap().chars().count(),
            MAX_TRACE_THINKING_CHARS
        );
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
