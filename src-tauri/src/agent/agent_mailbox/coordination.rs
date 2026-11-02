//! Event-driven inbox handoff. Short transactions claim work; execution runs outside
//! the lock. Original message ownership and original terminal deliveries never change.
use super::replies::Ticket;
use super::*;

const PURPOSE: &str = "readonly_inbox";

/// A registered message counts before an executor is started. Failed/cancelled
/// receivers remain visible, but do not become an obligation to restart them.
pub(super) fn pending_on(conn: &Connection, root: &str) -> Result<bool, String> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM agent_messages m JOIN agent_child_runs c ON c.id=COALESCE(m.claimed_run_id,m.target_run_id) WHERE m.root_run_id=?1 AND m.sender_kind IN ('agent_material','peer_answer') AND m.delivered_at IS NULL AND c.status IN ('queued','running','succeeded'))",
        [root], |r| r.get(0)).map_err(|e|e.to_string())
}

/// Admission, claim and terminal submission share the database serialization
/// boundary. Concurrent kicks can never claim the same message twice.
fn claim(db: &DbState) -> Result<Vec<Ticket>, String> {
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let candidates = {
        let mut stmt = tx.prepare("SELECT m.target_run_id,m.target_session_id,m.root_run_id,r.session_id,c.model_id,c.workspace_root,COALESCE(c.result_json,'') FROM agent_messages m JOIN agent_child_runs c ON c.id=m.target_run_id JOIN agent_run_traces r ON r.id=m.root_run_id WHERE m.sender_kind IN ('agent_material','peer_answer') AND m.delivered_at IS NULL AND m.claimed_run_id IS NULL AND c.status='succeeded' AND r.status IN ('running','waiting_human') GROUP BY m.target_run_id,m.root_run_id ORDER BY MIN(m.rowid)").map_err(|e|e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };
    let mut tickets = Vec::new();
    for (target, session, root_run, root_session, model, workspace, prior) in candidates {
        // A currently active continuation can take additional mail at its next
        // safe boundary; a late letter after that execution ends gets a new run.
        let active: Option<String> = tx.query_row("SELECT id FROM agent_child_runs WHERE resumed_from=?1 AND purpose=?2 AND status IN ('queued','running') LIMIT 1",params![target,PURPOSE],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
        let run = if let Some(id) = active {
            id
        } else {
            let (total, workers):(i64,i64)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(kind='worker'),0) FROM agent_child_runs WHERE parent_run_id=?1 AND status IN ('queued','running')",[&root_run],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|e|e.to_string())?;
            if total >= i64::from(crate::agent::child_reviewer::MAX_TOTAL_CHILDREN_PER_RUN)
                || workers >= i64::from(crate::agent::child_reviewer::MAX_CHILDREN_PER_KIND_PER_RUN)
            {
                continue;
            }
            let id = uuid::Uuid::new_v4().to_string();
            tx.execute("INSERT INTO agent_child_runs(id,parent_run_id,parent_session_id,workspace_root,model_id,status,started_at,kind,resumed_from,task_id,attempt_id,purpose,execution_config_json) VALUES (?1,?2,?3,?4,?5,'queued',?6,'worker',?7,?8,1,?9,(SELECT execution_config_json FROM agent_child_runs WHERE id=?7))",params![id,root_run,root_session,workspace,model,chrono::Utc::now().to_rfc3339(),target,format!("inbox:{id}"),PURPOSE]).map_err(|e|e.to_string())?;
            tickets.push(Ticket {
                request: format!("inbox:{id}"),
                run: id.clone(),
                original: target.clone(),
                responsible: root_run.clone(),
                root: Actor::new(&root_run, &root_session),
                requester: Actor::new(&target, &session),
                model,
                workspace: workspace.into(),
                prior,
                inbox: true,
            });
            id
        };
        tx.execute("UPDATE agent_messages SET claimed_run_id=?1 WHERE target_run_id=?2 AND root_run_id=?3 AND sender_kind IN ('agent_material','peer_answer') AND delivered_at IS NULL AND claimed_run_id IS NULL",params![run,target,root_run]).map_err(|e|e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(tickets)
}

pub(crate) fn kick(db: &DbState) {
    // Synchronous utilities/tests can operate without a Tokio executor. They do
    // not claim work that they cannot start.
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    match claim(db) {
        Ok(tickets) => {
            for ticket in tickets {
                super::replies::launch(db, ticket, None);
            }
        }
        Err(error) => eprintln!("[agent-inbox] 登记保留，接续未启动：{error}"),
    }
}

/// Query projection only: execution, receipt delivery and root collection remain
/// separate facts. Pagination never changes acknowledgement state.
pub(crate) fn list(db: &DbState, actor: &Actor, offset: u64) -> Result<Value, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let owner = root(&conn, actor)?;
    let mut stmt=conn.prepare("SELECT m.id,m.sender_run_id,m.target_run_id,m.claimed_run_id,m.created_at,m.delivered_at,c.status FROM agent_messages m LEFT JOIN agent_child_runs c ON c.id=COALESCE(m.claimed_run_id,m.target_run_id) WHERE m.root_run_id=?1 AND m.sender_kind IN ('agent_material','peer_answer') ORDER BY m.rowid LIMIT 101 OFFSET ?2").map_err(|e|e.to_string())?;
    let rows=stmt.query_map(params![owner.run,offset.min(i64::MAX as u64)],|r|Ok(json!({"message_id":r.get::<_,String>(0)?,"sender_run_id":r.get::<_,String>(1)?,"recipient_run_id":r.get::<_,String>(2)?,"execution_run_id":r.get::<_,Option<String>>(3)?,"created_at":r.get::<_,String>(4)?,"received_at":r.get::<_,Option<String>>(5)?,"execution_status":r.get::<_,Option<String>>(6)?}))).map_err(|e|e.to_string())?;
    let mut items = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let has_more = items.len() > 100;
    items.truncate(100);
    for item in &mut items {
        item["delivery_status"] = json!(if !item["received_at"].is_null() {
            "in_context"
        } else {
            match item["execution_status"].as_str() {
                Some("queued") => "queued",
                Some("running") => "running",
                Some("succeeded") if item["execution_run_id"].is_null() => "pending_start",
                Some("succeeded") => "awaiting_receipt",
                _ => "not_received",
            }
        });
    }
    Ok(
        json!({"items":items,"offset":offset,"next_offset":offset.saturating_add(items.len() as u64),"has_more":has_more}),
    )
}

#[cfg(test)]
mod tests;
