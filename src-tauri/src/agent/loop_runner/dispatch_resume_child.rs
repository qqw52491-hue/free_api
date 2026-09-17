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
            let child_data: Result<(String, String, Option<String>, Option<String>, String, String, Option<String>), String> = {
                let conn = ctx.state.0.lock().map_err(|e| e.to_string())?;
                conn.query_row(
                    "SELECT status, kind, workspace_copy, message_history_json, model_id, workspace_root, result_json FROM agent_child_runs WHERE id = ?1",
                    rusqlite::params![target_child_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, Option<String>>(6)?,
                        ))
                    },
                ).map_err(|e| format!("未找到子任务「{}」记录: {}", target_child_id, e))
            };

            match child_data {
                Err(err) => DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: err,
                    success: false,
                    route: "host_rejection:child_not_found".to_string(),
                },
                Ok((status, kind, ws_copy, hist_json, child_model, root_str, prev_result_json)) => {
                    if status != "succeeded" {
                        DispatchResult { view: None,
                            stdout: String::new(),
                            stderr: format!("❌ 仅能追问已成功的子任务（该任务当前状态为「{}」）；如需继续请直接派发新任务。", status),
                            success: false,
                            route: "host_rejection:status_not_succeeded".to_string(),
                        }
                    } else if kind == "review" || kind == "reviewer" {
                        // 🌟 Reviewer 纯上下文对质分支：只读审查者无工作区副本，纯继承脱水历史进行反驳研判
                        let mut messages: Vec<Value> = match hist_json.as_deref() {
                            Some(raw) => serde_json::from_str(raw).unwrap_or_default(),
                            None => Vec::new(),
                        };
                        if messages.is_empty() {
                            DispatchResult { view: None,
                                stdout: String::new(),
                                stderr: "❌ 该审查任务未留存历史对话记录，无法恢复对质。".to_string(),
                                success: false,
                                route: "host_rejection:missing_history".to_string(),
                            }
                        } else {
                            messages.push(json!({
                                "role": "user",
                                "content": format!(
                                    "【父 Agent 反驳与对质】\n{}\n\n【对质研判规则】：\n请针对上述反驳与证据进行重新研判，输出更新后的审查 JSON：\n1. 若父 Agent 反驳与测试证据成立，请对该项缺陷进行撤回：在 objection_verdicts 中将该 finding_id 的 verdict 设为 withdraw 并给出裁决理由，或从 findings 中剔除并在 missing_evidence 说明已撤回；\n2. 若仍坚持该缺陷，必须在 why 中给出全新证据与理由，或在 objection_verdicts 中将 verdict 设为 maintain；\n3. 重新推导 routing（certain/needs_alignment/blocked）。",
                                    followup_msg
                                )
                            }));

                            let routing = crate::agent::router::AgentRouter::route_for_role(
                                ctx.context,
                                ctx.model_routing,
                                ctx.step_id,
                                crate::agent::router::roles::REVIEWER,
                            );
                            let model_to_use = if !child_model.is_empty() {
                                child_model
                            } else {
                                let (preferred, _note) = crate::agent::router::prefer_cross_family_reviewer(
                                    &*ctx.state.inner().0.lock().unwrap(),
                                    &routing.model_id,
                                );
                                preferred
                            };
                            let reasoning_effort = ctx.model_routing.reasoning_effort_for(&routing.tier).to_string();

                            let new_child_run_id = match crate::agent::child_reviewer::begin_child_run_extended(
                                ctx.state.inner(),
                                ctx.parent_run_id,
                                ctx.final_session_id,
                                ctx.workspace_dir,
                                &model_to_use,
                                0,
                                "review",
                                None,
                                None,
                                Some(target_child_id),
                            ) {
                                Ok(id) => {
                                    // 对质目标是阶段审查任务时，登记复检 run id 以便结果回流阶段语义
                                    if ctx.checkpoint.is_stage_review_target(target_child_id) {
                                        ctx.checkpoint.stage_review_rebuttal_run_id = Some(id.clone());
                                    }
                                    id
                                }
                                Err(err) => {
                                    return Ok(ArmOutcome::Early(DispatchResult { view: None,
                                        stdout: String::new(),
                                        stderr: format!("❌ 创建审查对质任务失败: {}", err),
                                        success: false,
                                        route: "host_rejection:begin_run_failed".to_string(),
                                    }));
                                }
                            };

                            let mut config = match ModelConfig::from_db_state(ctx.state.inner(), &model_to_use) {
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
                            config.max_tokens = 900;
                            if !ctx.checkpoint.plan_phase.is_approved() {
                                // 反驳对质同样消耗审查预算：必须走统一预扣，堵住无上限自增旁路
                                if let Err(reason) = ctx.checkpoint.reserve_plan_review_round() {
                                    return Ok(ArmOutcome::Early(DispatchResult { view: None,
                                        stdout: String::new(),
                                        stderr: format!("【审查轮数已达上限】{}\n{}", reason, crate::agent::gate_messages::reviewer_retry_limit()),
                                        success: false,
                                        route: "plan_gatekeeper:reviewer_retry_limit".to_string(),
                                    }));
                                }
                            }

                            let db_clone = DbState(ctx.state.0.clone());
                            let new_child_run_id_clone = new_child_run_id.clone();
                            let parent_session_id_clone = ctx.final_session_id.to_string();
                            let mut interrupt_rx_clone = ctx.interrupt_rx.clone();
                            let _app_clone = ctx.app.clone();

                            tokio::spawn(async move {
                                let started_at = std::time::Instant::now();
                                let child_log_suffix = format!("child-{}", new_child_run_id_clone);
                                let mut on_stream_event = |_: crate::llm::StreamEvent<'_>| -> bool { true };
                                let call = crate::llm::stream_with_retry(
                                    &config,
                                    &messages,
                                    &reasoning_effort,
                                    &mut interrupt_rx_clone,
                                    1,
                                    &mut on_stream_event,
                                );
                                let stream_result = match tokio::time::timeout(
                                    std::time::Duration::from_secs(
                                        crate::agent::child_reviewer::child_timeout_secs(&config.model_name),
                                    ),
                                    call,
                                ).await {
                                    Ok(Ok(res)) => res,
                                    Ok(Err(e)) => {
                                        crate::agent::utils::llm_client::log_auxiliary_llm_exchange(
                                            &parent_session_id_clone,
                                            &child_log_suffix,
                                            1,
                                            &config,
                                            &messages,
                                            None,
                                            "request_error",
                                            Some(&e),
                                        );
                                        crate::agent::child_reviewer::finish_child_run(
                                            &db_clone,
                                            &new_child_run_id_clone,
                                            "failed",
                                            started_at.elapsed().as_millis() as u64,
                                            0,
                                            Some(crate::agent::types::ChildErrorKind::RequestOrStream),
                                            None,
                                        );
                                        return;
                                    }
                                    Err(_) => {
                                        crate::agent::utils::llm_client::log_auxiliary_llm_exchange(
                                            &parent_session_id_clone,
                                            &child_log_suffix,
                                            1,
                                            &config,
                                            &messages,
                                            None,
                                            "request_error",
                                            Some("timeout"),
                                        );
                                        crate::agent::child_reviewer::finish_child_run(
                                            &db_clone,
                                            &new_child_run_id_clone,
                                            "timed_out",
                                            started_at.elapsed().as_millis() as u64,
                                            0,
                                            Some(crate::agent::types::ChildErrorKind::Timeout),
                                            None,
                                        );
                                        return;
                                    }
                                };

                                crate::agent::utils::llm_client::log_auxiliary_llm_exchange(
                                    &parent_session_id_clone,
                                    &child_log_suffix,
                                    1,
                                    &config,
                                    &messages,
                                    Some(&stream_result),
                                    "success",
                                    None,
                                );

                                let duration_ms = started_at.elapsed().as_millis() as u64;
                                let completion_tokens = stream_result.usage.map(|u| u.completion_tokens).unwrap_or(0);
                                let (findings, confidence, evidence_refs, missing_evidence, routing, objection_verdicts, withdrawn_findings) = 
                                    match crate::agent::child_reviewer::parse_child_review_rebuttal_output(&stream_result.full_response) {
                                        Ok(res) => res,
                                        Err(parse_err) => {
                                            // 对质解析失败也要落 raw：这是 09-07 pro 实弹 12 条
                                            // NULL invalid 的来源，丢 raw = 丢唯一诊断证据
                                            crate::agent::child_reviewer::finish_child_run(
                                                &db_clone,
                                                &new_child_run_id_clone,
                                                "invalid_output",
                                                duration_ms,
                                                completion_tokens,
                                                Some(crate::agent::types::ChildErrorKind::InvalidOutput),
                                                Some(&crate::agent::child_reviewer::child_invalid_result_json(
                                                    &new_child_run_id_clone,
                                                    duration_ms,
                                                    completion_tokens,
                                                    &format!("解析错误: {parse_err}\n原始输出: {}", stream_result.full_response),
                                                )),
                                            );
                                            return;
                                        }
                                    };

                                let review_res = crate::agent::child_reviewer::ChildReviewResult {
                                    child_run_id: new_child_run_id_clone.clone(),
                                    status: "succeeded".to_string(),
                                    findings,
                                    confidence,
                                    evidence_refs,
                                    missing_evidence,
                                    duration_ms,
                                    completion_tokens,
        prompt_tokens: 0,
                                    error_kind: None,
                                    raw_result_json: None,
                                    kind: Some("review".to_string()),
                                    routing: Some(routing),
                                    withdrawn_findings,
                                    objection_verdicts,
                                    scorecard: None,
                                };

                                let res_json = serde_json::to_string(&review_res).ok();
                                crate::agent::child_reviewer::finish_child_run_with_history(
                                    &db_clone,
                                    &new_child_run_id_clone,
                                    "succeeded",
                                    duration_ms,
                                    completion_tokens,
                                    None,
                                    res_json.as_deref(),
                                    serde_json::to_string(&messages).ok().as_deref(),
                                );
                            });

                            DispatchResult { view: None,
                                stdout: format!(
                                    "✅ 审查专家对质已成功启动：新 child_run_id={}（纯上下文继承唤醒，正在重新研判反驳）。你可以继续执行其他工作，或使用 wait_jobs 等待结果。",
                                    new_child_run_id
                                ),
                                stderr: String::new(),
                                success: true,
                                route: "resume_child:reviewer_rebuttal_spawned".to_string(),
                            }
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
