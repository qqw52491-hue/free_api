//! Explicit peer questions can start a read-only answer attempt. Admission and
//! root completion serialize in SQLite; the original delivery is never reopened.
use super::*;
use crate::agent::child_worker::WorkerResult;
use crate::agent::execution::actions::{Capabilities, Profile, Session};
use rusqlite::Connection;
use std::path::PathBuf;

pub(crate) const PENDING_REPLY: &str = "agent_reply:pending";
const PURPOSE: &str = "readonly_reply";

#[derive(Clone, Debug)]
pub(super) struct Ticket {
    pub(super) request: String,
    pub(super) run: String,
    pub(super) original: String,
    pub(super) responsible: String,
    pub(super) root: Actor,
    pub(super) requester: Actor,
    pub(super) model: String,
    pub(super) workspace: PathBuf,
    pub(super) prior: String,
    pub(super) inbox: bool,
}

pub(crate) fn root_active_on(conn: &Connection, run: &str) -> Result<bool, String> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM agent_run_traces WHERE id=?1 AND status IN ('running','waiting_human'))",
        [run], |r| r.get(0)).map_err(|e| e.to_string())
}

pub(crate) fn root_active(db: &DbState, run: &str) -> bool {
    db.0.lock()
        .ok()
        .and_then(|c| root_active_on(&c, run).ok())
        .unwrap_or(false)
}

/// Must be called inside the terminal transaction, before publishing Done.
pub(crate) fn pending_on(conn: &Connection, run: &str) -> Result<bool, String> {
    let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='agent_child_runs')", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    if !exists {
        return Ok(false);
    }
    if super::coordination::pending_on(conn, run)? {
        return Ok(true);
    }
    conn.query_row("SELECT EXISTS(SELECT 1 FROM agent_child_runs WHERE parent_run_id=?1 AND purpose IN (?2,'readonly_inbox') AND (status IN ('queued','running') OR applied_at IS NULL OR applied_at=''))",
        params![run, PURPOSE], |r| r.get(0)).map_err(|e| e.to_string())
}

pub(crate) fn read_result(db: &DbState, actor: &Actor, params: &Value) -> Result<Value, String> {
    let id = params["child_run_id"]
        .as_str()
        .ok_or("read_agent_result 缺少 child_run_id。")?;
    if params.get("view").is_some_and(|v| !v.is_string()) {
        return Err("read_agent_result.view 类型错误：期望 string。".into());
    }
    for field in ["offset", "limit"] {
        if params.get(field).is_some_and(|v| v.as_u64().is_none()) {
            return Err(format!(
                "read_agent_result.{field} 类型错误：期望非负整数。"
            ));
        }
    }
    let view = params["view"].as_str().unwrap_or("result");
    if !matches!(view, "result" | "history" | "checkpoint" | "dispatch_input" | "steps" | "step_output") {
        return Err("view 仅允许 result/history/checkpoint/dispatch_input/steps/step_output。".into());
    }
    if matches!(view, "steps" | "step_output") {
        let historical = {
            let conn = db.reader()?;
            observation::member_on(&conn, actor, id)?
        };
        if view == "steps" {
            let mut page = crate::agent::reviewer_gatekeeper::execution_steps_page(db, id, params)?;
            page["historical_reference"] = json!(historical);
            return Ok(page);
        }
        return observation::step_output(db, id, params);
    }
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let (target, kind) = match resolve(&conn, actor, id) {
        Ok(member) => member,
        Err(_) if view == "result" && observation::member_on(&conn, actor, id)? =>
            (Actor::new(id, ""), "historical_child".into()),
        Err(error) => return Err(error),
    };
    if view == "dispatch_input" && kind == "main" {
        return Err("主任务没有子任务派发输入快照；本次未读取交付结果替代该快照。".into());
    }
    let (status, mut text): (String, Option<String>) = if kind == "main" {
        conn.query_row(
            "SELECT status,completion_json FROM agent_run_traces WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    } else {
        conn.query_row(
            if view == "dispatch_input" {
                "SELECT status,json_extract(execution_checkpoint_json,'$.state.dispatch_input') FROM agent_child_runs WHERE id=?1"
            } else if view == "checkpoint" {
                "SELECT status,execution_checkpoint_json FROM agent_child_runs WHERE id=?1"
            } else if view == "history" {
                "SELECT status,message_history_json FROM agent_child_runs WHERE id=?1"
            } else {
                "SELECT status,result_json FROM agent_child_runs WHERE id=?1"
            },
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }
    .map_err(|e| e.to_string())?;
    let mut source = if kind == "main" {
        "agent_run_traces.completion_json"
    } else if view == "dispatch_input" {
        "agent_child_runs.execution_checkpoint_json.state.dispatch_input"
    } else if view == "checkpoint" {
        "agent_child_runs.execution_checkpoint_json"
    } else if view == "history" {
        "agent_child_runs.message_history_json"
    } else {
        "agent_child_runs.result_json"
    };
    if kind=="task" && view=="checkpoint" {
        let full:Option<String>=conn.query_row("SELECT json_object('phase',phase,'version',version,'step_id',json_extract(snapshot_json,'$.step_id'),'pending',json_extract(snapshot_json,'$.pending'),'prepared_instruction',json_extract(snapshot_json,'$.prepared_instruction')) FROM agent_checkpoints WHERE run_id=?1",[&target.session],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
        if full.is_some() {text=full;source="agent_checkpoints(phase,version,step_id,pending,prepared_instruction)";}
    }
    if kind == "task" && view == "history" && text.is_none() {
        // Generic tasks already persist their history in their own checkpoint.
        // Project only existing conversational material, never approval/control state.
        text = conn.query_row("SELECT json_object('current_goal',json_extract(snapshot_json,'$.data.current_goal'),'turns_history',json_extract(snapshot_json,'$.data.turns_history'),'progress_summary',json_extract(snapshot_json,'$.data.progress_summary'),'observation_summary',json_extract(snapshot_json,'$.data.observation_summary')) FROM agent_checkpoints WHERE run_id=?1",
            [&target.session],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
        source="agent_checkpoints.data(current_goal,turns_history,progress_summary,observation_summary)";
    }
    let text = text.unwrap_or_default();
    let offset = params["offset"]
        .as_u64()
        .unwrap_or(0)
        .min(usize::MAX as u64) as usize;
    let limit = params["limit"].as_u64().unwrap_or(8192).clamp(1, 8192) as usize;
    let total = text.chars().count();
    let page: String = text.chars().skip(offset).take(limit).collect();
    Ok(
        json!({"child_run_id":id,"execution_status":status,"source_field":source,"offset":offset,"next_offset":offset.saturating_add(page.chars().count()),"total_chars":total,"text":page,"historical_reference":true}),
    )
}

fn insert_message(
    conn: &Connection,
    id: &str,
    from: &Actor,
    to: &Actor,
    root: &Actor,
    payload: &Value,
) -> Result<(), String> {
    conn.execute("INSERT OR IGNORE INTO agent_messages(id,session_id,root_run_id,sender_run_id,sender_session_id,sender_kind,target_run_id,target_session_id,payload_json,created_at) VALUES (?1,?2,?3,?4,?5,'agent',?6,?7,?8,?9)",
        params![id,root.session,root.run,from.run,from.session,to.run,to.session,payload.to_string(),chrono::Utc::now().to_rfc3339()]).map(|_| ()).map_err(|e|e.to_string())
}

fn register(
    db: &DbState,
    actor: &Actor,
    params: &Value,
    retry: &str,
) -> Result<(Value, Option<Ticket>), String> {
    validate_payload(params)?;
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let owner = root(&tx, actor)?;
    let (target, kind) = resolve(&tx, actor, params["target"].as_str().unwrap())?;
    if kind == "main" {
        return Err("运行中的父任务请用 send 提问，未启动答复任务。".into());
    }
    let payload = json!({"message":params.get("message"),"data":params.get("data"),"refs":params.get("refs"),"reply_to":target.run});
    let key = params["idempotency_key"].as_str().unwrap_or(retry);
    let request = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        json!([actor.run, "readonly_reply", key])
            .to_string()
            .as_bytes(),
    )
    .to_string();
    let existing: Option<(String,String,String)> = tx.query_row("SELECT m.target_run_id,m.payload_json,c.status FROM agent_messages m JOIN agent_child_runs c ON c.id=m.target_run_id WHERE m.id=?1 AND m.sender_run_id=?2",params![request,actor.run],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(|e|e.to_string())?;
    if let Some((run, body, status)) = existing {
        if body != payload.to_string() {
            return Err("追问重试键已绑定其他问题或目标，未重复启动。".into());
        }
        return Ok((
            json!({"request_id":request,"reply_run_id":run,"child_run_id":run,"kind":"worker","execution_status":status,"read_only":true,"duplicate":true}),
            None,
        ));
    }
    if !root_active_on(&tx, &owner.run)? {
        return Err("所属主任务已结束或状态未知；追问未受理，未启动执行。".into());
    }
    if actor.run != owner.run {
        let active: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_child_runs WHERE id=?1 AND status IN ('queued','running'))",[&actor.run],|r|r.get(0)).map_err(|e|e.to_string())?;
        if !active {
            return Err("发送方执行已结束；新的自动追问未受理。".into());
        }
    }
    // A read-only answer may explain a submitted result, never undo cancellation.
    let (status, model, workspace, prior, responsible): (String,String,String,Option<String>,String) = tx.query_row("SELECT status,model_id,workspace_root,result_json,parent_run_id FROM agent_child_runs WHERE id=?1",[&target.run],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).map_err(|e|e.to_string())?;
    if status != "succeeded" {
        return Err(format!("目标状态为 {status}；仅已成功结束的成员可自动只读答复，运行中请 send，失败或取消任务由负责人显式续派。"));
    }
    let (total, workers): (i64,i64) = tx.query_row("SELECT COUNT(*),COALESCE(SUM(kind='worker'),0) FROM agent_child_runs WHERE parent_run_id=?1 AND status IN ('queued','running')",[&owner.run],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|e|e.to_string())?;
    if total >= i64::from(crate::agent::child_reviewer::MAX_TOTAL_CHILDREN_PER_RUN)
        || workers >= i64::from(crate::agent::child_reviewer::MAX_CHILDREN_PER_KIND_PER_RUN)
    {
        return Err("当前子任务并发额度已满，追问未受理；可等待已有成员结束后重试。".into());
    }
    let run = uuid::Uuid::new_v4().to_string();
    let reply = Actor::new(&run, &format!("child:{run}"));
    let now = chrono::Utc::now().to_rfc3339();
    // Host-owned coordination work is a direct child of the live root. This lets
    // the existing receipt reducer account for it even when B's original owner ended.
    tx.execute("INSERT INTO agent_child_runs(id,parent_run_id,parent_session_id,workspace_root,model_id,status,started_at,kind,resumed_from,task_id,attempt_id,purpose,execution_config_json) VALUES (?1,?2,?3,?4,?5,'queued',?6,'worker',?7,?8,1,?9,(SELECT execution_config_json FROM agent_child_runs WHERE id=?7))",
        params![run,owner.run,owner.session,workspace,model,now,target.run,format!("reply:{request}"),PURPOSE]).map_err(|e|e.to_string())?;
    insert_message(&tx, &request, actor, &reply, &owner, &payload)?;
    insert_message(
        &tx,
        &format!("{request}:started"),
        &reply,
        &owner,
        &owner,
        &json!({"message":"已受理只读追问，等待答复；原成果不修改。","data":{"request_id":request,"reply_run_id":run,"target_run_id":target.run,"requester_run_id":actor.run,"original_responsible_run_id":responsible}}),
    )?;
    if responsible != owner.run && responsible != actor.run {
        let (responsible_actor, _) = resolve(&tx, &owner, &responsible)?;
        insert_message(
            &tx,
            &format!("{request}:responsible-started"),
            &reply,
            &responsible_actor,
            &owner,
            &json!({"message":"所属成员的新一轮只读追问已受理，原交付不修改。","data":{"request_id":request,"reply_run_id":run,"target_run_id":target.run,"requester_run_id":actor.run}}),
        )?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok((
        json!({"request_id":request,"reply_run_id":run,"child_run_id":run,"kind":"worker","execution_status":"queued","read_only":true,"next":"用 wait() 等待自己的收信，或 read_agent_result(child_run_id=reply_run_id) 查结果；修改请求由原负责人安排。"}),
        Some(Ticket {
            request,
            run,
            original: target.run,
            responsible,
            root: owner,
            requester: actor.clone(),
            model,
            workspace: workspace.into(),
            prior: prior.unwrap_or_default(),
            inbox: false,
        }),
    ))
}

pub(crate) fn ask(
    db: &DbState,
    actor: &Actor,
    params: &Value,
    retry: &str,
    app: Option<tauri::AppHandle>,
) -> Result<Value, String> {
    let (body, ticket) = register(db, actor, params, retry)?;
    if let Some(ticket) = ticket {
        launch(db, ticket, app);
    }
    crate::agent::job_engine::notify_job_state_changed();
    Ok(body)
}

pub(super) fn launch(db: &DbState, ticket: Ticket, app: Option<tauri::AppHandle>) {
    crate::agent::work_history::bind_async(
        db,
        &ticket.root.session,
        &ticket.run,
        Some(&ticket.original),
    );
    let db = db.clone();
    let rescue_db = db.clone();
    let rescue = ticket.clone();
    let execution = tokio::spawn(answer(db, ticket, app));
    tokio::spawn(async move {
        if let Err(error) = execution.await {
            let output = crate::agent::execution::actions::Output {
                status: "failed".into(),
                error_kind: Some(format!("只读答复执行器退出：{error}")),
                confidence: "unverified".into(),
                ..Default::default()
            };
            let _ = finish(&rescue_db, &rescue, output, vec![], 0);
        }
    });
}

fn answer(
    db: DbState,
    ticket: Ticket,
    app: Option<tauri::AppHandle>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    Box::pin(async move {
        let started = std::time::Instant::now();
        let config = crate::llm::ModelConfig::from_db_state(&db, &ticket.model).map_err(|error|format!("只读答复的模型配置读取失败，未调用模型：{error}。请由负责人检查原成员模型或显式续派。"));
        let interrupt = crate::agent::control::stream_interrupt_channels()
            .lock()
            .await
            .get(&ticket.root.session)
            .map(|s| s.subscribe());
        let setup = config.and_then(|model| {
            interrupt
                .ok_or_else(|| "主任务执行器未连接，答复未启动。".to_string())
                .map(|interrupt| (model, interrupt))
        });
        let (model, interrupt) = match setup {
            Ok(value) => value,
            Err(error) => {
                let _ = finish(
                    &db,
                    &ticket,
                    crate::agent::execution::actions::Output {
                        status: "failed".into(),
                        error_kind: Some(error),
                        confidence: "unverified".into(),
                        ..Default::default()
                    },
                    vec![],
                    0,
                );
                return;
            }
        };
        let admitted = db.0.lock().ok().and_then(|conn| conn.execute("UPDATE agent_child_runs SET status='running' WHERE id=?1 AND status='queued' AND EXISTS(SELECT 1 FROM agent_run_traces WHERE id=?2 AND status IN ('running','waiting_human'))",params![ticket.run,ticket.root.run]).ok()).unwrap_or(0);
        if admitted != 1 {
            let _ = finish(
                &db,
                &ticket,
                crate::agent::execution::actions::Output {
                    status: "cancelled".into(),
                    error_kind: Some("主任务已停止或答复不再处于待启动状态。".into()),
                    confidence: "unverified".into(),
                    ..Default::default()
                },
                vec![],
                0,
            );
            return;
        }
        let prior: String = ticket.prior.chars().take(4000).collect();
        let instruction = if ticket.inbox {
            "你是原成员的一轮只读收信接续。读取宿主投递的消息和原交接，根据内容决定是否用 send_message 直接回复发送者；通知和答复不要求再次回复。仅可 read_file/list_dir/read_message/read_agent_result/send_message/finish。不能修改、执行命令或派发任务。finish 记录本次处理结论，不会自动再发一封回复。每轮输出 JSON 动作；finish 返回 findings/confidence/evidence_refs/missing_evidence。"
        } else {
            "你是一次只读追问的答复者。根据历史交接、收到的问题和必要读取回答，然后 finish。仅可 read_file、list_dir、read_agent_result、read_message、finish。禁止写入、运行命令、派发任务或恢复原实施流程。若需要修改，在 missing_evidence 说明。每轮输出 JSON 动作；finish 返回 findings/confidence/evidence_refs/missing_evidence。"
        };
        let messages = vec![
            json!({"role":"system","content":instruction}),
            json!({"role":"user","content":format!("此前成员编号：{}；原负责人：{}。历史结果节选（可能截断，原文可 read_agent_result）：\n{}",ticket.original,ticket.responsible,prior)}),
        ];
        let session = Session::new(
            db.clone(),
            app,
            ticket.root.session.clone(),
            ticket.run.clone(),
            model,
            "auto".into(),
            interrupt,
            messages,
            Capabilities::Reply {
                workspace: ticket.workspace.clone(),
                root_run: ticket.root.run.clone(),
                can_send: ticket.inbox,
            },
            Profile {
                kind: "worker",
                max_steps: crate::agent::child_worker::MAX_WORKER_STEPS,
                timeout_secs: crate::agent::child_worker::WORKER_TIMEOUT_SECS,
                call_timeout_secs: crate::agent::child_worker::WORKER_PER_CALL_TIMEOUT_SECS,
                output_tokens: 900,
                observation_chars: 40000,
                action_chars: 8000,
                finish_on_budget: true,
                test_command: String::new(),
            },
        );
        let session = crate::agent::execution::actions::execute(session).await;
        if let Err(error) = finish(
            &db,
            &ticket,
            session.output,
            session.messages,
            started.elapsed().as_millis() as u64,
        ) {
            eprintln!("[readonly-reply {}] 结果未提交：{error}", ticket.run);
        }
    })
}

pub(super) fn finish(
    db: &DbState,
    ticket: &Ticket,
    mut output: crate::agent::execution::actions::Output,
    messages: Vec<Value>,
    elapsed: u64,
) -> Result<(), String> {
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    if !root_active_on(&tx, &ticket.root.run)? {
        output.status = "cancelled".into();
        output.error_kind = Some("所属主任务已结束；没有继续执行或修改原成果。".into());
    }
    let result = WorkerResult {
        child_run_id: ticket.run.clone(),
        status: output.status.clone(),
        findings: output.findings,
        confidence: output.confidence,
        evidence_refs: output.evidence_refs,
        missing_evidence: output.missing_evidence,
        duration_ms: elapsed,
        completion_tokens: output.completion_tokens,
        prompt_tokens: output.prompt_tokens,
        error_kind: output.error_kind,
        steps_used: output.steps_used,
        action_log: output.action_log,
    };
    let mut value = serde_json::to_value(&result).map_err(|e| e.to_string())?;
    value["read_only_reply"] = json!(!ticket.inbox);
    value["inbox_continuation"] = json!(ticket.inbox);
    value["requester_run_id"] = json!(ticket.requester.run);
    value["reply_to"] = json!(ticket.original);
    value["request_id"] = json!(ticket.request);
    value["original_responsible_run_id"] = json!(ticket.responsible);
    let history = crate::agent::child_writer::dehydrate_child_messages(&messages);
    let changed=tx.execute("UPDATE agent_child_runs SET status=?2,result_json=?3,message_history_json=?4,finished_at=?5,duration_ms=?6,completion_tokens=?7,prompt_tokens=?8,error_kind=?9 WHERE id=?1 AND status IN ('queued','running')",
        params![ticket.run,result.status,value.to_string(),serde_json::to_string(&history).map_err(|e|e.to_string())?,chrono::Utc::now().to_rfc3339(),elapsed as i64,result.completion_tokens,result.prompt_tokens,result.error_kind]).map_err(|e|e.to_string())?;
    if changed == 1 && !ticket.inbox {
        let from = Actor::new(&ticket.run, &format!("child:{}", ticket.run));
        let payload = json!({"message":"只读追问已结束；这是补充答复，不代表原成果被修改或重新通过验证。","data":value});
        insert_message(
            &tx,
            &format!("{}:answer", ticket.request),
            &from,
            &ticket.requester,
            &ticket.root,
            &payload,
        )?;
        tx.execute(
            "UPDATE agent_messages SET sender_kind='peer_answer' WHERE id=?1",
            [format!("{}:answer", ticket.request)],
        )
        .map_err(|e| e.to_string())?;
        if ticket.requester.run != ticket.root.run {
            insert_message(
                &tx,
                &format!("{}:finished", ticket.request),
                &from,
                &ticket.root,
                &ticket.root,
                &payload,
            )?;
        }
    }
    if changed == 1
        && !ticket.inbox
        && ticket.responsible != ticket.root.run
        && ticket.responsible != ticket.requester.run
    {
        let from = Actor::new(&ticket.run, &format!("child:{}", ticket.run));
        let (responsible_actor, _) = resolve(&tx, &ticket.root, &ticket.responsible)?;
        insert_message(
            &tx,
            &format!("{}:responsible-finished", ticket.request),
            &from,
            &responsible_actor,
            &ticket.root,
            &json!({"message":"所属成员的只读追问已结束；没有修改原交付。","data":value}),
        )?;
    }
    if changed == 1 && ticket.inbox && result.status == "succeeded" {
        // Mail accepted after the last model turn is still outstanding. The
        // next associated run claims it; this old terminal only ends itself.
        tx.execute("UPDATE agent_messages SET claimed_run_id=NULL WHERE claimed_run_id=?1 AND delivered_at IS NULL", [&ticket.run]).map_err(|e|e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    drop(conn);
    super::coordination::kick(db);
    crate::agent::job_engine::notify_job_state_changed();
    Ok(())
}

#[cfg(test)]
mod tests;
