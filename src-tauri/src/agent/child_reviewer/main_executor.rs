//! Review contracts on the ordinary main executor. This adapter owns evidence,
//! read-only tools and result conversion, never a model/tool execution loop.
use super::*;
use crate::agent::{child_task::TaskBinding, execution::runtime::Snapshot};
use tauri::Manager;
tokio::task_local! { static PARENT_RUN: String; }
pub(crate) async fn in_parent_run<F: std::future::Future>(id: &str, future: F) -> F::Output {
    PARENT_RUN.scope(id.into(), future).await
}
pub(crate) fn parent_run() -> Option<String> {
    PARENT_RUN.try_with(Clone::clone).ok()
}

/// Business-state fields can schedule work after tool dispatch (for example a
/// verification contract). A read-only role must not create those side channels.
pub(crate) fn restrict_instruction(
    mut inst: crate::agent::types::AgentInstruction,
) -> crate::agent::types::AgentInstruction {
    let forbidden = inst.review_contract_update.is_some()
        || inst.scope_extension.is_some()
        || inst.request_retriage.is_some()
        || inst.submit_plan.is_some()
        || inst.human_reply_decision.is_some();
    inst.review_contract_update = None;
    inst.scope_extension = None;
    inst.request_retriage = None;
    inst.submit_plan = None;
    inst.human_reply_decision = None;
    inst.change_verification_update = None;
    inst.target_blocks_update = None;
    inst.todo_update.clear();
    if forbidden {
        inst.tool = Some("review_profile_reject".into());
        inst.action = None;
        inst.command = None;
        inst.params = Some(
            json!({"reason":"审查角色不能修改实施契约、申请范围授权、重分流或提交实施方案；本次附带动作未执行。请仅取证或用 finish 交回审查结果。"}),
        );
        inst.parallel = None;
        inst.plan_tree = None;
        inst.commands.clear();
        inst.dispatches = None;
        inst.prepare_finish = None;
        inst.fail_task = None;
    }
    inst
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ReviewProgram {
    pub parent_session: String,
    pub workspace: std::path::PathBuf,
    pub messages: Vec<Value>,
    pub allowed_refs: Vec<String>,
    pub schema: Option<crate::agent::protocol::ContractSchema>,
    pub has_verify_obligation: Option<bool>,
    pub role: Option<Value>,
    pub role_label: Option<Value>,
    pub model: String,
    pub can_finalize: Option<bool>,
    pub evidence_scope_refs: Vec<String>,
    pub final_fingerprint: Option<String>,
    pub verify_obligations: Vec<crate::agent::review_contract::ReviewObligation>,
    #[serde(default)]
    pub evidence_since: Option<chrono::DateTime<chrono::Utc>>,
}

pub(crate) fn load(db: &DbState, id: &str) -> Result<Option<ReviewProgram>, String> {
    use rusqlite::OptionalExtension;
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let exists:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='agent_child_runs')",[],|r|r.get(0)).map_err(|e|e.to_string())?;
    if !exists {
        return Ok(None);
    }
    let raw: Option<String> = conn
        .query_row(
            "SELECT execution_checkpoint_json FROM agent_child_runs WHERE id=?1 AND kind='review'",
            [id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    let Some(raw) = raw else { return Ok(None) };
    let snapshot: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    snapshot["state"]
        .get("main_review")
        .cloned()
        .map(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
        .transpose()
}

pub(crate) fn validate(
    db: &DbState,
    id: &str,
    program: &ReviewProgram,
    payload: &Value,
    elapsed: u64,
) -> Result<Value, String> {
    if let Some(fingerprint) = program.final_fingerprint.as_ref() {
        let mut result = crate::agent::reviewer_gatekeeper::parse_reviewer_json(
            &payload.to_string(),
            &program.model,
            fingerprint,
            elapsed,
        )?;
        let snapshot = Snapshot::load(db, id, "reviewer")?.ok_or("审查执行记录缺失")?;
        let rechecks: std::collections::BTreeMap<
            String,
            crate::agent::host_verify::ObligationVerifyResult,
        > = serde_json::from_value(snapshot.state.get("rechecks").cloned().unwrap_or(json!({})))
            .map_err(|e| e.to_string())?;
        crate::agent::reviewer_gatekeeper::enforce_failed_rechecks(&mut result, &rechecks);
        if snapshot.state["verification_pending"] == true {
            let note =
                "此前宿主复验已发起但结果未确认；本轮未自动重放该命令，不能把它当作已通过的验证。";
            result.missing_evidence.push(note.into());
            result.summary.push_str(&format!("\n{note}"));
        }
        return serde_json::to_value(result).map_err(|e| e.to_string());
    }
    let mut raw = payload.clone();
    if let Some(object) = raw.as_object_mut() {
        for key in ["summary", "details", "artifacts"] {
            if !program
                .schema
                .as_ref()
                .is_some_and(|s| s.fields.iter().any(|f| f.name == key))
            {
                object.remove(key);
            }
        }
    }
    let mut result = build_review_delivery(
        &raw.to_string(),
        ReviewDeliveryContext {
            child_run_id: id,
            allowed_refs: &program.allowed_refs,
            schema: program.schema.as_ref(),
            has_verify_obligation: program.has_verify_obligation,
            kind: Some("review"),
            duration_ms: elapsed,
            completion_tokens: 0,
            prompt_tokens: 0,
            role: program.role.as_ref(),
            role_label: program.role_label.as_ref(),
            model: &program.model,
            plan_review_can_finalize: program.can_finalize,
        },
    )?;
    if let Err(error) = crate::agent::impact_pack::check_evidence_scope_verdicts(
        &program.evidence_scope_refs,
        &result,
    ) {
        let mut snapshot = Snapshot::load(db, id, "reviewer")?.ok_or("审查记录缺失")?;
        let attempts = snapshot.state["coverage_attempts"].as_u64().unwrap_or(0);
        if attempts < 2 {
            snapshot.state["coverage_attempts"] = json!(attempts + 1);
            snapshot.save(db)?;
            return Err(error);
        }
        result["evidence_scope_coverage_incomplete"] = json!(true);
        let note = json!(format!("覆盖缺口（知情重试后仍未补齐）：{error}"));
        if let Some(array) = result["missing_evidence"].as_array_mut() {
            array.push(note);
        }
    }
    Ok(result)
}

pub(crate) fn prepare(db: &DbState, id: &str, program: &ReviewProgram) -> Result<String, String> {
    let mut snapshot = Snapshot::load(db, id, "reviewer")?.ok_or("审查登记检查点缺失")?;
    let resumed: bool =
        db.0.lock()
            .map_err(|e| e.to_string())?
            .query_row(
                "SELECT resumed_from IS NOT NULL FROM agent_child_runs WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
    let mut program = program.clone();
    if resumed {
        // Match run_resumed_review: follow-up evidence is not restricted to the
        // first dispatch's output-reference list. Keep the schema, read-only
        // permissions, evidence time boundary and original review obligations.
        program.allowed_refs.clear();
        program.messages.push(json!({"role":"user","content":"【本轮审查续接】可以引用本轮对质补充或通过只读取证取得的新证据；首次派发的引用列表不限制本轮 evidence_refs。原有来源归属、任务时间边界、结果字段契约和只读权限继续适用。"}));
    }
    snapshot.state["main_review"] = serde_json::to_value(&program).map_err(|e| e.to_string())?;
    snapshot.save(db)?;
    let mut config = crate::agent::execution::runtime::config(db, id)?;
    config.workflow.plan_mode = None;
    config.workflow.completion = Some("handoff".into());
    // Handoff resolves the ordinary executor's fixed stages to off; keep the
    // persisted reviewer configuration valid for failover and legacy adapters.
    config.stages = Default::default();
    db.0.lock()
        .map_err(|e| e.to_string())?
        .execute(
            "UPDATE agent_child_runs SET execution_config_json=?2 WHERE id=?1",
            params![
                id,
                serde_json::to_string(&config).map_err(|e| e.to_string())?
            ],
        )
        .map_err(|e| e.to_string())?;
    Ok(crate::agent::task_context::review_goal(&program.messages))
}

#[cfg(test)]
mod tests;

pub(crate) fn start(
    app: AppHandle,
    db: &DbState,
    id: &str,
    parent_run: &str,
    model: &str,
    effort: String,
    program: ReviewProgram,
    interrupt: watch::Receiver<u64>,
    progress_member_id: Option<&str>,
) -> Result<(), String> {
    let managed = app
        .try_state::<DbState>()
        .ok_or("通用审查执行器缺少数据库宿主；未启动。")?;
    if !std::sync::Arc::ptr_eq(&managed.0, &db.0) {
        return Err("通用审查执行器的数据库归属不一致；未启动。".into());
    }
    if app
        .try_state::<std::sync::Arc<tokio::sync::Mutex<crate::agent::mcp::PluginRegistry>>>()
        .is_none()
    {
        return Err("通用审查执行器缺少工具注册表宿主；未启动。".into());
    }
    drop(managed);
    let goal = prepare(db, id, &program)?;
    let registration = crate::agent::review_control::register(
        db,
        &program.parent_session,
        id,
        model,
        true,
        interrupt,
    )?;
    let mut cancellation = registration.interrupt();
    let mut routing = crate::agent::router::ModelRoutingConfig::default();
    crate::agent::execution::runtime::bind_primary_model(&mut routing, model);
    routing.pro_reasoning_effort = effort.clone();
    routing.flash_reasoning_effort = effort;
    let binding = TaskBinding {
        parent_session: program.parent_session.clone(),
        parent_run: parent_run.into(),
        child_session: format!("task-{id}"),
        child_run: id.into(),
    };
    crate::agent::child_task::runtime::spawn(
        app,
        db.clone(),
        binding,
        routing,
        goal,
        program.workspace,
        true,
        "auto".into(),
        None,
        Some(crate::agent::child_task::runtime::ReviewRelay {
            member_id: progress_member_id.map(str::to_owned),
            label: program.role_label.as_ref().and_then(Value::as_str).map(str::to_owned),
            model: model.to_owned(),
        }),
    )?;
    let id = id.to_string();
    let db = db.clone();
    tokio::spawn(async move {
        loop {
            if !crate::agent::child_task::runtime::attached(&id) {
                break;
            }
            tokio::select! {
                changed=cancellation.changed()=>{if changed.is_err(){break;}let _=crate::agent::child_task::runtime::request_cancel(&id);},
                _=tokio::time::sleep(Duration::from_millis(50))=>{}
            }
        }
        if !registration.complete() {
            if let Ok(conn) = db.0.lock() {
                let _ = crate::agent::review_control::cancel_child_on(&conn, &id);
            }
        }
    });
    Ok(())
}

fn reserve_tool_step(
    db: &DbState,
    id: &str,
    name: &str,
    action: &str,
    params: &Value,
) -> Result<Snapshot, String> {
    if !params.is_object() {
        return Err("审查工具参数必须是 JSON 对象；本次未执行，未消耗取证预算。请使用 {\"tool\":\"工具名\",\"params\":{...}}。".into());
    }
    if name.contains('/') {
        // A qualified MCP name identifies the entire tool. A model-supplied
        // action or argument cannot narrow its remote capabilities.
        if !crate::agent::execution::runtime::config(db, id)?.permits(&name.to_ascii_lowercase()) {
            return Err(format!("执行配置未允许 MCP 工具 {name}；本次未执行。"));
        }
    } else {
        crate::agent::execution::runtime::authorize_call(db, id, name, action, params)?;
    }
    let mut snapshot = Snapshot::load(db, id, "reviewer")?.ok_or("审查执行检查点缺失")?;
    let used = snapshot.state["tool_steps"].as_u64().unwrap_or(0);
    let budget = u64::from(crate::agent::review_tools::forensic_budget());
    if used >= budget {
        return Err(format!(
            "只读取证预算已用尽（{used}/{budget}），本次未执行；请用 finish 提交完整审查结果。"
        ));
    }
    snapshot.state["tool_steps"] = json!(used + 1);
    snapshot.save(db)?;
    Ok(snapshot)
}

fn read_tool(
    db: &DbState,
    id: &str,
    program: &ReviewProgram,
    name: &str,
    mut params: Value,
) -> Result<String, String> {
    let name = if name == "get_step_output" { "read_step_output" } else { name };
    let parent: String = db.reader()?.query_row(
        "SELECT parent_run_id FROM agent_child_runs WHERE id=?1", [id], |r| r.get(0),
    ).map_err(|e|e.to_string())?;
    if name == "list_task_tree" {
        for key in ["offset", "limit"] {
            if params.get(key).is_some_and(|v| v.as_u64().is_none()) {
                return Err(format!("{key} 必须是非负整数；未执行查询"));
            }
        }
        let actor = crate::agent::agent_mailbox::Actor::new(id, &format!("task-{id}"));
        let mut overview = crate::agent::agent_mailbox::family_overview(
            db, &actor, params["offset"].as_u64().unwrap_or(0), params["limit"].as_u64().unwrap_or(30).clamp(1, 100),
        )?;
        if let Some(tasks) = overview["tasks"].as_array_mut() {
            for task in tasks {
                if let Some(row) = task.as_object_mut() {
                    row.remove("execution_config");
                    row.remove("checkpoint");
                }
            }
        }
        overview["scope_notice"] = json!("同一任务树的只读概览；详细步骤与结果仍按本次审查范围授权。");
        return Ok(overview.to_string());
    }
    if matches!(name, "list_execution_steps" | "list_child_runs" | "read_child_result") {
        return crate::agent::reviewer_gatekeeper::review_history_tool(db, &parent, name, &params);
    }
    params["tool"] = json!(name);
    if matches!(
        name,
        "read_step_output"
            | "search_step_output"
            | "evidence_find"
            | "evidence_inspect"
            | "evidence_compare"
    ) {
        let since = program
            .evidence_since
            .or(
                crate::agent::checkpoint::load_checkpoint(db, &program.parent_session)?
                    .and_then(|cp| cp.task_started_at),
            )
            .ok_or("未取得父任务时间边界，无法核对证据范围。")?;
        if params.get("run_id").is_some_and(|v|v.as_str().is_none_or(|s|s.trim().is_empty())) {
            return Err("run_id 必须是非空字符串；未读取回执。".into());
        }
        let evidence_session = if let Some(run) = params["run_id"].as_str() {
            crate::agent::reviewer_gatekeeper::review_run_session(db, &parent, run)?
        } else { program.parent_session.clone() };
        return match name {
            "read_step_output" => crate::agent::reviewer_gatekeeper::read_review_step_output(
                db,
                &evidence_session,
                &params,
                since,
            ),
            "search_step_output" => {
                crate::agent::reviewer_gatekeeper::search_review_step_output(
                    db,
                    &evidence_session,
                    &params,
                    since,
                )
            }
            _ => crate::agent::reviewer_gatekeeper::read_registered_evidence(
                db,
                &evidence_session,
                name,
                &params,
                &program.workspace,
                since,
            ),
        };
    }
    if name == "git_diff" {
        if let Some(baseline) = crate::agent::reviewer_gatekeeper::GitBaseline::git_for_session(
            &program.parent_session,
        ) {
            return Ok(baseline
                .collect_git_diff(&program.workspace)
                .map(|(diff, _)| diff)
                .unwrap_or_else(|| "任务起点以来无 Git 改动".into()));
        }
    }
    crate::agent::review_tools::execute(
        &program.workspace,
        Some(&crate::agent::path_guard::get_session_tmp_dir(
            &program.parent_session,
        )),
        Some(&program.parent_session),
        name,
        &params,
    )
}

pub(crate) mod mcp_tools;
mod parallel;

async fn invoke_read_tool(
    db: &DbState,
    id: &str,
    program: &ReviewProgram,
    registry: &tokio::sync::Mutex<crate::agent::mcp::PluginRegistry>,
    name: &str,
    action: &str,
    params: Value,
    interrupt: &watch::Receiver<u64>,
) -> Result<String, String> {
    if interrupt.has_changed().unwrap_or(true) {
        return Err("审查已收到停止信号，本次取证未执行。".into());
    }
    let plugin = if name.contains('/') {
        Some(mcp_tools::resolve(db, registry, name).await?)
    } else {
        None
    };
    // Charge on the parent task before starting I/O. Workers do not write the checkpoint.
    reserve_tool_step(db, id, name, action, &params)?;
    if let Some((client, tool)) = plugin {
        if interrupt.has_changed().unwrap_or(true) {
            return Err("审查已收到停止信号，本次取证未执行。".into());
        }
        return mcp_tools::call(&client, &format!("task-{id}"), &tool, params).await;
    }
    let (db, id, program, name, interrupt) = (
        db.clone(), id.to_owned(), program.clone(), name.to_owned(), interrupt.clone(),
    );
    tokio::task::spawn_blocking(move || {
        if interrupt.has_changed().unwrap_or(true) {
            return Err("审查已收到停止信号，本次取证未执行。".into());
        }
        read_tool(&db, &id, &program, &name, params)
    }).await.map_err(|error| format!("审查取证未返回结果：{error}"))?
}


pub(crate) async fn tool(
    db: &DbState,
    id: &str,
    program: &ReviewProgram,
    inst: &crate::agent::types::AgentInstruction,
    registry: &tokio::sync::Mutex<crate::agent::mcp::PluginRegistry>,
    interrupt: &mut watch::Receiver<u64>,
) -> crate::agent::types::DispatchResult {
    if let Some(branches) = &inst.parallel {
        return parallel::run(db, id, program, branches, registry, interrupt).await;
    }
    let result: Result<String, String> = async {
        let requested_name = inst.get_tool();
        let params = inst.get_params();
        let action = inst.get_action();
        if requested_name == "verify" && program.final_fingerprint.is_some() {
            let snapshot = reserve_tool_step(db, id, &requested_name, &action, &params)?;
            if snapshot.state["verification_pending"] == true {
                return Err(
                    "此前宿主复验的结果未知，未自动重跑；请核对已有证据并披露未确认事项。".into(),
                );
            }
            static VERIFY_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
            let target: Vec<_> = program
                .verify_obligations
                .iter()
                .filter(|o| params["id"].as_str().is_none_or(|id| id == o.id))
                .cloned()
                .collect();
            if target.is_empty() {
                return Err("未找到可执行的已绑定验证义务；未执行。".into());
            }
            let generation = *interrupt.borrow();
            if interrupt.has_changed().unwrap_or(true) {
                return Err("审查已收到停止信号，未执行新的复验。".into());
            }
            let _guard = tokio::select! {
                _=interrupt.changed()=>return Err("等待复验时收到停止信号，未执行。".into()),
                guard=VERIFY_LOCK.lock()=>guard,
            };
            if *interrupt.borrow() != generation {
                return Err("审查已停止，未执行新的复验。".into());
            }
            let mut pending = Snapshot::load(db, id, "reviewer")?.ok_or("审查执行记录缺失")?;
            pending.state["verification_pending"] = json!(true);
            pending.save(db)?;
            let results = crate::agent::host_verify::run_obligation_verifications(
                &program.workspace,
                &target,
            )
            .await;
            let mut snapshot = Snapshot::load(db, id, "reviewer")?.ok_or("审查记录缺失")?;
            if !snapshot.state["rechecks"].is_object() {
                snapshot.state["rechecks"] = json!({});
            }
            for result in &results {
                snapshot.state["rechecks"][&result.obligation_id] =
                    serde_json::to_value(result).map_err(|e| e.to_string())?;
            }
            snapshot.state["verification_pending"] = json!(false);
            snapshot.save(db)?;
            return serde_json::to_string(&results).map_err(|e| e.to_string());
        }
        invoke_read_tool(db, id, program, registry, &requested_name, &action, params, interrupt).await
    }
    .await;
    match result {
        Ok(stdout) => crate::agent::types::DispatchResult {
            stdout,
            stderr: String::new(),
            success: true,
            route: "review:read_only_tool".into(),
            view: None,
        },
        Err(stderr) => crate::agent::types::DispatchResult {
            stdout: String::new(),
            stderr,
            success: false,
            route: "review:tool_rejected".into(),
            view: None,
        },
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_final(
    app: AppHandle,
    db: &DbState,
    parent: &str,
    session: &str,
    workspace: &Path,
    config: &ModelConfig,
    messages: &[Value],
    fingerprint: &str,
    obligations: Vec<crate::agent::review_contract::ReviewObligation>,
    evidence_since: chrono::DateTime<chrono::Utc>,
    mut interrupt: watch::Receiver<u64>,
    progress_member_id: &str,
) -> Result<crate::agent::reviewer_gatekeeper::ReviewResult, String> {
    let id = begin_child_run_extended(
        db,
        parent,
        session,
        workspace,
        &config.model_id,
        0,
        "review",
        Some(3),
        None,
        None,
        &ChildRunIdentity::fresh("final_review"),
    )?;
    let program = ReviewProgram {
        parent_session: session.into(),
        workspace: workspace.into(),
        messages: messages.into(),
        allowed_refs: vec![],
        schema: None,
        has_verify_obligation: None,
        role: None,
        role_label: None,
        model: config.model_id.clone(),
        can_finalize: None,
        evidence_scope_refs: vec![],
        final_fingerprint: Some(fingerprint.into()),
        verify_obligations: obligations,
        evidence_since: Some(evidence_since),
    };
    if let Err(error) = start(
        app,
        db,
        &id,
        parent,
        &config.model_id,
        "auto".into(),
        program,
        interrupt.clone(),
        Some(progress_member_id),
    ) {
        crate::agent::execution::runtime::fail_checkpoint(db, &id, error.clone());
        return Err(error);
    }
    let mut interrupt_open = true;
    while crate::agent::child_task::runtime::attached(&id) {
        tokio::select! {
            _=tokio::time::sleep(Duration::from_millis(50))=>{},
            changed=interrupt.changed(),if interrupt_open=>{
                interrupt_open=changed.is_ok();
                let _=crate::agent::child_task::runtime::request_cancel(&id);
            }
        }
    }
    let (status, raw): (String, Option<String>) = db
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .query_row(
            "SELECT status,result_json FROM agent_child_runs WHERE id=?1 AND parent_session_id=?2",
            params![id, session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    // Inline final-review transport consumes its own receipt. Parent mailbox must
    // not reinterpret a final ReviewResult as a child findings contract.
    db.0.lock().map_err(|e|e.to_string())?.execute("UPDATE agent_child_runs SET consumed_at=COALESCE(consumed_at,?2),applied_at=COALESCE(applied_at,?2) WHERE id=?1",params![id,chrono::Utc::now().to_rfc3339()]).map_err(|e|e.to_string())?;
    if status != "succeeded" {
        return Err(format!(
            "通用终审执行未成功（{status}）；未获得有效审查结论。"
        ));
    }
    serde_json::from_str(raw.as_deref().ok_or("终审回执缺失")?).map_err(|e| e.to_string())
}

pub(crate) fn settle(
    db: &DbState,
    binding: &TaskBinding,
    outcome: &Result<(), String>,
    elapsed: u64,
) -> Result<Value, String> {
    let program = load(db, &binding.child_run)?.ok_or("审查配置缺失")?;
    let cp = crate::agent::checkpoint::load_checkpoint(db, &binding.child_session)?;
    use rusqlite::OptionalExtension;
    let terminal: Option<(String, Option<String>)> =
        db.0.lock()
            .map_err(|e| e.to_string())?
            .query_row(
                "SELECT status,completion_json FROM agent_run_traces WHERE id=?1 AND session_id=?2",
                params![binding.child_run, binding.child_session],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
    let completion = terminal
        .as_ref()
        .and_then(|(_, raw)| raw.as_deref())
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
    let prompt_tokens = cp
        .as_ref()
        .map(|c| c.data.run_prompt_tokens as i64)
        .unwrap_or(0);
    let completion_tokens = cp
        .as_ref()
        .map(|c| c.data.run_completion_tokens as i64)
        .unwrap_or(0);
    let result = cp
        .as_ref()
        .filter(|cp| cp.phase == crate::agent::checkpoint::AgentPhase::Done)
        .and_then(|cp| cp.pending_finalization.as_ref())
        .filter(|r| {
            r.is_ready()
                && r.handoff
                && !r.acceptance_fingerprint.is_empty()
                && terminal
                    .as_ref()
                    .is_some_and(|(status, _)| status == "succeeded")
                && completion
                    .as_ref()
                    .and_then(|c| c["finalization_receipt"].as_str())
                    == Some(r.receipt_id.as_str())
        })
        .ok_or_else(|| {
            outcome
                .as_ref()
                .err()
                .cloned()
                .unwrap_or_else(|| "审查执行未取得正式终态收据".into())
        })
        .and_then(|receipt| {
            validate(
                db,
                &binding.child_run,
                &program,
                &receipt.finish_payload,
                elapsed,
            )
        });
    let (status, mut body, error) = match result {
        Ok(value) => ("succeeded", value, None),
        Err(error) => {
            let kind = ChildErrorKind::Other(error.clone());
            (
                "failed",
                serde_json::from_str(&child_error_result_json(
                    &binding.child_run,
                    "failed",
                    &kind,
                    elapsed,
                    0,
                    0,
                    Some(&error),
                ))
                .map_err(|e| e.to_string())?,
                Some(kind),
            )
        }
    };
    if program.final_fingerprint.is_none() {
        body["prompt_tokens"] = json!(prompt_tokens);
        body["completion_tokens"] = json!(completion_tokens);
    }
    let mut history = program.messages.clone();
    if let Some(cp) = cp {
        for msg in cp.data.turns_history {
            if msg.role == "system"
                || msg
                    .content
                    .as_str()
                    .is_some_and(|s| s.contains("【宿主审查任务与历史】"))
            {
                continue;
            }
            history.push(json!({"role":msg.role,"content":msg.content}));
        }
    }
    finish_child_run_with_history(
        db,
        &binding.child_run,
        status,
        elapsed,
        completion_tokens,
        prompt_tokens,
        error,
        Some(&body.to_string()),
        Some(&serde_json::to_string(&history).map_err(|e| e.to_string())?),
    );
    Ok(body)
}
