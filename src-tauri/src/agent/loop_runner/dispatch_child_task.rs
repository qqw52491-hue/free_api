//! 异步通用委派：父模型可按任务 ID 收发消息，工作区变更按序执行。
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
    match dispatch_inner(ctx, inst).await {
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
) -> Result<DispatchResult, String> {
    let params = inst.get_params();
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
    match effective.as_str() {
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
        "list" => {
            return Ok(output(
                json!({"tasks":crate::agent::agent_mailbox::family_list(ctx.state.inner(),&actor)?}),
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
    if child_task::parent_session(ctx.final_session_id).is_some() {
        return Ok(DispatchResult { view:None,success:false,stdout:String::new(),stderr:"首版仅支持一层通用任务委派；本次未启动子任务。已有专用 worker/writer/reviewer 仍可使用。".into(),route:"child_task:depth_limit".into() });
    }
    if inst.dispatches.is_some() {
        return Ok(DispatchResult {
            view: None,
            success: false,
            stdout: String::new(),
            stderr: "child_task 首版串行执行，请单次提供 task；dispatches 未执行。".into(),
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
    let task_identity = json!([params["task"], params.get("context"), predecessor]).to_string();
    let key = format!(
        "general:{}:{}:{}",
        ctx.context.history_task_id,
        ctx.step_id,
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, task_identity.as_bytes())
    );
    let existing = child_task::find_task(ctx.state.inner(), ctx.final_session_id, &key)?;
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
    // 共享工作区串行使用；不把父级仍在运行的专用任务/后台 Shell 带进来竞争。
    let busy = child_task::busy_jobs(ctx.state.inner(), ctx.parent_run_id, existing.as_deref())?;
    if !busy.is_empty() {
        return Ok(DispatchResult {
            view: None,
            success: false,
            stdout: String::new(),
            stderr: format!(
                "共享工作区仍有在途任务；本次未启动通用子任务。请先回收或停止：{}",
                busy.join(", ")
            ),
            route: "child_task:workspace_busy".into(),
        });
    }
    ctx.checkpoint.prepare_tool(inst.clone(), ctx.context);
    save_checkpoint(ctx.state.inner(), ctx.checkpoint)?;
    let _workspace = crate::agent::workspace_lock::acquire_delegation_lock(
        ctx.workspace_dir,
        ctx.final_session_id,
        ctx.parent_run_id,
    )
    .await?;
    let id = match existing {
        Some(id) => id,
        None => crate::agent::child_reviewer::begin_child_run_extended(
            ctx.state.inner(),
            ctx.parent_run_id,
            ctx.final_session_id,
            ctx.workspace_dir,
            &ctx.model_routing.pro,
            0,
            "task",
            Some(1),
            None,
            predecessor.as_deref(),
            &crate::agent::child_reviewer::ChildRunIdentity {
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
    let goal = format!(
        "{}\n\n{}\n\n【宿主提供的父会话证据目录】{}（只读引用请用绝对路径；你的 tmp/ 独立）。",
        params["task"].as_str().unwrap_or_default(),
        params["context"].as_str().unwrap_or_default(),
        crate::agent::path_guard::get_session_tmp_dir(ctx.final_session_id).display()
    );
    child_task::runtime::spawn(
        ctx.app.clone(),
        ctx.state.inner().clone(),
        binding,
        ctx.model_routing.clone(),
        goal,
        ctx.workspace_dir.into(),
        ctx.checkpoint.auto_pilot,
        ctx.checkpoint.plan_mode.as_str().into(),
        _workspace,
    )?;
    Ok(DispatchResult {view:None,success:true,stdout:json!({"child_run_id":id,"execution_status":"running","delivery_accepted":false,"next":"可用 child_task.status / send / wait / cancel；共享工作区暂由该子任务使用。需要信息时子任务可给 parent 发消息，父任务等待会被消息唤醒。"}).to_string(),stderr:String::new(),route:"child_task:spawned".into()})
}
