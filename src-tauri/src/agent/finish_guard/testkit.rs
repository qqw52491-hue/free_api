//! finish_guard 测试共享夹具与导入。

    use super::*;
    use crate::agent::dispatcher::is_write_or_side_effect_tool;
    use crate::agent::review_contract::ReviewObligation;

    /// 测试夹具：直接向 agent_child_runs 写入/更新/清理子任务行，用于还原门禁的真实判定场景。
    pub(crate) fn insert_child_run(
        db: &DbState,
        id: &str,
        parent_run_id: &str,
        status: &str,
        result_json: Option<&str>,
        consumed_at: Option<&str>,
    ) {
        let conn = db.0.lock().unwrap();
        let result = result_json
            .map(|r| format!("'{}'", r))
            .unwrap_or_else(|| "NULL".to_string());
        let consumed = consumed_at
            .map(|c| format!("'{}'", c))
            .unwrap_or_else(|| "NULL".to_string());
        conn.execute(
            &format!(
                "INSERT OR REPLACE INTO agent_child_runs
                 (id, parent_run_id, parent_session_id, workspace_root, model_id, status, started_at, kind, result_json, consumed_at, applied_at)
                 VALUES ('{id}', '{parent}', 'sess', '{ws}', 'model', '{status}', '2026-01-01T00:00:00Z', 'review', {result}, {consumed}, {consumed})",
                id = id,
                parent = parent_run_id,
                ws = std::env::temp_dir().display(),
                status = status,
                result = result,
                consumed = consumed,
            ),
            [],
        )
        .unwrap();
    }

    pub(crate) fn mark_child_run(db: &DbState, id: &str, status: &str) {
        let conn = db.0.lock().unwrap();
        conn.execute(
            &format!(
                "UPDATE agent_child_runs SET status = '{status}' WHERE id = '{id}'",
                status = status,
                id = id,
            ),
            [],
        )
        .unwrap();
    }

    pub(crate) fn delete_child_runs(db: &DbState, parent_run_id: &str) {
        let conn = db.0.lock().unwrap();
        conn.execute(
            &format!("DELETE FROM agent_child_runs WHERE parent_run_id = '{parent}'", parent = parent_run_id),
            [],
        )
        .unwrap();
    }

    pub(crate) fn todo(id: usize, status: &str, description: &str) -> TodoItem {
        TodoItem {
            id,
            status: status.to_string(),
            description: description.to_string(),
            evidence_output_id: None,
        }
    }
