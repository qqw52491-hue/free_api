//! resume_child 追问臂：子 Agent 对质/续跑与 verdict 裁决处理。

//! 子 Agent 派发臂：child_reviewer / child_writer / child_worker / resume_child。

use crate::agent::checkpoint::{save_checkpoint, AgentCheckpoint};
use crate::agent::child_reviewer::result_as_dispatch_output;
use crate::agent::context::resource_scope::ResourceOperation;
use crate::agent::control::{take_pending_interrupt, HumanControl};
use crate::agent::dispatcher::{
    emit_tool_event, is_finish_instruction, is_write_or_side_effect_tool, run_agent_step,
};
use crate::agent::mcp::PluginRegistry;
use crate::agent::run_state::{AgentRunEvent, AgentRunMachine};
use crate::agent::types::{AgentInstruction, DispatchResult, RUN_INTERRUPTED_ROUTE};
use crate::agent::utils::call_llm;
use crate::db::DbState;
use crate::llm::ModelConfig;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, watch};

use super::*;

pub(super) async fn dispatch_resume_child(
    ctx: &mut DispatchCtx<'_>,
    inst: &AgentInstruction,
) -> Result<ArmOutcome, String> {
    Ok(ArmOutcome::Done({
        let params = inst.get_params();
        let target_child_id = params
            .get("child_run_id")
            .or_else(|| params.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let followup_msg = params
            .get("message")
            .or_else(|| params.get("task"))
            .or_else(|| params.get("query"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let test_command_override = params
            .get("test_command")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string());

        if target_child_id.is_empty() {
            DispatchResult { view: None,
                stdout: String::new(),
                stderr: "❌ resume_child 缺少必要参数 child_run_id。".to_string(),
                success: false,
                route: "host_rejection:missing_child_run_id".to_string(),
            }
        } else if followup_msg.is_empty() {
            DispatchResult { view: None,
                stdout: String::new(),
                stderr: "❌ resume_child 缺少必要参数 message（对该子任务的追问或补充要求）。".to_string(),
                success: false,
                route: "host_rejection:missing_followup_message".to_string(),
            }
        } else {
            // 会话归属校验由加载函数内聚承担（child_run_id 是会话内凭据）
            let child_data = crate::agent::child_reviewer::load_child_run_for_resume(
                ctx.state.inner(),
                target_child_id,
                ctx.final_session_id,
            );

            match child_data {
                Err(err) => DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: err,
                    success: false,
                    route: "host_rejection:child_not_found".to_string(),
                },
                Ok((status, kind, ws_copy, hist_json, child_model, root_str, prev_result_json, prev_identity)) => {
                    // 审查类兜底：invalid_output 的审查子任务若产出过可解析的
                    // findings/objection_verdicts（已计入 blocking 台账），对质
                    // 通道必须可达——否则门禁开的「resume_child 对质撤回」药方
                    // 指向死路（实测 3823067c 会话 step47 死锁）。其余非成功态
                    // （failed/cancelled/running）仍拒绝。
                    let is_review_kind = kind == "review" || kind == "reviewer";
                    let invalid_review_with_findings = is_review_kind
                        && status == "invalid_output"
                        && prev_result_json
                            .as_deref()
                            .and_then(|r| serde_json::from_str::<serde_json::Value>(r).ok())
                            .is_some_and(|v| {
                                v.get("findings")
                                    .and_then(|f| f.as_array())
                                    .is_some_and(|a| !a.is_empty())
                                    || v.get("objection_verdicts")
                                        .and_then(|f| f.as_array())
                                        .is_some_and(|a| !a.is_empty())
                            });
                    if status != "succeeded" && !invalid_review_with_findings {
                        DispatchResult { view: None,
                            stdout: String::new(),
                            stderr: format!("❌ 仅能追问已成功的子任务（该任务当前状态为「{}」）；如需继续请直接派发新任务。", status),
                            success: false,
                            route: "host_rejection:status_not_succeeded".to_string(),
                        }
                    } else if is_review_kind {
                        let routing = crate::agent::router::AgentRouter::route_for_role(
                            ctx.context, ctx.model_routing, ctx.step_id,
                            crate::agent::router::roles::REVIEWER,
                        );
                        let model_to_use = if !child_model.is_empty() { child_model }
                            else if ctx.model_routing.auto_cross_family_reviewer {
                                crate::agent::router::prefer_cross_family_reviewer(
                                    &*ctx.state.inner().0.lock().unwrap(), &routing.model_id,
                                ).0
                            } else { routing.model_id.clone() };
                        match ModelConfig::from_db_state(ctx.state.inner(), &model_to_use) {
                            Err(error) => DispatchResult { view: None, success: false,
                                stdout: String::new(), stderr: format!("加载模型配置失败：{error}"),
                                route: "host_rejection:model_config_failed".into() },
                            Ok(config) => match crate::agent::child_reviewer::spawn_review_resume(
                                ctx.state.inner(), ctx.parent_run_id, ctx.final_session_id,
                                ctx.workspace_dir, target_child_id, followup_msg, config,
                                ctx.model_routing.reasoning_effort_for(&routing.tier).to_string(),
                                ctx.checkpoint, ctx.interrupt_rx.clone(),
                            ) {
                                Ok(id) => DispatchResult { view: None, success: true,
                                    stdout: format!("审查专家对质已启动：child_run_id={id}。继承原任务上下文；其他成员结论保留，可继续独立工作或 wait_jobs 等待。"),
                                    stderr: String::new(), route: "resume_child:reviewer_rebuttal_spawned".into() },
                                Err(error) => DispatchResult { view: None, success: false,
                                    stdout: String::new(), stderr: error.message, route: error.route.into() },
                            },
                        }
                    } else {
                        // 校验工作区物理存在性（写入/侦察类 worker 必须依赖工作区副本）
                        let ws_path = match ws_copy {
                            Some(ref p) if PathBuf::from(p).exists() => PathBuf::from(p),
                            _ => PathBuf::new(),
                        };
                        if ws_path.as_os_str().is_empty() {
                            DispatchResult { view: None,
                                stdout: String::new(),
                                stderr: "❌ 该子任务的工作副本已随父运行结束回收，无法继续追问，请重新派发新任务。".to_string(),
                                success: false,
                                route: "host_rejection:workspace_recycled".to_string(),
                            }
                        } else {
                            let mut messages: Vec<Value> = match hist_json.as_deref() {
                                Some(raw) => serde_json::from_str(raw).unwrap_or_default(),
                                None => Vec::new(),
                            };
                            if messages.is_empty() {
                                DispatchResult { view: None,
                                    stdout: String::new(),
                                    stderr: "❌ 该子任务未留存历史对话记录，无法恢复。".to_string(),
                                    success: false,
                                    route: "host_rejection:missing_history".to_string(),
                                }
                            } else {
                                // 注入追问消息
                                messages.push(json!({
                                    "role": "user",
                                    "content": format!("【追问与补充任务】\n{}", followup_msg)
                                }));

                                let routing = crate::agent::router::AgentRouter::route_for_role(
                                    ctx.context,
                                    ctx.model_routing,
                                    ctx.step_id,
                                    if kind == "writer" {
                                        crate::agent::router::roles::WRITER
                                    } else {
                                        crate::agent::router::roles::SCOUT
                                    },
                                );
                                let model_to_use = if !child_model.is_empty() {
                                    child_model
                                } else {
                                    routing.model_id.clone()
                                };
                                let reasoning_effort = ctx.model_routing.reasoning_effort_for(&routing.tier).to_string();

                                // 追问续跑继承原任务逻辑身份：task_id/batch 不变，
                                // attempt 由落库侧按同 task_id 自动递增
                                let resume_identity = crate::agent::child_reviewer::ChildRunIdentity::inherited(
                                    &prev_identity,
                                    None,
                                );
                                let new_child_run_id = match crate::agent::child_reviewer::begin_child_run_extended(
                                    ctx.state.inner(),
                                    ctx.parent_run_id,
                                    ctx.final_session_id,
                                    ctx.workspace_dir,
                                    &model_to_use,
                                    0,
                                    &kind,
                                    None,
                                    Some(ws_path.to_string_lossy().as_ref()),
                                    Some(target_child_id),
                                    &resume_identity,
                                ) {
                                    Ok(id) => id,
                                    Err(err) => {
                                        return Ok(ArmOutcome::Early(DispatchResult { view: None,
                                            stdout: String::new(),
                                            stderr: format!("❌ 创建追问子任务失败: {}", err),
                                            success: false,
                                            route: "host_rejection:begin_run_failed".to_string(),
                                        }));
                                    }
                                };

                                let config = match ModelConfig::from_db_state(ctx.state.inner(), &model_to_use) {
                                    Ok(cfg) => cfg,
                                    Err(err) => {
                                        return Ok(ArmOutcome::Early(DispatchResult { view: None,
                                            stdout: String::new(),
                                            stderr: format!("❌ 加载模型配置失败: {}", err),
                                            success: false,
                                            route: "host_rejection:model_config_failed".to_string(),
                                        }));
                                    }
                                };

                                if kind == "writer" {
                                    // 🛑 前置方案门禁：恢复(resume_child)写入型子任务等同于开启新的写入路径，
                                    // 未获批准前必须拦截，防止绕过派发门禁（B1.8）通过旧 writer 子任务改代码。
                                    // 僵尸 blocker 清账同主写入门禁（见 B1.8 注释）。
                                    if ctx.checkpoint.stage_review_blockers_open > 0 {
                                        if let Some((idx, _, _)) =
                                            crate::agent::eval_oracle::query_active_progress().await
                                        {
                                            if let Some(note) = ctx.checkpoint.clear_stale_stage_blockers(idx as u32) {
                                                let _ = ctx.app.emit(
                                                    &format!("agent-log-{}", ctx.final_session_id),
                                                    note,
                                                );
                                            }
                                        }
                                    }
                                    if let Some(rejection) = check_pre_write_gatekeeper(
                                        ctx.checkpoint,
                                        ctx.context,
                                        ctx.state.inner(),
                                        ctx.parent_run_id,
                                        ctx.step_id,
                                        true,
                                    ) {
                                        crate::agent::telemetry::record_gate_fire(
                                            ctx.state.inner(),
                                            &crate::agent::telemetry::GateFireRecord {
                                                run_id: ctx.parent_run_id,
                                                session_id: ctx.final_session_id,
                                                step_id: ctx.step_id as i64,
                                                gate: "pre_write",
                                                route: &rejection.route,
                                            },
                                        );
                                        emit_tool_event(
                                            ctx.app,
                                            ctx.final_session_id,
                                            ctx.step_id,
                                            inst,
                                            "failed",
                                            Some(rejection.route.clone()),
                                            Some(false),
                                            &rejection.stderr,
                                            Some(ctx.tool_started_at.elapsed().as_millis()),
                                            None,
                                        );
                                        return Ok(ArmOutcome::Early(rejection));
                                    }

                                    let inherited_test_cmd = prev_result_json
                                        .as_deref()
                                        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                                        .and_then(|v| v.get("test_command").and_then(|s| s.as_str()).map(|s| s.to_string()))
                                        .filter(|s| !s.trim().is_empty());
                                    let test_cmd = test_command_override
                                        .or(inherited_test_cmd)
                                        .unwrap_or_else(|| "cargo test".to_string());
                                    let writer_req = crate::agent::child_writer::WriterRequest {
                                        task: followup_msg.to_string(),
                                        test_command: test_cmd,
                                        focus_paths: Vec::new(),
                                        context_excerpts: None,
                                        hard_scope: String::new(),
                                        step_budget: None,
                                    };
                                    let max_steps = crate::agent::child_writer::MAX_WRITER_STEPS / 2;
                                    let parent_root = PathBuf::from(&root_str);
                                    tokio::spawn(crate::agent::child_writer::run_writer_loop(
                                        DbState(ctx.state.0.clone()),
                                        Some(ctx.app.clone()),
                                        ctx.final_session_id.to_string(),
                                        new_child_run_id.clone(),
                                        parent_root,
                                        ws_path,
                                        config,
                                        reasoning_effort,
                                        writer_req,
                                        String::new(),
                                        ctx.interrupt_rx.clone(),
                                        Some(messages),
                                        Some(max_steps),
                                    ));

                                    DispatchResult { view: None,
                                        stdout: format!(
                                            "✅ 写入 worker 追问已成功启动：新 child_run_id={}（继承上轮历史与工作区副本，步数上限减半为 {} 步）。你可以继续执行其他工作，或使用 wait_jobs 等待结果。",
                                            new_child_run_id, max_steps
                                        ),
                                        stderr: String::new(),
                                        success: true,
                                        route: "resume_child:writer_spawned".to_string(),
                                    }
                                } else {
                                    let worker_req = crate::agent::child_worker::WorkerRequest {
                                        task: followup_msg.to_string(),
                                        focus_paths: Vec::new(),
                                        context_excerpts: None,
                                        hard_scope: String::new(),
                                    };
                                    let max_steps = crate::agent::child_worker::MAX_WORKER_STEPS / 2;
                                    tokio::spawn(crate::agent::child_worker::run_worker_loop(
                                        DbState(ctx.state.0.clone()),
                                        Some(ctx.app.clone()),
                                        ctx.final_session_id.to_string(),
                                        new_child_run_id.clone(),
                                        ws_path,
                                        config,
                                        reasoning_effort,
                                        worker_req,
                                        String::new(),
                                        ctx.interrupt_rx.clone(),
                                        Some(messages),
                                        Some(max_steps),
                                    ));

                                    DispatchResult { view: None,
                                        stdout: format!(
                                            "✅ 侦察 worker 追问已成功启动：新 child_run_id={}（继承上轮历史，步数上限减半为 {} 步）。你可以继续执行其他工作，或使用 wait_jobs 等待结果。",
                                            new_child_run_id, max_steps
                                        ),
                                        stderr: String::new(),
                                        success: true,
                                        route: "resume_child:worker_spawned".to_string(),
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    // --- B3. 特殊拦截：AI 主动挂起呼叫人类 ---
    }))
}
