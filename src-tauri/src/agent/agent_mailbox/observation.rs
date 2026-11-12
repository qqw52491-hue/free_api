//! Read-only views of registered executions. Historical access does not change
//! the ownership of a child, its mailbox, or its receipt.
use super::*;

pub(super) fn member_on(conn: &Connection, actor: &Actor, id: &str) -> Result<bool, String> {
    if resolve(conn, actor, id).is_ok_and(|(member, kind)| member.run == id && !kind.is_empty()) {
        return Ok(false);
    }
    let historical: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_child_runs c JOIN agent_run_traces p
         ON p.id=c.parent_run_id AND p.session_id=c.parent_session_id
         WHERE c.id=?1 AND c.parent_session_id=?2 AND c.parent_run_id<>?3)",
            params![id, actor.session, actor.run],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if historical {
        Ok(true)
    } else {
        Err("该子执行不属于当前任务树或本会话此前直接派发的成员；未读取。".into())
    }
}

pub(crate) fn history(
    db: &DbState,
    actor: &Actor,
    offset: u64,
    limit: u64,
) -> Result<Value, String> {
    let conn = db.reader()?;
    let limit = limit.clamp(1, 50);
    let mut statement = conn.prepare(
        "SELECT c.id,c.parent_run_id,c.kind,c.purpose,c.status,c.model_id,c.started_at,c.finished_at,
                c.result_json IS NOT NULL,c.applied_at,c.resumed_from
         FROM agent_child_runs c JOIN agent_run_traces p ON p.id=c.parent_run_id AND p.session_id=c.parent_session_id
         WHERE c.parent_session_id=?1 AND c.parent_run_id<>?2
         ORDER BY c.started_at DESC,c.id DESC LIMIT ?3 OFFSET ?4",
    ).map_err(|e| e.to_string())?;
    let rows = statement.query_map(params![actor.session, actor.run, limit+1, offset.min(i64::MAX as u64)], |r| Ok(json!({
        "child_run_id":r.get::<_,String>(0)?,"parent_run_id":r.get::<_,String>(1)?,
        "kind":r.get::<_,String>(2)?,"purpose":r.get::<_,Option<String>>(3)?,
        "execution_status":r.get::<_,String>(4)?,"model":r.get::<_,String>(5)?,
        "started_at":r.get::<_,String>(6)?,"finished_at":r.get::<_,Option<String>>(7)?,
        "result_available":r.get::<_,bool>(8)?,"collected_at":r.get::<_,Option<String>>(9)?,
        "resumed_from":r.get::<_,Option<String>>(10)?,"historical_reference":true,
    }))).map_err(|e| e.to_string())?;
    let mut tasks = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let has_more = tasks.len() > limit as usize;
    tasks.truncate(limit as usize);
    Ok(
        json!({"tasks":tasks,"has_more":has_more,"next_offset":offset.saturating_add(tasks.len() as u64),
        "scope_notice":"本会话此前直接派发的历史成员。请核对与当前目标的关系；旧运行 ID 不可作为本轮 wait_jobs 句柄。"}),
    )
}

pub(crate) fn status(db: &DbState, actor: &Actor, id: &str) -> Result<Value, String> {
    let (mut body, trace_session) = {
        let conn = db.reader()?;
        let historical = member_on(&conn, actor, id)?;
        if resolve(&conn, actor, id).is_ok_and(|(_, kind)| kind == "main") {
            conn.query_row(
                "SELECT session_id,status,goal,started_at,updated_at,completed_at,completion_json IS NOT NULL,(SELECT COUNT(*) FROM agent_run_steps WHERE run_id=?1) FROM agent_run_traces WHERE id=?1",
                [id], |r| Ok((json!({"run_id":id,"child_run_id":id,"parent_run_id":null,"kind":"main",
                    "execution_status":r.get::<_,String>(1)?,"task":r.get::<_,String>(2)?.chars().take(200).collect::<String>(),
                    "started_at":r.get::<_,String>(3)?,"updated_at":r.get::<_,String>(4)?,"finished_at":r.get::<_,Option<String>>(5)?,
                    "result_available":r.get::<_,bool>(6)?,"total_steps":r.get::<_,u64>(7)?,"historical_reference":false}),Some(r.get::<_,String>(0)?)))
            ).map_err(|e|e.to_string())?
        } else { conn.query_row(
            "SELECT c.parent_run_id,c.kind,c.purpose,c.model_id,c.status,c.started_at,c.finished_at,
                    c.error_kind,c.consumed_at,c.applied_at,c.resumed_from,c.result_json IS NOT NULL,
                    t.session_id,t.updated_at,t.goal,p.phase,
                    (SELECT COUNT(*) FROM agent_run_steps WHERE run_id=c.id),
                    CASE WHEN json_valid(c.execution_checkpoint_json) THEN COALESCE(
                        json_extract(c.execution_checkpoint_json,'$.state.main_review.role_label'),
                        json_extract(c.execution_checkpoint_json,'$.state.dispatch_input.params.role_label')) END,
                    CASE WHEN json_valid(c.result_json) THEN COALESCE(
                        json_extract(c.result_json,'$.summary'),
                        json_extract(c.result_json,'$.delivery.summary')) END
             FROM agent_child_runs c LEFT JOIN agent_run_traces t ON t.id=c.id
             LEFT JOIN agent_checkpoints p ON p.run_id=t.session_id WHERE c.id=?1",
            [id], |r| Ok((json!({
                "child_run_id":id,"parent_run_id":r.get::<_,String>(0)?,
                "kind":r.get::<_,String>(1)?,"purpose":r.get::<_,Option<String>>(2)?,
                "model_id":r.get::<_,String>(3)?,"execution_status":r.get::<_,String>(4)?,
                "started_at":r.get::<_,String>(5)?,"finished_at":r.get::<_,Option<String>>(6)?,
                "error_kind":r.get::<_,Option<String>>(7)?,"consumed_at":r.get::<_,Option<String>>(8)?,
                "collected_at":r.get::<_,Option<String>>(9)?,"resumed_from":r.get::<_,Option<String>>(10)?,
                "result_available":r.get::<_,bool>(11)?,"updated_at":r.get::<_,Option<String>>(13)?,
                "task":r.get::<_,Option<String>>(14)?.map(|s|s.lines().next().unwrap_or("").chars().take(200).collect::<String>()),
                "phase":r.get::<_,Option<String>>(15)?,"total_steps":r.get::<_,u64>(16)?,
                "role_label":r.get_ref(17)?.as_str().ok().map(|s|s.chars().take(120).collect::<String>()),
                "result_summary":r.get_ref(18)?.as_str().ok().map(|s|s.chars().take(240).collect::<String>()),
                "historical_reference":historical,
            }), r.get::<_,Option<String>>(12)?)),
        ).map_err(|e| e.to_string())? }
    };
    body["recent_steps"] = json!(crate::agent::reviewer_gatekeeper::recent_execution_steps(
        db, id
    )?);
    body["stage"] = json!(trace_session
        .as_deref()
        .and_then(crate::agent::control::read_stage));
    let attached = crate::agent::child_task::runtime::attached(id);
    body["main_executor_attached"] = json!(attached);
    body["waiting_human"] =
        json!(attached.then(|| crate::agent::child_task::runtime::paused(id).is_some()));
    body["next"] =
        json!("可用 read_agent_result 的 steps 或 result 视图回查；已记录步骤数不表示完成比例。");
    Ok(body)
}

/// The caller has authorized the child. Match the exact step-output identity
/// too, since the generic reader can fall back to an active parent's session.
pub(super) fn step_output(db: &DbState, id: &str, params: &Value) -> Result<Value, String> {
    let output_id = params["output_id"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or("step_output 缺少非空 output_id。")?;
    let session = crate::agent::reviewer_gatekeeper::review_run_session(db, id, id)?;
    let source = crate::agent::run_trace::saved_output_source(db, &session, output_id)?;
    if source["run_id"].as_str() != Some(id) {
        return Err("该输出未精确关联到指定子执行；未读取。".into());
    }
    let result = crate::agent::task_store::dispatch(db, &session, "get_step_output", params);
    if !result.success {
        return Err(result.stderr);
    }
    serde_json::from_str(&result.stdout).map_err(|e| e.to_string())
}
