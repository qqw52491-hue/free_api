//! Desktop collaboration view. Result availability and collection are separate
//! durable facts. A wait belongs only to the lifetime of its executing future.
use crate::db::DbState;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WaitKind {
    Jobs,
    Inbox,
}

#[derive(Clone, Serialize)]
pub(super) struct ActiveWait {
    waiter_run_id: String,
    kind: WaitKind,
    target_ids: Vec<String>,
}

static WAITS: Mutex<Vec<Arc<ActiveWait>>> = Mutex::new(Vec::new());

pub(crate) struct WaitGuard(Arc<ActiveWait>);

impl WaitGuard {
    pub(crate) fn enter(run: &str, kind: WaitKind, target_ids: &[String]) -> Self {
        let wait = Arc::new(ActiveWait {
            waiter_run_id: run.into(),
            kind,
            target_ids: target_ids
                .iter()
                .filter(|id| id.as_str() != run && id.as_str() != "self")
                .cloned()
                .collect(),
        });
        WAITS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(wait.clone());
        Self(wait)
    }
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        WAITS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|wait| !Arc::ptr_eq(wait, &self.0));
    }
}

#[derive(Serialize)]
pub(super) struct ChildReceipt {
    child_run_id: String,
    parent_run_id: String,
    execution_status: String,
    result_available: bool,
    collected_at: Option<String>,
    finished_at: Option<String>,
}

#[derive(Serialize)]
pub(super) struct TeamView {
    pub(super) tasks: Vec<serde_json::Value>,
    receipts: Vec<ChildReceipt>,
    waiting_on: Vec<ActiveWait>,
    focus_run_id: Option<String>,
    focus_run_ids: Vec<String>,
}

/// Authorize every root by its owning session and follow the recorded run IDs.
/// Include dedicated runs here while leaving the delegated-task dock's list and
/// write actions limited to ordinary tasks.
pub(super) fn observe(db: &DbState, session: &str) -> Result<TeamView, String> {
    let active_root = crate::agent::control::active_execution_run_id(session)?;
    let conn = db.reader()?;
    let receipts = {
        let mut stmt = conn
            .prepare(
                "WITH RECURSIVE family(id) AS (
                 SELECT id FROM agent_child_runs WHERE parent_session_id=?1
                 UNION
                 SELECT c.id FROM agent_child_runs c JOIN family f ON c.parent_run_id=f.id
             )
             SELECT c.id, c.parent_run_id, c.status, c.result_json IS NOT NULL,
                    NULLIF(c.applied_at, ''), c.finished_at
             FROM family f JOIN agent_child_runs c ON c.id=f.id",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([session], |row| {
                Ok(ChildReceipt {
                    child_run_id: row.get(0)?,
                    parent_run_id: row.get(1)?,
                    execution_status: row.get(2)?,
                    result_available: row.get(3)?,
                    collected_at: row.get(4)?,
                    finished_at: row.get(5)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };

    let focus_run_id = match active_root.as_ref() {
        Some(run) => Some(run.clone()),
        None => conn.query_row(
            "SELECT id FROM agent_run_traces WHERE session_id=?1 ORDER BY started_at DESC,id DESC LIMIT 1",
            [session], |row| row.get(0),
        ).optional().map_err(|e| e.to_string())?,
    };
    let mut focus_run_ids = Vec::new();
    if let Some(focus) = focus_run_id.as_ref() {
        let mut stmt = conn.prepare(concat!(
            "SELECT id FROM agent_run_traces WHERE id<>?1 AND id IN (",
            crate::agent::run_trace::task_run_ids!("?1", "?2"),
            ") ORDER BY started_at,id",
        )).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![focus, session], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        focus_run_ids = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        focus_run_ids.push(focus.clone());
    }
    drop(conn);

    let member_ids: HashSet<&str> = receipts
        .iter()
        .map(|row| row.child_run_id.as_str())
        .collect();
    let waiting_on = WAITS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|wait| {
            active_root.as_deref() == Some(wait.waiter_run_id.as_str())
                || member_ids.contains(wait.waiter_run_id.as_str())
        })
        .map(|wait| wait.as_ref().clone())
        .collect();
    Ok(TeamView {
        tasks: Vec::new(),
        receipts,
        waiting_on,
        focus_run_id,
        focus_run_ids,
    })
}
