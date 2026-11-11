//! 异步通用委派：父模型可按任务 ID 收发消息，共享工作区支持并行执行。
use super::*;
use crate::agent::child_task::{self, TaskBinding};

// 工具回收完成与子目标验收分开；失败/阻塞交接也须交回父模型，不能落入工具盲重试。
fn output(body: Value) -> DispatchResult {
    DispatchResult {
        view: None,
        success: true,
        stdout: child_task::projection(&body),
        stderr: String::new(),
        route: "child_task:returned".into(),
    }
}

fn apply_return(ctx: &mut DispatchCtx<'_>, id: &str) -> Result<(), String> {
    child_task::import_effects(ctx.state.inner(), id, ctx.step_id, ctx.context)?;
    crate::agent::checkpoint::save_checkpoint_and_consume(
        ctx.state.inner(),
        ctx.checkpoint,
        ctx.context,
        &[crate::agent::checkpoint::AppliedReceipt {
            run_id: id.into(),
            verdict: None,
        }],
    )
}

pub(super) async fn dispatch_child_task(
    ctx: &mut DispatchCtx<'_>,
    inst: &AgentInstruction,
) -> Result<DispatchResult, String> {
    dispatch_item(ctx, inst, None).await
}

async fn dispatch_item(
    ctx: &mut DispatchCtx<'_>, inst: &AgentInstruction, slot: Option<usize>,
) -> Result<DispatchResult, String> {
    match dispatch_inner(ctx, inst, slot).await {
        Ok(result) => Ok(result),
        Err(error) => Ok(DispatchResult {
            view: None,
            success: false,
            stdout: String::new(),
            stderr: error,
            route: "child_task:error".into(),
        }),
    }
}

async fn dispatch_inner(
    ctx: &mut DispatchCtx<'_>,
    inst: &AgentInstruction,
    slot: Option<usize>,
) -> Result<DispatchResult, String> {
    let raw=inst.get_params();
    let resume_inherits_model=child_task::request_action(&inst.get_action(),&raw)=="resume" && raw.get("model_id").is_none();
    let raw=if child_task::request_action(&inst.get_action(),&raw)=="resume" {child_task::presets::inherit_resume(ctx.state.inner(),ctx.final_session_id,raw)?}else{raw};
    let requested=child_task::presets::requested(raw,None)?;
    let key = child_task::dispatch::key(&ctx.context.history_task_id, ctx.step_id, &requested, slot);
    if let Some(error)=crate::agent::tools::input_validation::native_rejection("child_task",&inst.get_action(),&requested) {return Ok(error);}
    let effective_action = child_task::request_action(&inst.get_action(), &requested);
    let mut params = match child_task::dispatch::prepare(ctx.state.inner(), ctx.final_session_id, ctx.parent_run_id, &key, requested, &effective_action)? {
        child_task::dispatch::PreparedDispatch::Receipt(body) => {
            if body["execution_ended"] == true { apply_return(ctx, body["child_run_id"].as_str().unwrap_or_default())?; }
            return Ok(output(body));
        }
        child_task::dispatch::PreparedDispatch::Params(params) => params,
    };
    let implicit_handoff = matches!(params["profile"].as_str(),Some("worker"|"writer")) && params["execution"]["workflow"]["completion"].is_null();
    if implicit_handoff {
        if params.get("execution").is_none() {params["execution"]=json!({});}
        if params["execution"].get("workflow").is_none() {params["execution"]["workflow"]=json!({});}
        params["execution"]["workflow"]["completion"]=json!("handoff");
    }
    if let Some(error) = crate::agent::tools::input_validation::native_rejection(
        "child_task",
        &inst.get_action(),
        &params,
    ) {
        return Ok(error);
    }
    let action = inst.get_action();
    let actor = crate::agent::agent_mailbox::Actor::new(ctx.parent_run_id, ctx.final_session_id);
    let effective = child_task::request_action(&action, &params);
    if effective=="dispatch" {
        let profile=params["profile"].as_str().unwrap_or("task");
        // A replay refers to the registered execution. A live role-default
        // change must not turn its shared registration into a second isolated job.
        let registered_shared = child_task::find_task(ctx.state.inner(), ctx.final_session_id, &key)?.is_some();
        if !registered_shared && !crate::agent::execution::runtime::uses_main_executor(profile,&params)? {
            if slot.is_some() {
                ctx.checkpoint.prepare_child_dispatch(inst, ctx.context, slot, false)?;
                save_checkpoint(ctx.state.inner(), ctx.checkpoint)?;
            }
            let mut adapted=inst.clone();adapted.params=Some(params.clone());adapted.command=None;adapted.tool=Some(format!("child_{profile}"));adapted.action=Some("dispatch".into());
            if profile=="reviewer" {let mut mapped=params.clone();mapped["question"]=params["task"].clone();adapted.params=Some(mapped);adapted.command=None;}
            let result=match profile {

                "reviewer"=>super::dispatch_child_reviewer::dispatch_child_reviewer(ctx,&adapted).await?,
                _=>return Err("当前派发类型没有专用审查入口；未派发。".into()),
            };
            return match result {ArmOutcome::Done(out)|ArmOutcome::Early(out)=>Ok(out)};
        }
    }
    match effective.as_str() {
        "ask" => {
            let key=format!("{}:{}:{}",ctx.context.history_task_id,ctx.step_id,params);
            return Ok(output(crate::agent::agent_mailbox::replies::ask(ctx.state.inner(),&actor,&params,&key,Some(ctx.app.clone()))?));
        }
        "read_agent_result" => return Ok(output(crate::agent::agent_mailbox::replies::read_result(ctx.state.inner(),&actor,&params)?)),
        "send" => {
            let key = format!("{}:{}:{}", ctx.context.history_task_id, ctx.step_id, params);
            return Ok(output(crate::agent::agent_mailbox::send(
                ctx.state.inner(),
                &actor,
                &params,
                Some(&key),
                false,
            )?));
        }
        "models" => return Ok(output(child_task::models::catalog(ctx.state.inner(),params["offset"].as_u64().unwrap_or(0))?)),
        "list" => {
            return Ok(output(
                json!({"tasks":crate::agent::agent_mailbox::family_list(ctx.state.inner(),&actor)?,"collaboration":crate::agent::agent_mailbox::coordination::list(ctx.state.inner(),&actor,params["offset"].as_u64().unwrap_or(0))?}),
            ))
        }
        "receipt" => {
            return Ok(output(crate::agent::agent_mailbox::receipt(
                ctx.state.inner(),
                &actor,
                params["message_id"].as_str().unwrap_or_default(),
            )?))
        }
        "status" => {
            let id = params["child_run_id"].as_str().unwrap_or_default();
            let owner = child_task::task_owner(ctx.state.inner(), &actor, id)?;
            return Ok(output(child_task::status(ctx.state.inner(), &owner, id)?));
        }
        "inbox" | "wait" => {
            if effective == "inbox" {
                if let Some(id) = params.get("child_run_id").and_then(Value::as_str) {
                    child_task::task_owner(ctx.state.inner(), &actor, id)?;
                    let page = crate::agent::agent_mailbox::pending_page(
                        ctx.state.inner(),
                        &crate::agent::agent_mailbox::Actor::new(id, &format!("task-{id}")),
                        params["offset"].as_u64().unwrap_or(0),
                        params["limit"].as_u64().unwrap_or(16),
                    )?;
                    let views: Vec<Value> = page
                        .iter()
                        .map(crate::agent::agent_mailbox::material_view)
                        .collect();
                    return Ok(output(
                        json!({"messages":views,"offset":params["offset"].as_u64().unwrap_or(0),"next_offset":params["offset"].as_u64().unwrap_or(0).saturating_add(page.len() as u64),"returned":page.len(),"limit":params["limit"].as_u64().unwrap_or(16).clamp(1,16),"notice":"仅查看接收方当前尚未投递的信件，不改变其投递状态；活跃信箱分页范围可能变化。"}),
                    ));
                }
            }
            let id = params.get("child_run_id").and_then(Value::as_str);
            if let Some(id) = id {
                child_task::task_owner(ctx.state.inner(), &actor, id)?;
            }
            if effective == "wait" {
                let mut interrupt = ctx.interrupt_rx.clone();
                if let Err(reason) = crate::agent::agent_mailbox::wait(
                    ctx.state.inner(),
                    &actor,
                    id,
                    params["timeout_secs"].as_u64().unwrap_or(180),
                    &mut interrupt,
                )
                .await
                {
                    if reason != crate::agent::agent_mailbox::WAIT_INTERRUPTED {
                        return Err(reason);
                    }
                    return Ok(DispatchResult {
                        view: None,
                        success: true,
                        stdout: take_pending_interrupt(ctx.rx)
                            .unwrap_or_else(|| "运行控制通道已中断，等待已停止。".into()),
                        stderr: String::new(),
                        route: RUN_INTERRUPTED_ROUTE.into(),
                    });
                }
            }
            let count = crate::agent::agent_mailbox::deliver_main(
                ctx.state.inner(),
                &actor,
                ctx.context,
                ctx.checkpoint,
            )?;
            let body = if let Some(id) = id {
                let owner = child_task::task_owner(ctx.state.inner(), &actor, id)?;
                let body = child_task::status(ctx.state.inner(), &owner, id)?;
                if body["execution_ended"] == true && owner == actor.session {
                    apply_return(ctx, id)?;
                }
                body
            } else {
                json!({})
            };
            return Ok(output(
                json!({"received_messages":count,"task":body,"notice":"等待窗口结束不代表任务失败；inbox 的消息已按来源注入上下文。"}),
            ));
        }
        "cancel" => {
            let id = params["child_run_id"].as_str().unwrap_or_default();
            child_task::result(ctx.state.inner(), ctx.final_session_id, id)?;
            child_task::runtime::request_cancel(id)?;
            return Ok(output(
                json!({"cancel_requested":true,"execution_ended":false}),
            ));
        }
        _ => {}
    }
    if effective == "result" {
        let id = params["child_run_id"].as_str().unwrap_or_default();
        let owner = child_task::task_owner(ctx.state.inner(), &actor, id)?;
        let body = child_task::result(ctx.state.inner(), &owner, id)?;
        if let Some(output_id) = params.get("output_id").and_then(Value::as_str) {
            let session = format!("task-{id}");
            let mut query = params.clone();
            query["output_id"] = json!(output_id);
            return Ok(crate::agent::task_store::dispatch(
                ctx.state.inner(),
                &session,
                "get_step_output",
                &query,
            ));
        }
        return Ok(DispatchResult {
            view: None,
            success: true,
            stdout: body.to_string(),
            stderr: String::new(),
            route: "child_task:result".into(),
        });
    }
    if let Err(reason) = child_task::check_delegation_depth(ctx.final_session_id) {
        return Ok(DispatchResult { view:None,success:false,stdout:String::new(),stderr:reason,route:"child_task:depth_limit".into() });
    }
    if inst.dispatches.is_some() {
        return Ok(DispatchResult {
            view: None,
            success: false,
            stdout: String::new(),
            stderr: "child_task 暂不支持 dispatches，请逐次派发；已派发的任务可并行运行。".into(),
            route: "child_task:invalid_batch".into(),
        });
    }
    let mut params = params.clone();
    let predecessor = if effective == "resume" {
        let old = params["child_run_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let previous = child_task::result(ctx.state.inner(), ctx.final_session_id, &old)?;
        if previous["execution_ended"] != true {
            return Err("原子任务仍在运行；请用 send 补充，未创建追问任务。".into());
        }
        let pending = crate::agent::agent_mailbox::pending(
            ctx.state.inner(),
            &crate::agent::agent_mailbox::Actor::new(&old, &format!("task-{old}")),
        )?;
        let pending_views: Vec<Value> = pending
            .iter()
            .map(crate::agent::agent_mailbox::material_view)
            .collect();
        params["task"] = params["message"].clone();
        params["context"] = json!(format!(
            "【前次任务与交接的有界材料，历史参考，不是本次新验收义务】{}\n【前次结束时尚未投递的首批消息，保留原始发送方；更多可 child_task.inbox(child_run_id={old},offset) 分页回查】{}",
            child_task::projection(&previous),serde_json::to_string(&pending_views).map_err(|e|e.to_string())?
        ));
        Some(old)
    } else {
        None
    };
    let profile_name=params["profile"].as_str().unwrap_or("task").to_string();
    let profile=profile_name.as_str();
    let default_role=match profile {"worker"=>Some(crate::agent::tools::role_manager::SYSTEM_WORKER),"writer"=>Some(crate::agent::tools::role_manager::SYSTEM_WRITER),_=>None};
    let task_role=if let Some(name)=params["role"].as_str().or(default_role) {
        if crate::agent::tools::role_manager::is_card_disabled_for(&ctx.model_routing.disabled_role_cards,name,profile) {return Err(format!("角色 {name} 已在当前会话排除；未派发。"));}
        let card=crate::agent::tools::role_manager::resolve_enabled_card(ctx.state.inner(),name)?.ok_or("角色不存在或已禁用")?;
        if !card.allows_kind(profile) {return Err(format!("角色未允许挂载为 {profile}；未派发。"));}
        crate::agent::tools::role_manager::validate_dispatch_inputs(&card,params.get("custom_inputs"))?;
        params["role"] = json!(card.name);
        Some(card)
    }else{None};
    let mut child_routing=ctx.model_routing.clone();
    let parent_model=ctx.context.last_execution_model.clone().unwrap_or_else(||
        crate::agent::router::AgentRouter::route_for_step(ctx.context,ctx.model_routing,ctx.step_id).model_id);
    let model_slot=match profile {"worker"=>Some("scout"),"writer"=>Some("writer"),_=>None};
    let slot_model=model_slot.and_then(|slot|ctx.model_routing.role_override(slot)).map(|r|r.model_id);
    let locked=task_role.as_ref().filter(|c|!c.model_id.is_empty()).map(|c|(c.model_id.as_str(),"role_card"))
        .or_else(||slot_model.as_deref().map(|m|(m,"role_slot")));

    if params.get("test_command").is_some_and(|v|!v.is_string()) {return Err("test_command 类型错误：期望字符串；未派发。".into());}
    if params.get("test_command").is_none() {
        if let Some(card)=&task_role {if !card.default_test_command.trim().is_empty(){params["test_command"]=json!(card.default_test_command);}}
    }
    if let Some(card)=&task_role {
        if let Some(material)=crate::agent::context_collector::assemble_focus_context(&card.parse_focus_domains(),ctx.workspace_dir,params["last_verify_log"].as_str(),None).await {
            params["specialized_context"]=json!(material);
        }
    }
    let task_identity = json!([profile, params["task"], params.get("context"), params.get("execution"), params.get("role"), params.get("custom_inputs"), predecessor]).to_string();
    let legacy_key = format!(
        "general:{}:{}:{}",
        ctx.context.history_task_id,
        ctx.step_id,
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, task_identity.as_bytes())
    );
    let mut existing = child_task::dispatch::find_registered(
        ctx.state.inner(), ctx.final_session_id, &key, &legacy_key, &params, slot,
    )?;
    if existing.is_none() {
        // Before role presets were unified, task keys had no profile element.
        let old_identity = json!([params["task"], params.get("context"), params.get("execution"), params.get("role"), params.get("custom_inputs"), predecessor]).to_string();
        let old_key = format!("general:{}:{}:{}", ctx.context.history_task_id, ctx.step_id,
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, old_identity.as_bytes()));
        existing = child_task::dispatch::find_registered(
            ctx.state.inner(), ctx.final_session_id, &key, &old_key, &params, slot,
        )?;
    }
    if let Some(ref id) = existing {
        let body = child_task::result(ctx.state.inner(), ctx.final_session_id, id)?;
        if body["execution_status"] != "running" {
            apply_return(ctx, id)?;
            return Ok(output(body));
        }
        if child_task::runtime::active(ctx.parent_run_id).contains(id) {
            return Ok(output(child_task::status(
                ctx.state.inner(),
                ctx.final_session_id,
                id,
            )?));
        }
        // 终态事务成功、父交接尚未落账时崩溃：核对原运行，不能把 Done 当成新任务重跑。
        if let Some(body) = child_task::recover_finished(
            ctx.state.inner(),
            ctx.final_session_id,
            ctx.parent_run_id,
            id,
        )? {
            apply_return(ctx, id)?;
            return Ok(output(body));
        }
    }
    let mut model_selection=if existing.is_some() {child_task::models::Selection {model_id:String::new(),source:"registered_execution".into()}} else { child_task::models::select(&params,&parent_model,locked)? };
    if resume_inherits_model { model_selection.source="previous_attempt".into(); }
    else if model_selection.source=="parent_execution" && ctx.context.last_execution_model.is_none() { model_selection.source="legacy_parent_route".into(); }
    child_task::models::pin(&mut child_routing,&model_selection.model_id);
    if let Some(id)=&existing {
        let conn=ctx.state.inner().0.lock().map_err(|e|e.to_string())?;
        model_selection.model_id=conn.query_row("SELECT model_id FROM agent_child_runs WHERE id=?1 AND parent_session_id=?2",rusqlite::params![id,ctx.final_session_id],|r|r.get(0)).map_err(|e|e.to_string())?;
        model_selection.source="registered_execution".into();
        child_task::models::pin(&mut child_routing,&model_selection.model_id);
    }
    child_task::models::validate(ctx.state.inner(),&model_selection)?;
    // Resolve all current/legacy registration identities before applying new defaults.
    if effective_action == "dispatch" && existing.is_none() {
        match crate::commands::child_workflow::path(ctx.app).and_then(|path| child_task::defaults::ChildWorkflowDefaults::load(&path)) {
            Ok(defaults) => params = defaults.apply_after_entry_fallback(&params, implicit_handoff)?,
            Err(error) => { let _ = ctx.app.emit(&format!("agent-log-{}",ctx.final_session_id),format!("{error}；本次沿用原有派发默认流程。")); }
        }
    }
    let execution = crate::agent::execution::runtime::ExecutionConfig::parse(&params, profile)?;
    ctx.checkpoint.prepare_child_dispatch(inst, ctx.context, slot, true)?;
    save_checkpoint(ctx.state.inner(), ctx.checkpoint)?;
    let (workspace_session, workspace_run) = child_task::workspace_owner(ctx.final_session_id, ctx.parent_run_id)?;
    let _workspace = crate::agent::workspace_lock::acquire_delegation_lock(
        ctx.workspace_dir,
        &workspace_session,
        &workspace_run,
    )
    .await?;
    let id = match existing {
        Some(id) => id,
        None => crate::agent::child_reviewer::begin_child_run_extended(
            ctx.state.inner(),
            ctx.parent_run_id,
            ctx.final_session_id,
            ctx.workspace_dir,
            &child_routing.pro,
            0,
            "task",
            Some(i64::from(crate::agent::child_reviewer::MAX_TOTAL_CHILDREN_PER_RUN)),
            None,
            predecessor.as_deref(),
            &crate::agent::child_reviewer::ChildRunIdentity {
                contract_schema: task_role.as_ref().and_then(|card|card.parse_custom_schema()).filter(|schema|!schema.is_empty()).map(|schema|serde_json::to_string(&schema)).transpose().map_err(|e|e.to_string())?,
                execution_config: params.get("execution").map(Value::to_string),
                purpose: Some(profile.to_string()),
                dispatch_input: Some(json!({"params":params,"model_selection":model_selection}).to_string()),
                task_id: key,
                ..crate::agent::child_reviewer::ChildRunIdentity::fresh("task")
            },
        )?,
    };
    let binding = TaskBinding {
        parent_session: ctx.final_session_id.clone(),
        parent_run: ctx.parent_run_id.into(),
        child_session: format!("task-{id}"),
        child_run: id.clone(),
    };
    let mut role_context = task_role.as_ref().map(|card|format!("{}\n{}\n{}",crate::agent::tools::role_manager::persona_block(card),crate::agent::tools::role_manager::skills_block(ctx.state.inner(),card),card.parse_custom_schema().map(|s|s.render_prompt_instructions()).unwrap_or_default())).unwrap_or_default();
    if let Some(brief) = ctx.checkpoint.triage_verdict.as_ref().and_then(|v|v.investigation_brief_text()) {
        role_context.push_str(&format!("\n【入口调查参考（不是新增验收义务）】{brief}"));
    }
    let goal = crate::agent::task_context::ordinary(&params, &role_context,
        &crate::agent::path_guard::get_session_tmp_dir(ctx.final_session_id), &id);
    child_task::runtime::spawn(
        ctx.app.clone(),
        ctx.state.inner().clone(),
        binding,
        child_routing,
        goal,
        ctx.workspace_dir.into(),
        ctx.checkpoint.auto_pilot,
        execution.workflow.plan_mode.unwrap_or_else(||"auto".into()),
        _workspace,
    )?;
    Ok(DispatchResult {view:None,success:true,stdout:json!({"child_run_id":id,"model_selection":model_selection,"execution_status":"running","delivery_accepted":false,"next":"可用 child_task.status / send / wait / cancel；共享工作区可并行使用，请协调相关修改。需要信息时子任务可给 parent 发消息，父任务等待会被消息唤醒。"}).to_string(),stderr:String::new(),route:"child_task:spawned".into()})
}

/// Legacy dispatch spellings select a preset; they do not own a second executor.
pub(super) async fn dispatch_alias(ctx: &mut DispatchCtx<'_>, inst: &AgentInstruction, profile: &str) -> Result<ArmOutcome,String> {
    let items=inst.get_dispatch_items();
    let is_batch = items.len() > 1;
    if is_batch {
        ctx.checkpoint.begin_child_dispatch_batch(inst, ctx.context)?;
        save_checkpoint(ctx.state.inner(), ctx.checkpoint)?;
    }
    let mut reports=Vec::new();
    for (index, params) in items.into_iter().enumerate() {
        if is_batch {
            if let Some(report) = ctx.checkpoint.child_dispatch_batch.as_ref()
                .and_then(|batch| batch.results[index].as_ref()) {
                reports.push(report.clone());
                continue;
            }
        }
        let mut adapted=inst.clone();
        // Invalid siblings are reported individually, preserving prior receipts.
        let params = match child_task::presets::requested(params,Some(profile)) {
            Ok(params) => params,
            Err(error) => {
                let report = crate::agent::tools::input_validation::rejection(error);
                if is_batch {
                    ctx.checkpoint.child_dispatch_batch.as_mut().unwrap().results[index] = Some(report.clone());
                    save_checkpoint(ctx.state.inner(), ctx.checkpoint)?;
                }
                reports.push(report);
                continue;
            }
        };
        adapted.tool=Some("child_task".into());
        adapted.action=Some("dispatch".into());
        adapted.command=None;
        adapted.dispatches=None;
        adapted.params=Some(params);
        let report = Box::pin(dispatch_item(ctx,&adapted,is_batch.then_some(index))).await?;
        if is_batch {
            ctx.checkpoint.child_dispatch_batch.as_mut().unwrap().results[index] = Some(report.clone());
            // The adapter returned a known receipt; only an interruption before
            // this point may retain an uncertain specialised pending call.
            ctx.checkpoint.begin_child_dispatch_batch(inst, ctx.context)?;
            save_checkpoint(ctx.state.inner(), ctx.checkpoint)?;
        }
        reports.push(report);
    }
    if reports.len()==1 {return Ok(ArmOutcome::Done(reports.remove(0)));}
    let success=reports.iter().any(|r|r.success);
    Ok(ArmOutcome::Done(DispatchResult{view:None,success,stdout:json!({"results":reports.iter().map(|r|json!({"success":r.success,"output":r.stdout,"error":r.stderr})).collect::<Vec<_>>(),"notice":"共享工作区支持并行；请核对各项受理结果并协调相关修改。"}).to_string(),stderr:String::new(),route:"child_task:batch".into()}))
}
