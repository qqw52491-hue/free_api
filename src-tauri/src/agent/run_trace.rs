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
        let (id, goal, status, steps_json, completion_json, started_at, updated_at, completed_at) = row.map_err(|error| error.to_string())?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn test_db() -> DbState {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(
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
        ).unwrap();
        DbState(Arc::new(Mutex::new(connection)))
    }

    #[test]
    fn saves_and_loads_steps_in_session_order() {
        let db = test_db();
        upsert_snapshot(&db, "run-a", "session-1", "创建成绩单", "running", &[json!({"id": 0, "description": "创建工作簿"})], None).unwrap();
        upsert_snapshot(&db, "run-a", "session-1", "创建成绩单", "succeeded", &[json!({"id": 0, "status": "succeeded"})], Some(&json!({"summary": "完成"}))).unwrap();
        let traces = list_session_traces(&db, "session-1").unwrap();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0]["status"], "succeeded");
        assert_eq!(traces[0]["completion"]["summary"], "完成");
    }
}
