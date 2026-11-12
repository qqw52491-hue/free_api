//! 完整任务委派的宿主边界。执行仍由 main_loop 承担；这里仅维护身份、授权引用和交接。
//! 共享工作区允许并行，执行器独立运行；不复制主循环，不改变专用子 Agent 的结果契约。
pub(crate) mod runtime;
pub(crate) mod presets;
pub(crate) mod dispatch;
pub(crate) mod models;
pub(crate) mod defaults;
use crate::db::DbState;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

#[derive(Clone, Debug)]
pub(crate) struct TaskBinding {
    pub parent_session: String,
    pub parent_run: String,
    pub child_session: String,
    pub child_run: String,
}
static ACTIVE: LazyLock<Mutex<HashMap<String, TaskBinding>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub(crate) fn parent_session(child: &str) -> Option<String> {
    ACTIVE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(child)
        .map(|b| b.parent_session.clone())
}
/// Root is depth 0; five levels of ordinary delegated tasks are allowed.
pub(crate) const MAX_DELEGATION_DEPTH: usize = 5;
pub(crate) fn delegation_depth(session: &str) -> Result<usize, String> {
    let active = ACTIVE.lock().map_err(|e| e.to_string())?;
    let mut node = session.to_owned();
    let mut seen = std::collections::HashSet::new();
    let mut depth = 0;
    while let Some(binding) = active.get(&node) {
        if !seen.insert(node.clone()) { return Err("任务委派关系存在循环；未派发。".into()); }
        depth += 1;
        node = binding.parent_session.clone();
    }
    Ok(depth)
}
pub(crate) fn check_delegation_depth(session: &str) -> Result<usize, String> {
    let depth = delegation_depth(session)?;
    if depth >= MAX_DELEGATION_DEPTH {
        return Err(format!("当前普通任务位于第 {depth} 层，已达上限 {MAX_DELEGATION_DEPTH} 层；未派发。可自行完成或交回父任务。"));
    }
    Ok(depth)
}

pub(crate) fn is_delegating(parent: &str) -> bool {
    ACTIVE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .any(|b| b.parent_session == parent)
}

pub(crate) fn workspace_owner(session:&str,run:&str)->Result<(String,String),String> {
    let active=ACTIVE.lock().map_err(|e|e.to_string())?;
    let(mut session,mut run)=(session.to_string(),run.to_string());
    let mut seen=std::collections::BTreeSet::new();
    while let Some(binding)=active.get(&session) {
        if !seen.insert(session.clone()){return Err("工作区委派身份链存在循环，未取得项目锁。".into());}
        session=binding.parent_session.clone();run=binding.parent_run.clone();
    }
    Ok((session,run))
}
pub(crate) struct TaskGuard(pub TaskBinding);
impl TaskGuard {
    pub fn enter(binding: TaskBinding) -> Self {
        crate::agent::workspace_access::bind_delegate(
            &binding.child_session,
            &binding.parent_session,
        );
        ACTIVE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(binding.child_session.clone(), binding.clone());
        Self(binding)
    }
}
impl Drop for TaskGuard {
    fn drop(&mut self) {
        ACTIVE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0.child_session);
        crate::agent::workspace_access::unbind_delegate(&self.0.child_session);
    }
}

pub(crate) async fn stop_children(parent_run: &str) {
    runtime::stop_children(parent_run).await;
}

pub(crate) fn request_action(action: &str, params: &Value) -> String {
    let effective = if action.is_empty() || action.eq_ignore_ascii_case("child_task") {
        params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("dispatch")
    } else {
        action
    };
    effective.to_ascii_lowercase()
}

pub(crate) fn recover_finished(
    db: &DbState,
    parent: &str,
    _parent_run: &str,
    id: &str,
) -> Result<Option<Value>, String> {
    let parent_run: String =
        db.0.lock()
            .map_err(|e| e.to_string())?
            .query_row(
                "SELECT parent_run_id FROM agent_child_runs WHERE id=?1 AND parent_session_id=?2",
                params![id, parent],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
    // 先核验所有权，再检查终态，不能认养其他会话的执行。
    result(db, parent, id)?;
    let terminal: bool = db.0.lock().map_err(|e| e.to_string())?.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_run_traces WHERE id=?1 AND session_id=?2 AND status IN ('succeeded','failed','cancelled','interrupted'))",
        params![id,format!("task-{id}")], |r| r.get(0),
    ).map_err(|e| e.to_string())?;
    if !terminal {
        return Ok(None);
    }
    settle(
        db,
        &TaskBinding {
            parent_session: parent.into(),
            parent_run,
            child_session: format!("task-{id}"),
            child_run: id.into(),
        },
        &Ok(()),
        0,
    )
    .map(Some)
}

/// 以父任务身份与派发步骤查重；父会话续跑换 run_id 不会重新执行已经交付的子任务。
pub(crate) fn find_task(db: &DbState, session: &str, key: &str) -> Result<Option<String>, String> {
    db.0.lock().map_err(|e| e.to_string())?.query_row(
        "SELECT id FROM agent_child_runs WHERE parent_session_id=?1 AND task_id=?2 AND kind='task' ORDER BY started_at LIMIT 1",
        params![session, key], |r| r.get(0),
    ).optional().map_err(|e| e.to_string())
}

/// Reconstruct the same nested artifact path from recorded ownership, even after
/// in-memory delegate bindings have disappeared (for example after a restart).
fn artifact_base(conn: &rusqlite::Connection, id: &str) -> Result<std::path::PathBuf, String> {
    let mut current = id.to_owned();
    let mut descendants = Vec::new();
    let mut seen = std::collections::HashSet::new();
    loop {
        if !seen.insert(current.clone()) {
            return Err("子任务产物归属链存在循环。".into());
        }
        let parent_session: String = conn.query_row(
            "SELECT parent_session_id FROM agent_child_runs WHERE id=?1 AND kind='task'",
            [&current], |row| row.get(0),
        ).map_err(|e| e.to_string())?;
        descendants.push(format!("task-{current}"));
        // A resumed parent's run ID can change, but its child-session identity
        // remains stable. Only follow a session backed by a recorded task.
        let parent_id = parent_session.strip_prefix("task-");
        let recorded: bool = if let Some(parent_id) = parent_id {
            conn.query_row("SELECT EXISTS(SELECT 1 FROM agent_child_runs WHERE id=?1 AND kind='task')", [parent_id], |row| row.get(0)).map_err(|e| e.to_string())?
        } else { false };
        if !recorded {
            let mut path = crate::agent::path_guard::get_session_tmp_dir(&parent_session);
            for child in descendants.iter().rev() { path = path.join("delegated").join(child); }
            return Ok(path);
        }
        current = parent_id.unwrap().to_owned();
    }
}

/// 只有父会话可取本任务交接；不得用任意 session/output 取得跨会话数据。
pub(crate) fn result(db: &DbState, parent: &str, id: &str) -> Result<Value, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let (status, result): (String, Option<String>) = conn.query_row(
        "SELECT status,result_json FROM agent_child_runs WHERE id=?1 AND parent_session_id=?2 AND kind='task'",
        params![id, parent], |r| Ok((r.get(0)?, r.get(1)?)),
    ).map_err(|_| "该通用子任务不属于当前父会话或不存在。".to_string())?;
    let mut body = result
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    body["delivery_accepted"] = json!(status == "succeeded" && body["delivery_accepted"] == true);
    body["handoff_accepted"] = json!(status == "succeeded" && body["handoff_accepted"] == true);
    body["execution_ended"] = json!(status != "running");
    // status 与验收分别表达；没有正式提交的成功终态不能推断目标达成。
    body["execution_status"] = json!(status);
    let goal: Option<String> = conn
        .query_row(
            "SELECT goal FROM agent_run_traces WHERE id=?1 AND session_id=?2",
            params![id, format!("task-{id}")],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    body["goal"] = json!(goal);
    body["child_run_id"] = json!(id);
    body["child_session_id"] = json!(format!("task-{id}"));
    if body.get("artifact_base").and_then(Value::as_str).is_none() {
        body["artifact_base"] = json!(artifact_base(&conn, id)?);
    }
    Ok(body)
}

/// 交接以主循环事务提交的终态/收据为依据，Result::Ok 或模型“完成”文字均不足以算成功。
pub(crate) fn settle(
    db: &DbState,
    binding: &TaskBinding,
    outcome: &Result<(), String>,
    elapsed: u64,
) -> Result<Value, String> {
    let checkpoint = crate::agent::checkpoint::load_checkpoint(db, &binding.child_session)?;
    let receipt = checkpoint
        .as_ref()
        .and_then(|cp| cp.pending_finalization.as_ref());
    // Reuse the registered contract and the original evaluated payload, not the
    // later display projection (which adds host fields and resolves artifacts).
    let contract_error = match (checkpoint.as_ref(), receipt) {
        (Some(cp), Some(receipt)) if cp.phase == crate::agent::checkpoint::AgentPhase::Done && receipt.is_ready() => {
            crate::agent::execution::runtime::validate_output(db, &binding.child_run, &receipt.finish_payload).err()
        }
        _ => None,
    };
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut saved: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT status,completion_json FROM agent_run_traces WHERE id=?1 AND session_id=?2",
            params![binding.child_run, binding.child_session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if !saved.as_ref().is_some_and(|(status, _)| {
        matches!(
            status.as_str(),
            "succeeded" | "failed" | "cancelled" | "interrupted"
        )
    }) {
        let failure = json!({"summary":outcome.as_ref().err().cloned().unwrap_or_else(|| "执行器已退出，但未取得本次任务的正式终态。".into()),"status":"failed"});
        conn.execute("INSERT INTO agent_run_traces (id,session_id,goal,status,steps_json,completion_json,started_at,updated_at,completed_at)
            VALUES (?1,?2,'通用子任务','failed','[]',?3,?4,?4,?4) ON CONFLICT(id) DO UPDATE SET status='failed',completion_json=?3,updated_at=?4,completed_at=?4",
            params![binding.child_run,binding.child_session,failure.to_string(),chrono::Utc::now().to_rfc3339()]).map_err(|e|e.to_string())?;
        saved = Some(("failed".into(), Some(failure.to_string())));
    }
    let completion = saved
        .as_ref()
        .and_then(|(_, raw)| raw.as_deref())
        .and_then(|s| serde_json::from_str::<Value>(s).ok());
    let config_raw:Option<String>=conn.query_row("SELECT execution_config_json FROM agent_child_runs WHERE id=?1",[&binding.child_run],|r|r.get(0)).map_err(|e|e.to_string())?;
    let execution_config=config_raw.map(|raw|serde_json::from_str::<crate::agent::execution::runtime::ExecutionConfig>(&raw).map_err(|e|e.to_string())).transpose()?.unwrap_or_default();
    let mode_matches=receipt.is_some_and(|r|r.handoff==(execution_config.workflow.completion.as_deref()==Some("handoff")));
    let accepted = contract_error.is_none() && mode_matches && saved.as_ref().is_some_and(|(s, _)| s == "succeeded")
        && checkpoint
            .as_ref()
            .is_some_and(|cp| cp.phase == crate::agent::checkpoint::AgentPhase::Done)
        && receipt.is_some_and(|r| {
            r.is_ready()
                && !r.acceptance_fingerprint.is_empty()
                && completion
                    .as_ref()
                    .and_then(|v| v.get("finalization_receipt"))
                    .and_then(Value::as_str)
                    == Some(r.receipt_id.as_str())
        });
    let handoff=receipt.is_some_and(|r|r.handoff);
    let status = if accepted {
        "succeeded"
    } else if saved
        .as_ref()
        .is_some_and(|(s, _)| s == "cancelled" || s == "interrupted")
    {
        "cancelled"
    } else {
        "failed"
    };
    let body = json!({
        "child_run_id":binding.child_run,"child_session_id":binding.child_session,
        "parent_run_id":binding.parent_run,"execution_status":status,"delivery_accepted":accepted && !handoff,"handoff_accepted":accepted && handoff,
        "execution_ended":true,"delivery":completion,
        "artifact_base":artifact_base(&conn, &binding.child_run)?,
        "verification":receipt.map(|r| &r.verification),"review":receipt.map(|r| &r.review),
        "material_notices":receipt.map(|r| &r.material_notices),
        "host_error":outcome.as_ref().err(),
        "contract_check_error":contract_error,
        "unfinished": if accepted { Value::Null } else { json!("子目标未取得正式成功交付；请结合交付正文、错误和子任务记录继续处理。已有修改保留。") },
        "integration":"父 Agent 负责检查集成、整体目标及最终用户交付。"
    });
    conn.execute(
        "UPDATE agent_child_runs SET status=?2,result_json=?3,finished_at=?4,duration_ms=?5,consumed_at=?4 WHERE id=?1 AND kind='task'",
        params![binding.child_run,status,body.to_string(),chrono::Utc::now().to_rfc3339(),elapsed as i64],
    ).map_err(|e| e.to_string())?;
    drop(conn);
    crate::agent::agent_mailbox::coordination::kick(db);
    crate::agent::job_engine::notify_job_state_changed();
    Ok(body)
}

/// 导入宿主保存的实际副作用，恢复重放按来源去重；不导入子任务的 TODO/方案/验收裁决。
pub(crate) fn import_effects(
    db: &DbState,
    id: &str,
    parent_step: usize,
    context: &mut crate::agent::context::SandwichContext,
) -> Result<(), String> {
    let Some(cp) = crate::agent::checkpoint::load_checkpoint(db, &format!("task-{id}"))? else {
        return Ok(());
    };
    for (index, effect) in cp.data.side_effects.iter().enumerate() {
        let mut imported = effect.clone();
        imported.step_id = parent_step;
        imported.tool = format!("child_task/{id}/{}", effect.tool);
        imported.description = format!(
            "子任务 {id} 原步骤 {} 记录 {index}: {}",
            effect.step_id, effect.description
        );
        if !context.side_effects.iter().any(|existing| {
            existing.tool == imported.tool && existing.description == imported.description
        }) {
            context.side_effects.push(imported);
            context.change_verification = None;
        }
    }
    Ok(())
}

/// 子事件保持在独立进度载荷内，不能推进父任务的 complete 或人工状态。
pub(crate) fn relay_progress(id: &str, payload: Value) -> Option<Value> {
    let kind = payload["type"].as_str().unwrap_or_default();
    // A descendant is already safely wrapped; keep its identity across hops.
    if kind == "child_task_progress" { return Some(payload); }
    if !matches!(
        kind,
        "agent_paused" | "step_start" | "step_done" | "step_error" | "action_phase" | "complete"
    ) {
        return None;
    }
    Some(
        json!({"type":"child_task_progress","child_run_id":id,"event":{"type":kind,"step_id":payload.get("step_id"),"tool":payload.get("tool"),"action":payload.get("action"),"description":payload.get("description"),"message":payload.get("message"),"form":payload.get("form"),"child_session_id":format!("task-{id}")}}),
    )
}

/// Only the invocation preview crosses the session boundary. Do not copy the
/// child instruction's reasoning, output, or review material into the parent.
pub(crate) fn review_step_preview(step: &Value) -> Value {
    fn params_preview(params: &Value) -> Value {
        let mut result = serde_json::Map::new();
        for key in ["path", "file_path", "command", "cmd", "query", "pattern",
            "start_line", "end_line", "output_id"] {
            if let Some(value) = params.get(key) {
                if value.is_string() || value.is_number() {
                    result.insert(key.into(), value.clone());
                }
            }
        }
        for key in ["parallel", "commands"] {
            if let Some(items) = params[key].as_array() {
                result.insert(key.into(), Value::Array(items.iter().map(review_step_preview).collect()));
            }
        }
        Value::Object(result)
    }
    json!({
        "id": step.get("id").or_else(|| step.get("step_id")),
        "tool": step.get("tool"), "action": step.get("action"),
        "description": step.get("description"),
        "params": params_preview(step.get("params").or_else(|| step.get("command")).unwrap_or(&Value::Null)),
        "status": step.get("status"),
        "duration_ms": step.get("duration_ms").or_else(|| step["metrics"].get("tool_duration_ms")),
    })
}

pub(crate) fn relay_review_step(id: &str, payload: &Value) -> Option<Value> {
    (payload["type"] == "step_new").then(|| json!({
        "type": "child_task_progress", "child_run_id": id,
        "event": {"type": "step_new", "step": review_step_preview(&payload["step"])}
    }))
}

pub(crate) fn projection(body: &Value) -> String {
    let mut compact = body.clone();
    if let Some(goal) = compact["goal"].as_str().map(str::to_owned) {
        if goal.chars().count() > 2000 {
            compact["goal"] = json!(goal.chars().take(2000).collect::<String>());
            compact["goal_truncated"] = json!(true);
            compact["read_full"] = json!({"tool":"child_task","action":"result","params":{"child_run_id":body["child_run_id"]}});
        }
    }
    if let Some(delivery) = compact.get_mut("delivery") {
        let raw = delivery.to_string();
        if raw.chars().count() > 8_000 {
            *delivery =
                json!({"excerpt":raw.chars().take(8_000).collect::<String>(),"truncated":true});
            compact["read_full"] = json!({"tool":"child_task","action":"result","params":{"child_run_id":body["child_run_id"]}});
        }
    }
    compact.to_string()
}

#[cfg(test)]
mod tests;

/// 查询只展示宿主记录的步骤、活动与等待；不猜进度百分比或目标完成度。
pub(crate) fn status(db: &DbState, parent: &str, id: &str) -> Result<Value, String> {
    let mut body = result(db, parent, id)?;
    if body["execution_status"] == "running" && !runtime::attached(id) {
        if let Some(saved) = recover_finished(db, parent, "recovered-parent", id)? {
            body = saved;
        }
    }
    body["runtime_attached"] = json!(runtime::attached(id));
    let raw_summary = body
        .get("delivery")
        .and_then(|d| d.get("summary"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let summary_truncated = raw_summary.as_ref().is_some_and(|s| s.chars().count() > 180);
    let summary = raw_summary.map(|s| s.chars().take(180).collect::<String>());
    if let Some(map) = body.as_object_mut() {
        for field in ["goal", "delivery", "verification", "review"] {
            map.remove(field);
        }
    }
    if let Some(summary) = summary {
        body["summary"] = json!(summary);
        if summary_truncated {
            body["summary_truncated"] = json!(true);
        }
    }
    let session = format!("task-{id}");
    let mut latest_tool: Option<String> = None;
    let mut latest_step: Option<String> = None;
    if let Some(cp) = crate::agent::checkpoint::load_checkpoint(db, &session)? {
        body["step"] = json!(cp.step_id);
        body["phase"] = json!(cp.phase.as_str());
        if let Some(inst) = cp.prepared_instruction.as_ref() {
            let tool = inst.get_tool();
            if !tool.is_empty() {
                latest_tool = Some(tool);
            }
            let desc = inst.description.trim();
            if !desc.is_empty() && desc != "执行操作" {
                latest_step = Some(desc.chars().take(140).collect());
            }
        }
    }
    body["stage"] = json!(crate::agent::control::read_stage(&session));
    body["human_request"] = json!(runtime::paused(id));
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let meta:Option<(String,String,String,Option<String>)>=conn.query_row("SELECT goal,started_at,updated_at,completed_at FROM agent_run_traces WHERE id=?1 AND session_id=?2",params![id,session],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(|e|e.to_string())?;
    if let Some((goal, start, updated, end)) = meta {
        let clean_goal = goal.split("【宿主提供").next().unwrap_or(&goal).trim();
        let truncated = clean_goal.chars().count() > 300;
        body["task"] = json!(clean_goal.chars().take(300).collect::<String>());
        if truncated {
            body["task_truncated"] = json!(true);
        } else if let Some(map) = body.as_object_mut() {
            map.remove("task_truncated");
        }
        body["started_at"] = json!(start);
        body["updated_at"] = json!(updated);
        body["finished_at"] = json!(end);
    }
    if let Ok(Some((purpose, model_id, run_start, run_end, duration_ms, cp_json))) = conn
        .query_row(
            "SELECT purpose, model_id, started_at, finished_at, duration_ms, execution_checkpoint_json FROM agent_child_runs WHERE id=?1 AND kind='task'",
            [id],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()
    {
        if let Some(p) = purpose.filter(|s| !s.is_empty()) {
            body["purpose"] = json!(p);
        }
        if let Some(m) = model_id.filter(|s| !s.is_empty()) {
            body["model_id"] = json!(m);
        }
        if let Some(d) = duration_ms {
            body["duration_ms"] = json!(d);
        }
        if body.get("started_at").and_then(Value::as_str).is_none() {
            if let Some(s) = run_start {
                body["started_at"] = json!(s);
            }
        }
        if body.get("finished_at").and_then(Value::as_str).is_none() {
            if let Some(f) = run_end {
                body["finished_at"] = json!(f);
            }
        }
        if let Some(raw_cp) = cp_json {
            if let Ok(parsed) = serde_json::from_str::<Value>(&raw_cp) {
                if let Some(selection)=parsed["state"]["dispatch_input"].get("model_selection") {body["model_selection"]=selection.clone();}
                let params = &parsed["state"]["dispatch_input"]["params"];
                if let Some(clean_task) = params["task"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    let truncated = clean_task.chars().count() > 300;
                    body["task"] = json!(clean_task.chars().take(300).collect::<String>());
                    if truncated {
                        body["task_truncated"] = json!(true);
                    } else if let Some(map) = body.as_object_mut() {
                        map.remove("task_truncated");
                    }
                }
                if body.get("purpose").is_none() {
                    if let Some(prof) = params["profile"].as_str().filter(|s| !s.is_empty()) {
                        body["purpose"] = json!(prof);
                    }
                }
                if let Some(role) = params["role"].as_str().filter(|s| !s.is_empty()) {
                    body["role"] = json!(role);
                }
                if let Some(role_label) = params["role_label"]
                    .as_str()
                    .or_else(|| params["roleLabel"].as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    body["role_label"] = json!(role_label);
                }
            }
        }
    }
    if body.get("role_label").is_none() {
        if let Some(role_name) = body.get("role").and_then(Value::as_str) {
            if let Ok(Some(label)) = conn
                .query_row(
                    "SELECT label FROM agent_role_cards WHERE name=?1",
                    [role_name],
                    |r| r.get::<_, String>(0),
                )
                .optional()
            {
                if !label.trim().is_empty() {
                    body["role_label"] = json!(label.trim());
                }
            }
        }
    }
    if let Ok(total_steps) = conn.query_row(
        "SELECT COUNT(*) FROM agent_run_steps WHERE run_id=?1",
        [id],
        |r| r.get::<_, i64>(0),
    ) {
        if total_steps > 0 {
            body["total_steps"] = json!(total_steps);
        }
    }
    let mut recent_steps = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT seq, step_json FROM agent_run_steps WHERE run_id=?1 ORDER BY seq DESC LIMIT 6",
    ) {
        if let Ok(rows) = stmt.query_map([id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        }) {
            for row in rows.flatten() {
                let (seq, step_raw) = row;
                if let Ok(step_val) = serde_json::from_str::<Value>(&step_raw) {
                    let s_tool = step_val["tool"]
                        .as_str()
                        .or_else(|| step_val["action"].as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned);
                    let s_desc = step_val["description"]
                        .as_str()
                        .or_else(|| step_val["finding"].as_str())
                        .or_else(|| step_val["thought"].as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty() && *s != "执行操作")
                        .map(|s| s.chars().take(140).collect::<String>());
                    if latest_tool.is_none() {
                        latest_tool = s_tool.clone();
                    }
                    if latest_step.is_none() {
                        latest_step = s_desc.clone();
                    }
                    let p = &step_val["params"];
                    let target = p["path"]
                        .as_str()
                        .or_else(|| p["file_path"].as_str())
                        .or_else(|| p["command"].as_str())
                        .or_else(|| p["cmd"].as_str())
                        .or_else(|| p["query"].as_str())
                        .or_else(|| p["pattern"].as_str())
                        .map(|s| s.lines().next().unwrap_or(s).trim())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.chars().take(90).collect::<String>());
                    let s_status = step_val["status"]
                        .as_str()
                        .unwrap_or("succeeded")
                        .to_owned();
                    recent_steps.push(json!({
                        "seq": seq + 1,
                        "tool": s_tool.unwrap_or_else(|| "step".into()),
                        "target": target,
                        "description": s_desc,
                        "status": s_status,
                    }));
                }
            }
        }
    }
    if !recent_steps.is_empty() {
        recent_steps.reverse();
        body["recent_steps"] = json!(recent_steps);
    }
    if let Some(tool) = latest_tool {
        body["latest_tool"] = json!(tool);
    }
    if let Some(step_desc) = latest_step {
        body["latest_step"] = json!(step_desc);
    }
    let pending: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_messages WHERE target_run_id=?1 AND delivered_at IS NULL",
            [id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    body["queued_messages"] = json!(pending);
    if runtime::paused(id).is_some() {
        body["activity"] = json!("waiting_human")
    } else {
        body["activity"] = body["execution_status"].clone()
    }
    Ok(body)
}

pub(crate) fn task_owner(
    db: &DbState,
    actor: &crate::agent::agent_mailbox::Actor,
    id: &str,
) -> Result<String, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let owner: String = conn
        .query_row(
            "SELECT parent_session_id FROM agent_child_runs WHERE id=?1 AND kind='task'",
            [id],
            |r| r.get(0),
        )
        .map_err(|_| "通用子任务不存在。".to_string())?;
    if owner == actor.session {
        return Ok(owner);
    }
    let predecessor: Option<String> = conn
        .query_row(
            "SELECT resumed_from FROM agent_child_runs WHERE id=?1 AND kind='task'",
            [&actor.run],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    drop(conn);
    if predecessor.as_deref() == Some(id) {
        return Ok(owner);
    }
    crate::agent::agent_mailbox::authorize_target(db, actor, id)?;
    Ok(owner)
}

/// Desktop session may inspect and intervene in descendants, not other task trees.
/// Keep each node's actual parent for scoped result lookup and the originating
/// root run for user-authored messages and receipts.
fn visible_tasks(db: &DbState, session: &str, target: Option<&str>) -> Result<Vec<(String,String,String,usize)>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare(
        "WITH RECURSIVE tree(id,owner,root_run,depth,sort_path) AS (
            SELECT id,parent_session_id,parent_run_id,1,COALESCE(started_at,'') || '/' || id FROM agent_child_runs WHERE parent_session_id=?1 AND kind='task'
            UNION ALL
            SELECT c.id,c.parent_session_id,t.root_run,t.depth+1,t.sort_path || '>' || COALESCE(c.started_at,'') || '/' || c.id FROM agent_child_runs c JOIN tree t ON c.parent_session_id='task-' || t.id WHERE c.kind='task' AND t.depth < 64
         ) SELECT id,owner,root_run,depth FROM tree WHERE (?2 IS NULL OR id=?2) ORDER BY sort_path"
    ).map_err(|e| e.to_string())?;
    let rows=stmt.query_map(params![session,target], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get::<_,i64>(3)? as usize))).map_err(|e|e.to_string())?;
    rows.collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())
}

#[tauri::command]
pub(crate) async fn control_child_task(
    state: tauri::State<'_, DbState>,
    session_id: String,
    action: String,
    params: Value,
) -> Result<Value, String> {
    use tauri::Manager;
    if action == "list" {
        let mut tasks = Vec::new();
        for (id, owner, _, depth) in visible_tasks(state.inner(), &session_id, None)? {
            let mut body = status(state.inner(), &owner, &id)?;
            body["depth"] = json!(depth);
            body["parent_session_id"] = json!(owner);
            tasks.push(body);
        }
        return Ok(json!({"tasks":tasks}));
    }
    let id = params["child_run_id"].as_str().ok_or("缺少 child_run_id。")?;
    let (_, owner, parent_run, _) = visible_tasks(state.inner(), &session_id, Some(id))?
        .into_iter().next().ok_or("该子任务不属于当前会话的任务树；操作未执行。")?;
    let full = result(state.inner(), &owner, id)?;
    match action.as_str() {
        "status" => status(state.inner(), &owner, id),
        // 桌面“查看完整内容”：直接返回宿主保存的完整交接，不经模型侧的 2000/8000 字投影截断。
        "result" => Ok(full),
        "receipt" => crate::agent::agent_mailbox::receipt(
            state.inner(),
            &crate::agent::agent_mailbox::Actor::new(&parent_run, &session_id),
            params["message_id"].as_str().unwrap_or_default(),
        ),
        "cancel" => {
            runtime::request_cancel(id)?;
            Ok(json!({"cancel_requested":true,"execution_ended":false}))
        }
        "answer" => {
            if runtime::paused(id).is_none() {
                return Err("该子任务当前没有待回答的人工请求；回答未发送。".into());
            }
            let message = params["message"].as_str().ok_or("缺少回答内容。")?;
            crate::agent::control::send_human_intervention(
                state,
                format!("task-{id}"),
                message.into(),
            )
            .await?;
            Ok(json!({"answer_sent":true}))
        }
        "send" => {
            let mut payload = params.clone();
            payload["target"] = json!(id);
            crate::agent::agent_mailbox::send(
                state.inner(),
                &crate::agent::agent_mailbox::Actor::new(&parent_run, &session_id),
                &payload,
                None,
                true,
            )
        }
        _ => Err("未知动作；允许 list/status/result/receipt/send/answer/cancel。".into()),
    }
}
