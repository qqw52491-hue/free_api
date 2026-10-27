//! RunTool 阶段执行器：工具派发、子 Agent 调度与人工介入处理。
//! 既有外部引用路径 crate::agent::loop_runner::* 通过下方 re-export 全部维持不变。

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

pub(crate) const REPEATED_FAILURE_BLOCKED_ROUTE: &str = "repeated_deterministic_failure";

#[derive(Debug, Clone)]
pub struct StepExecutionOutcome {
    pub instruction: AgentInstruction,
    pub result: DispatchResult,
    pub token_usage: Option<crate::agent::types::TokenUsage>,
    pub thinking_duration_ms: u64,
    pub tool_duration_ms: u64,
    /// 本步模型推理全文（reasoning stream）。只进复盘轨迹与落库，不回灌模型上下文。
    pub thinking_text: String,
}

/// 执行一个可恢复步骤的 CallLlm 与 RunTool 阶段。
///
/// 该 handler 封装模型规划、工具派发、人工等待、重试和救援；每个外部副作用
pub use crate::agent::call_llm_phase::run_call_llm_and_run_tool_phases;
pub(crate) use crate::agent::plan_gate::{
    check_pre_write_gatekeeper, extract_plan_decision, parse_plan_approval_decision,
    strip_plan_decision_marker,
};

/// ask_human 冷却判定（先监听后求助）：刚被门禁拦截（本步或上一步）且门禁未指示求助时，
/// 返回冷却拒绝文案；否则 None 放行。一次性豁免：拒绝后清除拦截记录，
/// 模型再行动一步后仍被困住可正常求助，避免死锁。
pub(crate) fn evaluate_ask_human_cooldown(
    checkpoint: &mut AgentCheckpoint,
    step_id: usize,
) -> Option<String> {
    if checkpoint.gate_human_escape_directed {
        checkpoint.gate_human_escape_directed = false;
        return None;
    }
    let blocked_step = checkpoint.recent_gate_block_step?;
    if step_id.saturating_sub(blocked_step as usize) > 1 {
        checkpoint.recent_gate_block_step = None;
        return None;
    }
    checkpoint.recent_gate_block_step = None;
    Some(
        "【❌ 求助失败：本次 ask_human 已被宿主冷却拒绝（先监听，后求助）】\n【原因】刚刚被系统门禁拦截，尚未做出任何被拦截意见要求的行动即直接呼叫人类。\n【必须执行的操作】：\n请先执行拦截意见中要求的动作（重新取证 read/grep、按审查建议修改方案、重新派发 child_reviewer 复审，或调用 wait_jobs 挂起等待后台结论）；\n完成至少一步有效行动后若仍受阻，再向人类求助并附上你已尝试的动作与结果。".to_string(),
    )
}

/// submit_plan 编排已拆至 crate::agent::plan_submit::handle_submit_plan（此处仅 re-export）。
use crate::agent::plan_submit::handle_submit_plan;

mod dispatch_ask_human;
mod dispatch_child_reviewer;
mod dispatch_child_worker;
mod dispatch_child_writer;
mod dispatch_jobs;
mod dispatch_local;
mod dispatch_resume_child;
mod dispatch_view_image;
mod dispatch_watch;

/// 派发臂共享上下文：run_tool_phase 的入参打包，供各 dispatch_* 子模块使用。
pub(super) struct DispatchCtx<'a> {
    pub app: &'a AppHandle,
    pub state: &'a State<'a, DbState>,
    pub registry_state: &'a State<'a, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    pub model_routing: &'a crate::agent::router::ModelRoutingConfig,
    pub parent_run_id: &'a str,
    pub workspace_dir: &'a Path,
    pub final_session_id: &'a String,
    pub step_id: usize,
    pub tool_started_at: Instant,
    pub context: &'a mut crate::agent::context::SandwichContext,
    pub checkpoint: &'a mut AgentCheckpoint,
    pub run_machine: &'a mut AgentRunMachine,
    pub rx: &'a mut mpsc::Receiver<HumanControl>,
    pub interrupt_rx: &'a mut watch::Receiver<u64>,
}

/// 派发臂结果：Early = 分支内已自行发射工具事件，主流程直接透传；
/// Done = 常规结果，由 run_tool_phase 尾部统一发射工具事件。
pub(super) enum ArmOutcome {
    Early(DispatchResult),
    Done(DispatchResult),
}

/// 执行持久化状态机的 RunTool 阶段。
///
/// 调用前为未知或可能有副作用的操作创建 pending checkpoint；人工等待也以
/// WaitUser 阶段保存。所有旧进度事件仍由该处理器原样发送。
pub async fn run_tool_phase(
    app: &AppHandle,
    state: &State<'_, DbState>,
    registry_state: &State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    model_routing: &crate::agent::router::ModelRoutingConfig,
    parent_run_id: &str,
    workspace_dir: &Path,
    final_session_id: &String,
    step_id: usize,
    inst: &AgentInstruction,
    context: &mut crate::agent::context::SandwichContext,
    checkpoint: &mut AgentCheckpoint,
    run_machine: &mut AgentRunMachine,
    rx: &mut mpsc::Receiver<HumanControl>,
    interrupt_rx: &mut watch::Receiver<u64>,
) -> Result<DispatchResult, String> {
    // 顶层动作误写为工具调用的兼容解包：{"tool":"submit_plan","command":{...}}
    // 实测跨场复犯的格式摩擦，就地归一为独立动作（语义等价），免打回重学。
    let normalized_inst;
    let inst = match inst.unwrap_standalone_tool_alias() {
        Some(n) => {
            normalized_inst = n;
            &normalized_inst
        }
        None => inst,
    };
    let instruction_tool = inst.get_tool();
    let instruction_action = inst.get_action();
    crate::agent::control::mark_stage(
        final_session_id,
        &format!("dispatch:{}:{}", instruction_tool, instruction_action),
    );
    // submit_plan 优先裁决：它是探索期的合法收敛出口，独立于探索预算门禁与工具派发链。
    // 与工具调用混发属协议误用（提交语义含糊），直接拒绝让模型单独重提。
    let submit_plan_request = inst.submit_plan.clone();

    // 明确失败动作先于业务门禁和任何工具副作用；混发的工具及状态更新均不执行。
    if let Some(request) = &inst.fail_task {
        let result = crate::agent::failure::failure_action_result(request);
        if result.route == "task:failed" {
            checkpoint.pending_task_failure = Some(request.reason.trim().to_string());
        }
        emit_tool_event(app, final_session_id, step_id, inst, "failed",
            Some(result.route.clone()), Some(false), &result.stderr, Some(0), None);
        return Ok(result);
    }

    // 参数错误不进入审查/冷却/读取台账：预检只读，不修改 checkpoint。
    if let Some(result) = crate::agent::tools::input_validation::instruction_rejection(state.inner(), inst) {
        emit_tool_event(app, final_session_id, step_id, inst, "failed",
            Some(result.route.clone()), Some(false), &result.stderr, Some(0), None);
        return Ok(result);
    }

    if let Some(decision) = &inst.human_reply_decision {
        if let Err(error) = checkpoint.apply_human_reply_decision(state.inner(), context, decision) {
            return Ok(DispatchResult { view: None, success: false, stdout: String::new(), stderr: error, route: "human_reply:invalid_source".into() });
        }
        save_checkpoint(state.inner(), checkpoint)?;
        if !inst.has_executable_entry() {
            return Ok(DispatchResult { view: None, success: true, stdout: "已记录主模型对本次人类回复的判断。".into(), stderr: String::new(), route: "human_reply:resolved".into() });
        }
    }

    // --- B. 执行动作 ---
    if let Some(reason) = context.repeated_failure_blocker(
        &inst.get_tool(),
        &inst.get_action(),
        &inst.get_params(),
    ) {
        return Ok(DispatchResult { view: None,
            success: false,
            stdout: String::new(),
            stderr: reason,
            route: REPEATED_FAILURE_BLOCKED_ROUTE.to_string(),
        });
    }

    // --- B0. 重复读取硬拦截：同工具+同路径第 3 次读取且内容未变 → 拒绝并给抓手 ---
    if let Some(block_msg) = context.check_repeat_read_dispatch(inst, workspace_dir) {
        let _ = app.emit(
            &format!("agent-log-{}", final_session_id),
            "🛑 重复读取已拦截：内容未变化，强制切换策略".to_string(),
        );
        return Ok(DispatchResult { view: None,
            success: false,
            stdout: String::new(),
            stderr: block_msg,
            route: "host_rejection:repeat_read".to_string(),
        });
    }

    app.emit(
        &format!("agent-log-{}", final_session_id),
        format!("▶ 步骤 {}: {}", step_id + 1, inst.description),
    )
    .map_err(|e| e.to_string())?;
    app.emit(
        &format!("agent-progress-{}", final_session_id),
        json!({
            "type": "step_start",
            "step_id": step_id,
            "description": &inst.description
        }),
    )
    .map_err(|e| e.to_string())?;
    let tool_started_at = Instant::now();
    emit_tool_event(
        app,
        final_session_id,
        step_id,
        inst,
        "started",
        None,
        None,
        "",
        None,
        None,
    );

    if let Some(submission) = submit_plan_request {
        let result = if inst.has_executable_entry() {
            DispatchResult { view: None,
                success: false,
                stdout: String::new(),
                stderr: "【⚠️ submit_plan 协议误用】方案提交是独立的状态转换动作，必须与工具调用分步提交。\n请单独输出 {\"submit_plan\": {...}}，不要附带 tool/action/params。".to_string(),
                route: "plan_liveness:submit_plan_mixed".to_string(),
            }
        } else {
            handle_submit_plan(
                app,
                state.inner(),
                model_routing,
                parent_run_id,
                workspace_dir,
                final_session_id,
                step_id,
                &submission,
                context,
                checkpoint,
                interrupt_rx.clone(),
            )
            .await
        };
        let mut result = result;
        if let Some(notice) = inst.ignored_mixed_entry_notice() {
            if result.success {
                result.stdout.push_str(&notice);
            } else {
                result.stderr.push_str(&notice);
            }
        }
        crate::agent::telemetry::record_gate_fire(
            state.inner(),
            &crate::agent::telemetry::GateFireRecord {
                run_id: parent_run_id,
                session_id: final_session_id,
                step_id: step_id as i64,
                gate: "plan_liveness",
                route: &result.route,
            },
        );
        emit_tool_event(
            app,
            final_session_id,
            step_id,
            inst,
            if result.success { "completed" } else { "failed" },
            Some(result.route.clone()),
            Some(result.success),
            if result.success { &result.stdout } else { &result.stderr },
            Some(tool_started_at.elapsed().as_millis()),
            None,
        );
        return Ok(result);
    }

    // request_retriage 早路由：重新分流复核是独立的状态判断动作，必须与工具调用分步提交。
    // 宿主只裁决协议形状与配额，把证据包交给 supervisor 档模型复核，判决落地见
    // retriage::apply_retriage_revision（窄化/拆分/重规划/升维，绝不放宽任何门禁）。
    if let Some(request) = inst.request_retriage.clone() {
        let result = if inst.has_executable_entry() {
            DispatchResult { view: None,
                success: false,
                stdout: String::new(),
                stderr: "【⚠️ request_retriage 协议误用】重新分流申请是独立的状态判断动作，必须与工具调用分步提交。\n请单独输出 {\"request_retriage\": {\"reason\": \"...\", \"unresolved_questions\": [...]}}，不要附带 tool/action/params。".to_string(),
                route: "plan_liveness:retriage_mixed".to_string(),
            }
        } else if request.reason.trim().is_empty() {
            DispatchResult { view: None,
                success: false,
                stdout: String::new(),
                stderr: "【⚠️ request_retriage 已拒绝】必须填写 reason：发现了什么证据让原分流判决可能失效。".to_string(),
                route: "plan_liveness:retriage_rejected".to_string(),
            }
        } else if checkpoint.retriage_used >= crate::agent::retriage::max_retriage_per_run() {
            DispatchResult { view: None,
                success: false,
                stdout: String::new(),
                stderr: format!(
                    "【⚠️ request_retriage 已拒绝】本轮分流复核配额已用尽（{} 次）。请基于现有证据继续：证据成熟则 submit_plan，方向存疑则先收敛取证范围。",
                    crate::agent::retriage::max_retriage_per_run()
                ),
                route: "plan_liveness:retriage_rejected".to_string(),
            }
        } else {
            let revision = crate::agent::retriage::run_retriage(
                app,
                state.inner(),
                model_routing,
                checkpoint,
                &context.user_goal,
                "agent_request",
                &request.reason,
                &request.unresolved_questions,
                final_session_id,
                interrupt_rx.clone(),
            )
            .await;
            let decision = revision.decision.clone();
            let feedback =
                crate::agent::retriage::apply_retriage_revision(context, checkpoint, &revision);
            let _ = app.emit(
                &format!("agent-log-{}", final_session_id),
                format!("🧭 分流复核: {} — {}", decision, revision.reason),
            );
            DispatchResult { view: None,
                success: true,
                stdout: feedback,
                stderr: String::new(),
                route: format!("retriage:{}", decision),
            }
        };
        crate::agent::telemetry::record_gate_fire(
            state.inner(),
            &crate::agent::telemetry::GateFireRecord {
                run_id: parent_run_id,
                session_id: final_session_id,
                step_id: step_id as i64,
                gate: "retriage",
                route: &result.route,
            },
        );
        emit_tool_event(
            app,
            final_session_id,
            step_id,
            inst,
            if result.success { "completed" } else { "failed" },
            Some(result.route.clone()),
            Some(result.success),
            if result.success { &result.stdout } else { &result.stderr },
            Some(tool_started_at.elapsed().as_millis()),
            None,
        );
        return Ok(result);
    }

    // prepare_finish 早路由：它只是标记——真正的门禁评估在 main_loop 终态段执行
    // （那里才有 open_todos/义务验证/宿主验证/终审的完整上下文）。成功路径不在此处
    // emit 完成事件，避免与 main_loop 的权威收据事件重复。
    if inst.prepare_finish.is_some() {
        let result = if inst.has_executable_entry() {
            DispatchResult { view: None,
                success: false,
                stdout: String::new(),
                stderr: "【⚠️ prepare_finish 协议误用】终态预检是独立的评估动作，必须与工具调用分步提交。\n请单独输出 {\"prepare_finish\": {\"summary\": \"...\"}}，不要附带 tool/action/params。".to_string(),
                route: "finalization:prepare_mixed".to_string(),
            }
        } else {
            DispatchResult { view: None,
                success: true,
                stdout: inst
                    .prepare_finish
                    .as_ref()
                    .and_then(|req| req.summary.clone().or_else(|| req.details.clone()))
                    .unwrap_or_else(|| "终态预检".to_string()),
                stderr: String::new(),
                route: "prepare_finish".to_string(),
            }
        };
        crate::agent::telemetry::record_gate_fire(
            state.inner(),
            &crate::agent::telemetry::GateFireRecord {
                run_id: parent_run_id,
                session_id: final_session_id,
                step_id: step_id as i64,
                gate: "finalization",
                route: &result.route,
            },
        );
        if !result.success {
            emit_tool_event(
                app,
                final_session_id,
                step_id,
                inst,
                "failed",
                Some(result.route.clone()),
                Some(false),
                &result.stderr,
                Some(tool_started_at.elapsed().as_millis()),
                None,
            );
        }
        return Ok(result);
    }

    // --- B1. 协议终态：finish 不创建 pending、不派发工具，由主循环在终态自检裁决后统一收敛状态与发送事件。 ---
    if is_finish_instruction(inst) {
        let result = DispatchResult { view: None,
            stdout: crate::agent::finalization::finish_output(inst),
            stderr: String::new(),
            success: true,
            route: "finish".to_string(),
        };
        // 不在此处盲目广播 "succeeded"，交由 main_loop 的 finish guard 判定是否为 finish_challenged 统一发射事件
        return Ok(result);
    }

    // --- B1.5 P4 连续批发派发解析失败降级熔断检查 ---
    if let Err(breaker_msg) = context.check_batch_dispatch_allowed(inst.has_dispatches()) {
        return Ok(DispatchResult { view: None,
            stdout: String::new(),
            stderr: breaker_msg,
            success: false,
            route: "host_rejection:batch_circuit_breaker".to_string(),
        });
    }

    // --- B1.8 前置方案门禁拦截：未获得方案审批前，禁止直接调用破坏性修改工具或派发写入类子 Agent ---
    let is_write_tool = is_write_or_side_effect_tool(inst)
        || inst.get_tool().eq_ignore_ascii_case("child_writer")
        || inst.get_action().eq_ignore_ascii_case("child_writer");

    if is_write_tool {
        // 僵尸 blocker 清账：blocker 出生阶段已被宿主判分越过 → 自动清零放行
        // （final_only 记账带入收尾）。oracle 缺失/未越过时不动，对质与仲裁仍是出口。
        if checkpoint.stage_review_blockers_open > 0 {
            if let Some((idx, _, _)) = crate::agent::eval_oracle::query_active_progress().await {
                if let Some(note) = checkpoint.clear_stale_stage_blockers(idx as u32) {
                    let _ = app.emit(
                        &format!("agent-log-{}", final_session_id),
                        note,
                    );
                }
            }
        }
        // 🧭 任务入口分流驱动的方案门判定（TriageVerdict 已在 main_loop 落 checkpoint）：
        // 有效判决 → route 决定是否需要方案门；失败兜底仅豁免 auto 模式下的本会话 tmp 写入。
        // 机械安全地板（删改名动作、多文件扩散、
        // 用户显式选择的方案审批模式）不被判决推翻；
        // 无判决（分流未执行的旧 checkpoint 恢复）→ 退回机械兜底辅助：
        // 零爆炸半径小活（tmp/ 产物、全新文件、小编辑）仍放行，其余维持方案门。
        let floor_reason =
            crate::agent::triage::write_floor_violation(inst, context, final_session_id);
        let plan_needed = crate::agent::plan_gate::pre_write_plan_required(
            checkpoint, context, inst, final_session_id, workspace_dir,
        );
        // 判决被机械地板推翻 = 升级信号，记遥测供观测误分流率（每次命中都记）
        if let Some(reason) = floor_reason {
            if checkpoint
                .triage_verdict
                .as_ref()
                .is_some_and(|v| !v.requires_plan_gate())
            {
                crate::agent::telemetry::record_gate_fire(
                    state.inner(),
                    &crate::agent::telemetry::GateFireRecord {
                        run_id: parent_run_id,
                        session_id: &final_session_id,
                        step_id: step_id as i64,
                        gate: "triage",
                        route: &format!("triage_escalated:{}", reason),
                    },
                );
            }
        }
        let rejection = if plan_needed {
            check_pre_write_gatekeeper(
                checkpoint,
                context,
                state.inner(),
                parent_run_id,
                step_id,
                false,
            )
        } else {
            // 分流放行 ≠ 放行一切：已判出的阶段 blocker（中间态错误）仍须拦住写入
            crate::agent::plan_gate::stage_blocker_interception(checkpoint, state.inner())
        };
        if let Some(rejection) = rejection {
            crate::agent::telemetry::record_gate_fire(
                state.inner(),
                &crate::agent::telemetry::GateFireRecord {
                    run_id: parent_run_id,
                    session_id: &final_session_id,
                    step_id: step_id as i64,
                    gate: "pre_write",
                    route: &rejection.route,
                },
            );
            emit_tool_event(
                app,
                final_session_id,
                step_id,
                inst,
                "failed",
                Some(rejection.route.clone()),
                Some(false),
                &rejection.stderr,
                Some(tool_started_at.elapsed().as_millis()),
                None,
            );
            return Ok(rejection);
        }
        if !plan_needed && context.side_effects.len() == context.inherited_side_effects_count {
            let route = checkpoint
                .triage_verdict
                .as_ref()
                .map(|v| if v.fallback.is_some() { "fallback_session_tmp" } else { v.route.as_str() })
                .unwrap_or("mechanical_fallback");
            let _ = app.emit(
                &format!("agent-log-{}", final_session_id),
                format!("🧭 本次写入不触发方案门：适用路径={}；后续操作仍逐步判定", route),
            );
        }
    }

    // --- B2. 受限 child reviewer：异步派发，不阻塞主循环。结果在后续 step 中由 main_loop 的 poll_completed_child_runs 注入。 ---
    let mut ctx = DispatchCtx {
        app,
        state,
        registry_state,
        model_routing,
        parent_run_id,
        workspace_dir,
        final_session_id,
        step_id,
        tool_started_at,
        context,
        checkpoint,
        run_machine,
        rx,
        interrupt_rx,
    };

    let dispatch_result = if inst.get_tool().eq_ignore_ascii_case("child_reviewer") {
        match dispatch_child_reviewer::dispatch_child_reviewer(&mut ctx, inst).await? {
            ArmOutcome::Early(r) => return Ok(r),
            ArmOutcome::Done(r) => r,
        }
    } else if inst.get_tool().eq_ignore_ascii_case("watch_start")
        || inst.get_action().eq_ignore_ascii_case("watch_start")
    {
        match dispatch_watch::dispatch_watch_start(&mut ctx, inst).await? {
            ArmOutcome::Early(r) => return Ok(r),
            ArmOutcome::Done(r) => r,
        }
    } else if inst.get_tool().eq_ignore_ascii_case("watch_stop")
        || inst.get_action().eq_ignore_ascii_case("watch_stop")
    {
        match dispatch_watch::dispatch_watch_stop(&mut ctx, inst).await? {
            ArmOutcome::Early(r) => return Ok(r),
            ArmOutcome::Done(r) => r,
        }
    } else if inst.get_tool().eq_ignore_ascii_case("child_writer")
        || inst.get_action().eq_ignore_ascii_case("child_writer")
    {
        match dispatch_child_writer::dispatch_child_writer(&mut ctx, inst).await? {
            ArmOutcome::Early(r) => return Ok(r),
            ArmOutcome::Done(r) => r,
        }
    } else if inst.get_tool().eq_ignore_ascii_case("child_worker")
        || inst.get_action().eq_ignore_ascii_case("child_worker")
    {
        match dispatch_child_worker::dispatch_child_worker(&mut ctx, inst).await? {
            ArmOutcome::Early(r) => return Ok(r),
            ArmOutcome::Done(r) => r,
        }
    } else if inst.get_tool().eq_ignore_ascii_case("wait_jobs")
        || inst.get_tool().eq_ignore_ascii_case("wait_job")
        || inst.get_action().eq_ignore_ascii_case("wait_jobs")
    {
        match dispatch_jobs::dispatch_wait_jobs(&mut ctx, inst).await? {
            ArmOutcome::Early(r) => return Ok(r),
            ArmOutcome::Done(r) => r,
        }
    } else if inst.get_tool().eq_ignore_ascii_case("job_cancel")
        || inst.get_action().eq_ignore_ascii_case("job_cancel")
    {
        match dispatch_jobs::dispatch_job_cancel(&mut ctx, inst).await? {
            ArmOutcome::Early(r) => return Ok(r),
            ArmOutcome::Done(r) => r,
        }
    } else if inst.get_tool().eq_ignore_ascii_case("resume_child")
        || inst.get_action().eq_ignore_ascii_case("resume_child")
    {
        match dispatch_resume_child::dispatch_resume_child(&mut ctx, inst).await? {
            ArmOutcome::Early(r) => return Ok(r),
            ArmOutcome::Done(r) => r,
        }
    } else if inst.get_tool().eq_ignore_ascii_case("view_image")
        || inst.get_action().eq_ignore_ascii_case("view_image")
    {
        match dispatch_view_image::dispatch_view_image(&mut ctx, inst).await? {
            ArmOutcome::Early(r) => return Ok(r),
            ArmOutcome::Done(r) => r,
        }
    } else if inst.get_tool().eq_ignore_ascii_case("ask_human") || inst.get_action().eq_ignore_ascii_case("ask") {
        match dispatch_ask_human::dispatch_ask_human(&mut ctx, inst).await? {
            ArmOutcome::Early(r) => return Ok(r),
            ArmOutcome::Done(r) => r,
        }
    } else {
        match dispatch_local::dispatch_local_tool(&mut ctx, inst).await? {
            ArmOutcome::Early(r) => return Ok(r),
            ArmOutcome::Done(r) => r,
        }
    };

    let event_type = if dispatch_result.success {
        "succeeded"
    } else {
        "failed"
    };
    let result_text = if !dispatch_result.stdout.is_empty() {
        dispatch_result.stdout.as_str()
    } else {
        dispatch_result.stderr.as_str()
    };
    emit_tool_event(
        app,
        final_session_id,
        step_id,
        inst,
        event_type,
        Some(dispatch_result.route.clone()),
        Some(dispatch_result.success),
        result_text,
        Some(tool_started_at.elapsed().as_millis()),
        dispatch_result.view.as_ref(),
    );

    Ok(dispatch_result)
}

