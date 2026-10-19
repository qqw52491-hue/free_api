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
                        // 🌟 Reviewer 纯上下文对质分支：只读审查者无工作区副本，纯继承脱水历史进行反驳研判
                        let mut messages: Vec<Value> = match hist_json.as_deref() {
                            Some(raw) => serde_json::from_str(raw).unwrap_or_default(),
                            None => Vec::new(),
                        };
                        crate::agent::child_reviewer::retire_consumer_history(&mut messages);
                        crate::agent::child_reviewer::refresh_review_instructions(&mut messages);
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
                            } else if ctx.model_routing.auto_cross_family_reviewer {
                                let (preferred, _note) = crate::agent::router::prefer_cross_family_reviewer(
                                    &*ctx.state.inner().0.lock().unwrap(),
                                    &routing.model_id,
                                );
                                preferred
                            } else {
                                routing.model_id.clone()
                            };
                            let reasoning_effort = ctx.model_routing.reasoning_effort_for(&routing.tier).to_string();

                            // 对质目标是阶段审查任务时标记 rebuttal 用途；其余继承原任务用途
                            let is_rebuttal_target = ctx.checkpoint.is_stage_review_target(ctx.state.inner(), target_child_id);
                            let mut resume_identity = crate::agent::child_reviewer::ChildRunIdentity::inherited(
                                &prev_identity,
                                if is_rebuttal_target {
                                    Some("rebuttal".to_string())
                                } else {
                                    prev_identity.purpose.clone().or(Some("review".to_string()))
                                },
                            );

                            crate::agent::child_reviewer::retire_consumer_schema_json(
                                &mut resume_identity.contract_schema,
                            );

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
                                &resume_identity,
                            ) {
                                Ok(id) => {
                                    // 对质身份已由台账 purpose='rebuttal' 盖章承担
                                    //（resume_identity 落库时写入），无需 checkpoint 指针
                                    let _ = is_rebuttal_target;
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
                                let stream_stage_key = format!("child:{}", new_child_run_id_clone);
                                crate::agent::control::mark_stage(&stream_stage_key, "child_task:llm_call:1");
                                let mut last_activity_mark = std::time::Instant::now();
                                let mut on_stream_event = |_: crate::llm::StreamEvent<'_>| -> bool {
                                    if last_activity_mark.elapsed() >= std::time::Duration::from_secs(5) {
                                        crate::agent::control::mark_stage(&stream_stage_key, "child_task:stream_output");
                                        last_activity_mark = std::time::Instant::now();
                                    }
                                    true
                                };
                                let stream_result = crate::agent::review_stream::stream_review(
                                    &config, &messages, &reasoning_effort, &mut interrupt_rx_clone,
                                    None, crate::agent::child_reviewer::child_timeout_secs(&config.model_name),
                                    &mut on_stream_event,
                                ).await;
                                let stream_result = match stream_result {
                                    Ok(result) => result,
                                    Err(failure) => {
                                        failure.log(&parent_session_id_clone, &child_log_suffix, 1, &config, &messages);
                                        let (status, kind) = if failure.error == crate::llm::STREAM_INTERRUPTED_ERROR {
                                            ("cancelled", crate::agent::types::ChildErrorKind::Cancelled)
                                        } else if failure.is_timeout() {
                                            ("timed_out", crate::agent::types::ChildErrorKind::Timeout)
                                        } else {
                                            ("failed", crate::agent::types::ChildErrorKind::RequestOrStream)
                                        };
                                        let elapsed_ms = started_at.elapsed().as_millis() as u64;
                                        crate::agent::child_reviewer::finish_child_run(
                                            &db_clone, &new_child_run_id_clone, status,
                                            elapsed_ms, 0, 0, Some(kind.clone()),
                                            Some(&crate::agent::child_reviewer::child_error_result_json(
                                                &new_child_run_id_clone, status, &kind, elapsed_ms, 0, 0,
                                                Some(&failure.description()),
                                            )),
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
                                let prompt_tokens = stream_result.usage.map(|u| u.prompt_tokens).unwrap_or(0);
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
                                                prompt_tokens,
                                                Some(crate::agent::types::ChildErrorKind::InvalidOutput),
                                                Some(&crate::agent::child_reviewer::child_invalid_result_json(
                                                    &new_child_run_id_clone,
                                                    duration_ms,
                                                    completion_tokens,
                                                    prompt_tokens,
                                                    &stream_result.full_response,
                                                    &parse_err,
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
                                    prompt_tokens,
                                    error_kind: None,
                                    raw_result_json: None,
                                    kind: Some("review".to_string()),
                                    routing: Some(routing),
                                    withdrawn_findings,
                                    objection_verdicts,
                                    scorecard: None,
                                    task_id: None,
                                    attempt_id: None,
                                };

                                let res_json = serde_json::to_string(&review_res).ok();
                                crate::agent::child_reviewer::finish_child_run_with_history(
                                    &db_clone,
                                    &new_child_run_id_clone,
                                    "succeeded",
                                    duration_ms,
                                    completion_tokens,
                                    prompt_tokens,
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
