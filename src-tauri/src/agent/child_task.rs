//! 完整任务委派的宿主边界。执行仍由 main_loop 承担；这里仅维护身份、授权引用和交接。
//! 工作区变更按序，执行器独立运行；不复制主循环，不改变专用子 Agent 的结果契约。
pub(crate) mod runtime;
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
pub(crate) fn is_delegating(parent: &str) -> bool {
    ACTIVE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .any(|b| b.parent_session == parent)
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

pub(crate) fn busy_jobs(
    db: &DbState,
    parent: &str,
    resumed: Option<&str>,
) -> Result<Vec<String>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare("SELECT id FROM agent_child_runs WHERE parent_run_id=?1 AND status='running' AND (?2 IS NULL OR id!=?2)
        UNION ALL SELECT id FROM agent_background_jobs WHERE parent_run_id=?1 AND status='running'").map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![parent, resumed], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
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
    body["artifact_base"] = json!(crate::agent::path_guard::get_session_tmp_dir(parent)
        .join("delegated")
        .join(format!("task-{id}")));
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
    let accepted = saved.as_ref().is_some_and(|(s, _)| s == "succeeded")
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
        "parent_run_id":binding.parent_run,"execution_status":status,"delivery_accepted":accepted,
        "execution_ended":true,"delivery":completion,
        "artifact_base":crate::agent::path_guard::get_session_tmp_dir(&binding.parent_session).join("delegated").join(&binding.child_session),
        "verification":receipt.map(|r| &r.verification),"review":receipt.map(|r| &r.review),
        "material_notices":receipt.map(|r| &r.material_notices),
        "host_error":outcome.as_ref().err(),
        "unfinished": if accepted { Value::Null } else { json!("子目标未取得正式成功交付；请结合交付正文、错误和子任务记录继续处理。已有修改保留。") },
        "integration":"父 Agent 负责检查集成、整体目标及最终用户交付。"
    });
    conn.execute(
        "UPDATE agent_child_runs SET status=?2,result_json=?3,finished_at=?4,duration_ms=?5,consumed_at=?4 WHERE id=?1 AND kind='task'",
        params![binding.child_run,status,body.to_string(),chrono::Utc::now().to_rfc3339(),elapsed as i64],
    ).map_err(|e| e.to_string())?;
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
    if !matches!(
        kind,
        "agent_paused" | "step_start" | "step_done" | "step_error" | "action_phase" | "complete"
    ) {
        return None;
    }
    Some(
        json!({"type":"child_task_progress","child_run_id":id,"event":{"type":kind,"description":payload.get("description"),"message":payload.get("message"),"form":payload.get("form"),"child_session_id":format!("task-{id}")}}),
    )
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
    if let Some(map) = body.as_object_mut() {
        for field in ["goal", "delivery", "verification", "review"] {
            map.remove(field);
        }
    }
    let session = format!("task-{id}");
    if let Some(cp) = crate::agent::checkpoint::load_checkpoint(db, &session)? {
        body["step"] = json!(cp.step_id);
        body["phase"] = json!(cp.phase.as_str());
    }
    body["stage"] = json!(crate::agent::control::read_stage(&session));
    body["human_request"] = json!(runtime::paused(id));
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let meta:Option<(String,String,String,Option<String>)>=conn.query_row("SELECT goal,started_at,updated_at,completed_at FROM agent_run_traces WHERE id=?1 AND session_id=?2",params![id,session],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(|e|e.to_string())?;
    if let Some((goal, start, updated, end)) = meta {
        body["task"] = json!(goal
            .split("【宿主提供")
            .next()
            .unwrap_or(&goal)
            .chars()
            .take(240)
            .collect::<String>());
        body["started_at"] = json!(start);
        body["updated_at"] = json!(updated);
        body["finished_at"] = json!(end);
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

#[tauri::command]
pub(crate) async fn control_child_task(
    state: tauri::State<'_, DbState>,
    session_id: String,
    action: String,
    params: Value,
) -> Result<Value, String> {
    use tauri::Manager;
    if action == "list" {
        let ids: Vec<String> = {
            let conn = state.0.lock().map_err(|e| e.to_string())?;
            let mut stmt=conn.prepare("SELECT id FROM agent_child_runs WHERE parent_session_id=?1 AND kind='task' ORDER BY started_at DESC LIMIT 20").map_err(|e|e.to_string())?;
            let rows = stmt
                .query_map([&session_id], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
        };
        return ids
            .iter()
            .map(|id| status(state.inner(), &session_id, id))
            .collect::<Result<Vec<_>, _>>()
            .map(|tasks| json!({"tasks":tasks}));
    }
    let id = params["child_run_id"]
        .as_str()
        .ok_or("缺少 child_run_id。")?;
    result(state.inner(), &session_id, id)?;
    let parent_run: String = {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT parent_run_id FROM agent_child_runs WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?
    };
    match action.as_str() {
        "status" => status(state.inner(), &session_id, id),
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
        _ => Err("未知动作；允许 list/status/send/answer/cancel。".into()),
    }
}
