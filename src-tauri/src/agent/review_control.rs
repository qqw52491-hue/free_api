//! 人工停止单个审查请求；模型排除按现有任务身份持久化，不改写审查意见。
use crate::db::DbState;
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::watch;

pub(crate) const STOP_NOTE: &str = "用户已停止该审查，本任务后续审查排除此模型；取消不代表批准，已有有效意见保留";

#[derive(Clone)]
struct ActiveReview {
    session: String,
    task: String,
    model: String,
    child: bool,
    interrupt: watch::Sender<u64>,
}
fn active() -> &'static Mutex<HashMap<String, ActiveReview>> {
    static ACTIVE: OnceLock<Mutex<HashMap<String, ActiveReview>>> = OnceLock::new();
    ACTIVE.get_or_init(Default::default)
}

fn scope_on(conn: &rusqlite::Connection, session: &str) -> Result<(String, BTreeSet<String>), String> {
    let raw: Option<String> = conn.query_row(
        "SELECT snapshot_json FROM agent_checkpoints WHERE run_id=?1", [session], |r| r.get(0),
    ).optional().map_err(|e| e.to_string())?;
    let snapshot: Value = raw.map(|s| serde_json::from_str(&s)).transpose().map_err(|e| e.to_string())?.unwrap_or(Value::Null);
    Ok((snapshot["data"]["history_task_id"].as_str().unwrap_or_default().into(),
        snapshot["excluded_review_models"].as_array().into_iter().flatten()
            .filter_map(|v| v.as_str().map(str::to_owned)).collect()))
}

pub(crate) fn excluded_models(db: &DbState, session: &str) -> Result<BTreeSet<String>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    Ok(scope_on(&conn, session)?.1)
}
pub(crate) fn ensure_allowed(db: &DbState, session: &str, model: &str) -> Result<(), String> {
    if excluded_models(db, session)?.contains(model) {
        Err(format!("审查模型 {model} 已由用户在当前任务中排除，未发起请求；可使用其他未排除的审查员。"))
    } else { Ok(()) }
}

/// 普通保存与终态事务均须在持有连接期间合并同任务排除，再写回快照。
/// 排除是独立的增量控制字段，不推进执行版本，不能靠版本冲突强迫主循环重载。
pub(crate) fn merge_exclusions_on(conn: &rusqlite::Connection, cp: &mut crate::agent::checkpoint::AgentCheckpoint) -> Result<(), String> {
    let (task, excluded) = scope_on(conn, &cp.run_id)?;
    if task == cp.data.history_task_id { cp.excluded_review_models.extend(excluded); }
    Ok(())
}

pub(crate) struct ReviewRegistration {
    id: String,
    rx: watch::Receiver<u64>,
    forwarder: tokio::task::JoinHandle<()>,
}
impl ReviewRegistration {
    pub(crate) fn interrupt(&self) -> watch::Receiver<u64> { self.rx.clone() }
    /// 与人工停止串行核销：完成后旧 ID 不再可停；先停止则不能随后发布批准。
    pub(crate) fn complete(&self) -> bool {
        let Ok(mut entries) = active().lock() else { return false; };
        let unchanged = !self.rx.has_changed().unwrap_or(true);
        entries.remove(&self.id);
        unchanged
    }
}
impl Drop for ReviewRegistration {
    fn drop(&mut self) {
        if let Ok(mut entries) = active().lock() { entries.remove(&self.id); }
        self.forwarder.abort();
    }
}

pub(crate) fn register(db: &DbState, session: &str, id: &str, model: &str, child: bool, mut parent: watch::Receiver<u64>) -> Result<ReviewRegistration, String> {
    // 与停止入口保持相同锁顺序，关闭“检查未排除→停止→登记新请求”的竞争窗口。
    let mut entries = active().lock().map_err(|e| e.to_string())?;
    let (task, excluded) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        scope_on(&conn, session)?
    };
    if excluded.contains(model) { return Err(STOP_NOTE.into()); }
    let (sender, rx) = watch::channel(*parent.borrow());
    if parent.has_changed().unwrap_or(false) { sender.send_modify(|g| *g = g.wrapping_add(1)); }
    entries.insert(id.into(), ActiveReview { session: session.into(), task, model: model.into(), child, interrupt: sender.clone() });
    let forwarder = tokio::spawn(async move {
        loop {
            if parent.changed().await.is_err() { break; }
            sender.send_modify(|generation| *generation = generation.wrapping_add(1));
        }
    });
    Ok(ReviewRegistration { id: id.into(), rx, forwarder })
}

pub(crate) fn cancel_child_on(conn: &rusqlite::Connection, id: &str) -> Result<bool, String> {
    let now = chrono::Utc::now();
    let started: Option<String> = conn.query_row("SELECT started_at FROM agent_child_runs WHERE id=?1 AND status='running'", [id], |r| r.get(0))
        .optional().map_err(|e| e.to_string())?;
    let Some(started) = started else { return Ok(false); };
    let duration = chrono::DateTime::parse_from_rfc3339(&started).ok()
        .map(|start| now.signed_duration_since(start).num_milliseconds().max(0) as u64).unwrap_or(0);
    let result = crate::agent::child_reviewer::child_error_result_json(
        id, "cancelled", &crate::agent::types::ChildErrorKind::Cancelled, duration, 0, 0, Some(STOP_NOTE));
    let mut result: Value = serde_json::from_str(&result).map_err(|e| e.to_string())?;
    result["usage_unknown"] = json!(true);
    let changed = conn.execute(
        "UPDATE agent_child_runs SET status='cancelled',finished_at=?2,error_kind='cancelled',result_json=?3,duration_ms=?4 WHERE id=?1 AND status='running'",
        rusqlite::params![id, now.to_rfc3339(), result.to_string(), duration],
    ).map_err(|e| e.to_string())?;
    Ok(changed > 0)
}

#[derive(serde::Serialize)]
pub struct StopReviewOutcome { model_id: String, stopped: bool }

fn stop_member(db: &DbState, session: &str, id: &str) -> Result<StopReviewOutcome, String> {
    let entries = active().lock().map_err(|e| e.to_string())?;
    let member = entries.get(id).filter(|m| m.session == session)
        .ok_or("该审查已结束或不属于当前会话，未停止其他任务")?;
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let (task, mut excluded) = scope_on(&tx, session)?;
    if task.is_empty() || task != member.task { return Err("任务身份已变化，未修改新任务的审查配置".into()); }
    excluded.insert(member.model.clone());
    tx.execute(
        "UPDATE agent_checkpoints SET snapshot_json=json_set(snapshot_json,'$.excluded_review_models',json(?2)) WHERE run_id=?1",
        rusqlite::params![session, serde_json::to_string(&excluded).map_err(|e| e.to_string())?],
    ).map_err(|e| e.to_string())?;
    let stopped = if member.child { cancel_child_on(&tx, id)? } else { true };
    tx.commit().map_err(|e| e.to_string())?;
    if stopped { member.interrupt.send_modify(|g| *g = g.wrapping_add(1)); }
    crate::agent::job_engine::notify_job_state_changed();
    Ok(StopReviewOutcome { model_id: member.model.clone(), stopped })
}

#[tauri::command]
pub async fn stop_review_member(app: AppHandle, state: State<'_, DbState>, session_id: String, review_id: String) -> Result<StopReviewOutcome, String> {
    let outcome = stop_member(state.inner(), &session_id, &review_id)?;
    let note = if outcome.stopped { STOP_NOTE } else { "该审查已完成，原结论保留；当前任务后续排除此审查模型" };
    let _ = app.emit(&format!("agent-progress-{session_id}"), json!({
        "type":"review_member_stopped", "review_id":review_id, "model_id":outcome.model_id, "stopped":outcome.stopped, "description":note
    }));
    let _ = app.emit(&format!("agent-log-{session_id}"), note);
    Ok(outcome)
}

#[cfg(test)]
mod tests;
