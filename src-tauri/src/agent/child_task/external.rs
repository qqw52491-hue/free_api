//! External Codex hands back findings through the ordinary child receipt ledger.
use super::TaskBinding;
use crate::{agent::external_agents::PURPOSE, db::DbState};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub(crate) struct Request {
    pub command: PathBuf,
    pub workspace: PathBuf,
    pub prompt: String,
}

pub(crate) fn register(
    db: &DbState,
    parent_session: &str,
    parent_run: &str,
    workspace: &Path,
    key: String,
    dispatch: &Value,
    metadata: &Value,
) -> Result<TaskBinding, String> {
    use crate::agent::child_reviewer::{
        begin_child_run_extended, ChildRunIdentity, MAX_TOTAL_CHILDREN_PER_RUN,
    };
    let id = begin_child_run_extended(
        db,
        parent_run,
        parent_session,
        workspace,
        "",
        0,
        "task",
        Some(i64::from(MAX_TOTAL_CHILDREN_PER_RUN)),
        None,
        None,
        &ChildRunIdentity {
            purpose: Some(PURPOSE.into()),
            dispatch_input: Some(json!({"params":dispatch,"external_agent":metadata}).to_string()),
            task_id: key,
            ..ChildRunIdentity::fresh("task")
        },
    )?;
    Ok(TaskBinding {
        parent_session: parent_session.into(),
        parent_run: parent_run.into(),
        child_session: format!("task-{id}"),
        child_run: id,
    })
}

pub(crate) fn is_external(conn: &Connection, id: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_child_runs WHERE id=?1 AND kind='task' AND purpose=?2)",
        params![id, PURPOSE],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}

pub(crate) fn store_result(
    db: &DbState,
    binding: &TaskBinding,
    outcome: Result<String, String>,
    cancelled: bool,
    duration_ms: u64,
) -> Result<(), String> {
    let (status, body) = match outcome {
        Ok(output) => {
            let summary = output.trim().chars().take(500).collect::<String>();
            (
                "succeeded",
                json!({"handoff_accepted":true,"delivery":{"summary":summary,"output":output}}),
            )
        }
        Err(error) => (
            if cancelled { "cancelled" } else { "failed" },
            json!({"handoff_accepted":false,"delivery":{"summary":error},"host_error":error}),
        ),
    };
    let conn = db.writer()?;
    conn.execute(
        "UPDATE agent_child_runs SET status=?3,result_json=?4,finished_at=?5,duration_ms=?6,consumed_at=?5
         WHERE id=?1 AND parent_session_id=?2 AND kind='task' AND status='running' AND purpose=?7",
        params![binding.child_run, binding.parent_session, status, body.to_string(), chrono::Utc::now().to_rfc3339(), duration_ms, PURPOSE],
    ).map_err(|e| e.to_string())?;
    drop(conn);
    crate::agent::job_engine::notify_job_state_changed();
    Ok(())
}

pub(crate) const INTERRUPTED: &str = "本地 Codex 执行已中断。原结果未知，宿主没有自动重新派发。";

pub(crate) fn interrupt_orphans(conn: &Connection) -> rusqlite::Result<usize> {
    let notice = json!({"handoff_accepted":false,"delivery":{"summary":INTERRUPTED},"host_error":INTERRUPTED});
    conn.execute(
        "UPDATE agent_child_runs SET status='cancelled',result_json=?2,finished_at=?3,consumed_at=?3
         WHERE kind='task' AND purpose=?1 AND status='running'",
        params![PURPOSE, notice.to_string(), chrono::Utc::now().to_rfc3339()],
    )
}

pub(crate) fn collect(
    db: &DbState,
    session: &str,
    id: &str,
    checkpoint: &mut crate::agent::checkpoint::AgentCheckpoint,
    context: &mut crate::agent::context::SandwichContext,
) -> Result<bool, String> {
    let collected = db.writer()?.query_row(
        "SELECT NULLIF(applied_at,'') FROM agent_child_runs WHERE id=?1 AND parent_session_id=?2 AND purpose=?3 AND kind='task' AND result_json IS NOT NULL",
        params![id, session, PURPOSE], |r| r.get::<_, Option<String>>(0),
    ).optional().map_err(|e| e.to_string())?;
    let Some(collected) = collected else {
        return Ok(false);
    };
    if collected.is_none() {
        let body = super::result(db, session, id)?;
        let mut candidate = context.clone();
        candidate.add_host_feedback(&format!(
            "【外部 Agent 交回 {id}】{}",
            super::projection(&body)
        ));
        crate::agent::checkpoint::save_checkpoint_and_consume(
            db,
            checkpoint,
            &candidate,
            &[crate::agent::checkpoint::AppliedReceipt {
                run_id: id.into(),
                verdict: None,
            }],
        )?;
        *context = candidate;
    }
    Ok(true)
}

#[cfg(all(test, unix))]
mod tests;
