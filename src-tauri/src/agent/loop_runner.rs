//! RunTool 阶段执行器：工具派发、子 Agent 调度与人工介入处理。
//! 既有外部引用路径 crate::agent::loop_runner::* 通过下方 re-export 全部维持不变。

use crate::agent::checkpoint::{save_checkpoint, AgentCheckpoint};
use crate::agent::child_reviewer::{result_as_dispatch_output, spawn_child_reviewer};
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
        "【先监听，后求助】你刚刚被系统门禁拦截，尚未做出任何被拦截意见要求的行动就呼叫人类，本次求助已被宿主冷却拒绝。\n请先执行拦截意见中要求的动作：重新取证（read/grep）、按审查建议修改实现、重新派发 child_reviewer 复审，或调用 wait_jobs 挂起等待后台结论；\n完成至少一步有效动作后，若问题仍未解决，再向人类求助并附上你已尝试的动作与结果。".to_string(),
    )
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
    mut checkpoint: &mut AgentCheckpoint,
    mut run_machine: &mut AgentRunMachine,
    rx: &mut mpsc::Receiver<HumanControl>,
    interrupt_rx: &mut watch::Receiver<u64>,
) -> Result<DispatchResult, String> {
    // --- B. 执行动作 ---
    if let Some(reason) =
        context.repeated_failure_blocker(&inst.get_tool(), &inst.get_action(), &inst.get_params())
    {
        return Ok(DispatchResult {
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
        return Ok(DispatchResult {
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
    );

    // --- B1. 协议终态：finish 不创建 pending、不派发工具，由主循环在终态自检裁决后统一收敛状态与发送事件。 ---
    if is_finish_instruction(inst) {
        let result = DispatchResult {
            stdout: inst
                .command
                .as_ref()
                .and_then(|value| value.get("summary").or_else(|| value.get("details")))
                .and_then(|value| value.as_str())
                .unwrap_or("任务已完成")
                .to_string(),
            stderr: String::new(),
            success: true,
            route: "finish".to_string(),
        };
        // 不在此处盲目广播 "succeeded"，交由 main_loop 的 finish guard 判定是否为 finish_challenged 统一发射事件
        return Ok(result);
    }

    // --- B1.5 P4 连续批发派发解析失败降级熔断检查 ---
    if let Err(breaker_msg) = context.check_batch_dispatch_allowed(inst.has_dispatches()) {
        return Ok(DispatchResult {
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
        if let Some(rejection) = check_pre_write_gatekeeper(
            checkpoint,
            context,
            state.inner(),
            parent_run_id,
            step_id,
            false,
        ) {
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
            );
            return Ok(rejection);
        }
    }

    // --- B2. 受限 child reviewer：异步派发，不阻塞主循环。结果在后续 step 中由 main_loop 的 poll_completed_child_runs 注入。 ---
    let dispatch_result = if inst.get_tool().eq_ignore_ascii_case("child_reviewer") {
        // 记录派发前的消费状态：若本次派发一个任务都没起来，需原样回滚，避免白耗一轮审查预算。
        let pre_dispatch_consumed = checkpoint.plan_reviewer_consumed;
        // 方案审查轮数上限只约束批准前的方案审查讨论；
        // 已批准后的 child_reviewer 属于交付验证等其他用途，不消耗该预算、也不改变方案阶段。
        if !checkpoint.plan_phase.is_approved() {
            if checkpoint.plan_reviewer_retry_count
                >= crate::agent::checkpoint::MAX_PLAN_REVIEWER_RETRIES
            {
                let limit_msg = crate::agent::gate_messages::reviewer_retry_limit();
                let result = DispatchResult {
                    stdout: String::new(),
                    stderr: limit_msg,
                    success: false,
                    route: "plan_gatekeeper:reviewer_retry_limit".to_string(),
                };
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
                );
                return Ok(result);
            }

            // 仅在尚未批准时进入审查中阶段；若已批准，保留 Approved 状态，避免后续补充审查打回 Reviewing 阻塞修改
            if !checkpoint.plan_phase.is_approved() {
                checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Reviewing;
            }
            // 统一走 reserve：与反驳对质共用同一预扣入口，预算语义单一事实源
            if checkpoint.reserve_plan_review_round().is_err() {
                // 上方已做过上限检查，正常不会到这里；兜底不递增
            }
        }

        let routing = crate::agent::router::AgentRouter::route_for_role(
            context,
            model_routing,
            step_id,
            crate::agent::router::roles::REVIEWER,
        );
        let model_tier = routing.tier.to_string();
        let reasoning_effort = model_routing.reasoning_effort_for(&routing.tier).to_string();
        let items = inst.get_dispatch_items();
        let total = items.len();
        let is_batch = total > 1;

        let mut item_reports = Vec::new();
        let mut spawned_ids = Vec::new();
        let mut success_count = 0;
        let mut fail_count = 0;

        for (idx, child_params) in items.iter().enumerate() {
            let item_num = idx + 1;
            let mut reviewer_persona = String::new();
            let mut reviewer_model = routing.model_id.clone();
            let mut effective_params = child_params.clone();
            if let Some(obj) = effective_params.as_object_mut() {
                obj.remove("max_concurrency");
            }

            if let Some(role_name) = child_params.get("role").and_then(|v| v.as_str()) {
                match crate::agent::tools::role_manager::resolve_enabled_card(state.inner(), role_name) {
                    Ok(Some(card)) => {
                        if !card.allows_kind("reviewer") {
                            item_reports.push(format!(
                                "[{}/{}] ❌ 角色卡「{}」未授权挂载为 reviewer（允许类型: {}）。",
                                item_num, total, role_name, card.allowed_kinds
                            ));
                            fail_count += 1;
                            continue;
                        }
                        reviewer_persona = crate::agent::tools::role_manager::persona_block(&card);
                        if !card.model_id.is_empty() {
                            reviewer_model = card.model_id.clone();
                        }
                        if card.max_concurrency > 0 {
                            effective_params["max_concurrency"] = json!(card.max_concurrency.min(crate::agent::child_reviewer::MAX_TOTAL_CHILDREN_PER_RUN as i64));
                        }
                    }
                    Ok(None) => {
                        item_reports.push(format!(
                            "[{}/{}] ❌ 角色卡不存在或已禁用：{}；可用角色见角色池菜单，或先用 role_manager.upsert 创建。",
                            item_num, total, role_name
                        ));
                        fail_count += 1;
                        continue;
                    }
                    Err(error) => {
                        item_reports.push(format!(
                            "[{}/{}] ❌ 角色卡查询失败：{}。",
                            item_num, total, error
                        ));
                        fail_count += 1;
                        continue;
                    }
                }
                if reviewer_model == routing.model_id {
                    // 未被角色卡钉死时应用配置级异构优选；说明写进派发报告，人工可见
                    let (preferred, note) = crate::agent::router::prefer_cross_family_reviewer(
                        &*state.inner().0.lock().unwrap(),
                        &reviewer_model,
                    );
                    reviewer_model = preferred;
                    if let Some(note) = note {
                        item_reports.push(format!("[{}/{}]{}", item_num, total, note));
                    }
                }
            }

            match spawn_child_reviewer(
                Some(app.clone()),
                state.inner(),
                parent_run_id,
                final_session_id,
                workspace_dir,
                &reviewer_model,
                &model_tier,
                &reasoning_effort,
                &effective_params,
                &reviewer_persona,
                interrupt_rx.clone(),
            )
            .await
            {
                Ok(child_run_id) => {
                    let question_str = effective_params.get("question").and_then(|v| v.as_str()).unwrap_or("");
                    let question_brief = if question_str.chars().count() > 40 {
                        format!("{}...", question_str.chars().take(40).collect::<String>())
                    } else {
                        question_str.to_string()
                    };
                    item_reports.push(format!(
                        "[{}/{}] ✅ 已派发 child_run_id={} 审查任务: {}",
                        item_num, total, child_run_id, question_brief
                    ));
                    spawned_ids.push(child_run_id);
                    success_count += 1;
                }
                Err(error) => {
                    item_reports.push(format!(
                        "[{}/{}] ❌ 派发失败：{}",
                        item_num, total, error
                    ));
                    fail_count += 1;
                }
            }
        }

        // 一个审查任务都没起来（角色卡未授权/不存在、参数非法等）时回滚本轮审查预算：
        // 否则失败也会消耗仅有的 2 轮额度，额度耗尽后派发被限流、结论又永远收不到，
        // 交叉审查模式将陷入「既派不出、也写不了」的永久阻塞。
        if success_count == 0 && !checkpoint.plan_phase.is_approved() {
            checkpoint.rollback_plan_review_dispatch(pre_dispatch_consumed);
        }

        if !is_batch {
            if success_count == 1 {
                let child_run_id = &spawned_ids[0];
                let mut stdout = format!(
                    "子审查任务已异步派发，child_run_id: {}。需要结论时调用 wait_jobs(job_ids:[\"{}\"]) 挂起等待，返回值中将直接给出完整审查结论；若先执行其他独立操作，结果也会在后续步骤自动注入。",
                    child_run_id, child_run_id
                );
                if inst.is_mixed_dispatch_format() {
                    stdout = format!("⚠️ 检测到混合格式（同时包含 dispatches 与单发字段），系统已优先采用 dispatches。\n\n{}", stdout);
                }
                DispatchResult {
                    stdout,
                    stderr: String::new(),
                    success: true,
                    route: "child_reviewer:spawned".to_string(),
                }
            } else {
                let err_msg = item_reports.into_iter().next().unwrap_or_else(|| "派发失败".to_string());
                DispatchResult {
                    stdout: String::new(),
                    stderr: err_msg,
                    success: false,
                    route: "child_reviewer:rejected_or_failed".to_string(),
                }
            }
        } else {
            let mut summary = format!(
                "已批量处理 {} 个审查任务派发请求（{} 成功，{} 失败）：\n{}",
                total, success_count, fail_count, item_reports.join("\n")
            );
            if !spawned_ids.is_empty() {
                let ids_formatted: Vec<String> = spawned_ids.iter().map(|id| format!("\"{}\"", id)).collect();
                summary.push_str(&format!(
                    "\n\n已成功派发的审查任务均在后台异步执行；需要结论时调用 wait_jobs(job_ids:[{}]) 挂起等待，或先推进其他独立操作。",
                    ids_formatted.join(", ")
                ));
            }
            if inst.is_mixed_dispatch_format() {
                summary = format!("⚠️ 检测到混合格式（同时包含 dispatches 与单发字段），系统已优先采用 dispatches 批量派发。\n\n{}", summary);
            }
            let breaker_alert = context.record_batch_dispatch_outcome(success_count > 0);
            if success_count > 0 {
                DispatchResult {
                    stdout: summary,
                    stderr: String::new(),
                    success: true,
                    route: format!("child_reviewer:batch:{}ok{}fail", success_count, fail_count),
                }
            } else {
                let mut err = format!("批量审查派发全部失败（共 {} 项）：\n{}", total, item_reports.join("\n"));
                if let Some(alert) = breaker_alert {
                    err = format!("{}\n\n{}", err, alert);
                }
                DispatchResult {
                    stdout: String::new(),
                    stderr: err,
                    success: false,
                    route: "child_reviewer:rejected_or_failed".to_string(),
                }
            }
        }
    // --- B2.6 持续监听：注册周期巡检，外部变化以 watch_event 经 durable inbox 自动注入 ---
    } else if inst.get_tool().eq_ignore_ascii_case("watch_start")
        || inst.get_action().eq_ignore_ascii_case("watch_start")
    {
        let params = inst.get_params();
        let watch_outcome = async {
            let request = crate::agent::watch_engine::parse_watch_request(&params)?;
            crate::agent::watch_engine::begin_watch(
                state.inner(),
                parent_run_id,
                final_session_id,
                &request,
                Some(workspace_dir),
            )
            .await
        };
        match watch_outcome.await {
            Ok(watch_id) => {
                if let Some(watch_state) =
                    crate::agent::watch_engine::load_watch_state(state.inner(), &watch_id)
                {
                    let db_clone = crate::db::DbState(state.inner().0.clone());
                    let session_clone = final_session_id.clone();
                    let watch_clone = watch_id.clone();
                    let workspace_clone = workspace_dir.to_path_buf();
                    tokio::spawn(crate::agent::watch_engine::run_watch_loop(
                        db_clone,
                        Some(app.clone()),
                        session_clone,
                        watch_clone,
                        watch_state.interval_secs,
                        watch_state.last_snapshot,
                        Some(workspace_clone),
                    ));
                }
                let _ = app.emit(
                    &format!("agent-log-{}", final_session_id),
                    format!("👁️ 持续监听已启动: {}", watch_id),
                );
                DispatchResult {
                    stdout: format!(
                        "持续监听已启动，watch_id: {}。此后每个巡检周期内若目标发生变化，变化事件会作为 watch_event 自动注入你的上下文（无需轮询读取）；若长时间无变化则不会产生任何事件。任务收尾前请调用 watch_stop 释放该监听，否则 finish 会被拦截。",
                        watch_id
                    ),
                    stderr: String::new(),
                    success: true,
                    route: "watch:started".to_string(),
                }
            }
            Err(error) => DispatchResult {
                stdout: String::new(),
                stderr: error,
                success: false,
                route: "watch:rejected".to_string(),
            },
        }
    } else if inst.get_tool().eq_ignore_ascii_case("watch_stop")
        || inst.get_action().eq_ignore_ascii_case("watch_stop")
    {
        let params = inst.get_params();
        let stop_outcome = match params.get("watch_id").and_then(|v| v.as_str()) {
            Some(watch_id) => {
                crate::agent::watch_engine::stop_watch(state.inner(), parent_run_id, watch_id)
                    .map(|rows| format!("已停止监听 {}（{} 行）", watch_id, rows))
            }
            None => crate::agent::watch_engine::stop_all_watches(state.inner(), parent_run_id)
                .map(|rows| format!("已停止全部持续监听（{} 行）", rows)),
        };
        match stop_outcome {
            Ok(summary) => DispatchResult {
                stdout: summary,
                stderr: String::new(),
                success: true,
                route: "watch:stopped".to_string(),
            },
            Err(error) => DispatchResult {
                stdout: String::new(),
                stderr: error,
                success: false,
                route: "watch:stop_failed".to_string(),
            },
        }
    // --- B2.2 L3 隔离写入 worker：私有副本上改码跑测试，diff 回执，root 仍是真实工作区唯一写入者 ---
    } else if inst.get_tool().eq_ignore_ascii_case("child_writer")
        || inst.get_action().eq_ignore_ascii_case("child_writer")
    {
        let routing = crate::agent::router::AgentRouter::route_for_role(
            context,
            model_routing,
            step_id,
            crate::agent::router::roles::WRITER,
        );
        let reasoning_effort = model_routing.reasoning_effort_for(&routing.tier).to_string();
        let items = inst.get_dispatch_items();
        let total = items.len();
        let is_batch = total > 1;

        let mut item_reports = Vec::new();
        let mut spawned_ids = Vec::new();
        let mut success_count = 0;
        let mut fail_count = 0;

        for (idx, writer_params) in items.iter().enumerate() {
            let item_num = idx + 1;
            let mut writer_persona = String::new();
            let mut writer_model = routing.model_id.clone();
            let mut effective_params = writer_params.clone();
            if let Some(obj) = effective_params.as_object_mut() {
                obj.remove("max_concurrency");
            }

            if let Some(role_name) = writer_params.get("role").and_then(|v| v.as_str()) {
                match crate::agent::tools::role_manager::resolve_enabled_card(state.inner(), role_name) {
                    Ok(Some(card)) => {
                        if !card.allows_kind("writer") {
                            item_reports.push(format!(
                                "[{}/{}] ❌ 角色卡「{}」未授权挂载为 writer（允许类型: {}）。",
                                item_num, total, role_name, card.allowed_kinds
                            ));
                            fail_count += 1;
                            continue;
                        }
                        writer_persona = crate::agent::tools::role_manager::persona_block(&card);
                        if !card.model_id.is_empty() {
                            writer_model = card.model_id.clone();
                        }
                        effective_params = crate::agent::tools::role_manager::merge_writer_defaults(&card, writer_params);
                        if let Some(obj) = effective_params.as_object_mut() {
                            obj.remove("max_concurrency");
                        }
                        if !card.hard_scope.is_empty() {
                            effective_params["hard_scope"] = json!(card.hard_scope);
                        }
                        if card.max_concurrency > 0 {
                            effective_params["max_concurrency"] = json!(card.max_concurrency.min(crate::agent::child_reviewer::MAX_TOTAL_CHILDREN_PER_RUN as i64));
                        }
                    }
                    Ok(None) => {
                        item_reports.push(format!(
                            "[{}/{}] ❌ 角色卡不存在或已禁用：{}。",
                            item_num, total, role_name
                        ));
                        fail_count += 1;
                        continue;
                    }
                    Err(error) => {
                        item_reports.push(format!(
                            "[{}/{}] ❌ 角色卡查询失败：{}。",
                            item_num, total, error
                        ));
                        fail_count += 1;
                        continue;
                    }
                }
            }

            match crate::agent::child_writer::spawn_child_writer(
                Some(app.clone()),
                state.inner(),
                parent_run_id,
                final_session_id,
                workspace_dir,
                &writer_model,
                &reasoning_effort,
                &effective_params,
                writer_persona,
                interrupt_rx.clone(),
            )
            .await
            {
                Ok(child_run_id) => {
                    let task_str = effective_params.get("task").and_then(|v| v.as_str()).unwrap_or("");
                    let task_brief = if task_str.chars().count() > 40 {
                        format!("{}...", task_str.chars().take(40).collect::<String>())
                    } else {
                        task_str.to_string()
                    };
                    item_reports.push(format!(
                        "[{}/{}] ✅ 已派发 child_run_id={} 写入任务: {}",
                        item_num, total, child_run_id, task_brief
                    ));
                    spawned_ids.push(child_run_id);
                    success_count += 1;
                }
                Err(error) => {
                    item_reports.push(format!(
                        "[{}/{}] ❌ 派发失败：{}",
                        item_num, total, error
                    ));
                    fail_count += 1;
                }
            }
        }

        if !is_batch {
            if success_count == 1 {
                let child_run_id = &spawned_ids[0];
                let writer_params = &items[0];
                let mut stdout = format!(
                    "写入 worker 已在隔离副本上开工，child_run_id: {}。它完成修改并自行运行你指定的测试命令后，会以结构化回执（changed_files 含逐文件 diff、test_status、findings）回到你的上下文；用 wait_jobs(job_ids:[\"{}\"]) 回收。注意：你（root）才是真实工作区的唯一写入者——审阅其 diff 后由你用常规写工具落实修改并最终验证。",
                    child_run_id, child_run_id
                );
                let task_str = writer_params.get("task").and_then(|v| v.as_str()).unwrap_or("");
                let has_excerpts = writer_params.get("context_excerpts").is_some();
                if crate::agent::types::is_task_thin(task_str) && !has_excerpts {
                    stdout = format!("⚠️ 本次派发任务描述较简略，子 Agent 可能缺乏必要背景，建议在后续派发时携带 context_excerpts（包含 background、file_hints 或 todo_slice）。\n\n{}", stdout);
                }
                if inst.is_mixed_dispatch_format() {
                    stdout = format!("⚠️ 检测到混合格式（同时包含 dispatches 与单发字段），系统已优先采用 dispatches。\n\n{}", stdout);
                }
                DispatchResult {
                    stdout,
                    stderr: String::new(),
                    success: true,
                    route: "child_writer:spawned".to_string(),
                }
            } else {
                let err_msg = item_reports.into_iter().next().unwrap_or_else(|| "派发失败".to_string());
                DispatchResult {
                    stdout: String::new(),
                    stderr: err_msg,
                    success: false,
                    route: "child_writer:rejected_or_failed".to_string(),
                }
            }
        } else {
            let mut summary = format!(
                "已批量处理 {} 个写入 worker 派发请求（{} 成功，{} 失败）：\n{}",
                total, success_count, fail_count, item_reports.join("\n")
            );
            if !spawned_ids.is_empty() {
                let ids_formatted: Vec<String> = spawned_ids.iter().map(|id| format!("\"{}\"", id)).collect();
                summary.push_str(&format!(
                    "\n\n所有已派发的写入 worker 均在各自的私有副本上独立执行；完成后请调用 wait_jobs(job_ids:[{}]) 回收 diff 审查。",
                    ids_formatted.join(", ")
                ));
            }
            if inst.is_mixed_dispatch_format() {
                summary = format!("⚠️ 检测到混合格式（同时包含 dispatches 与单发字段），系统已优先采用 dispatches 批量派发。\n\n{}", summary);
            }
            let breaker_alert = context.record_batch_dispatch_outcome(success_count > 0);
            if success_count > 0 {
                DispatchResult {
                    stdout: summary,
                    stderr: String::new(),
                    success: true,
                    route: format!("child_writer:batch:{}ok{}fail", success_count, fail_count),
                }
            } else {
                let mut err = format!("批量写入 worker 派发全部失败（共 {} 项）：\n{}", total, item_reports.join("\n"));
                if let Some(alert) = breaker_alert {
                    err = format!("{}\n\n{}", err, alert);
                }
                DispatchResult {
                    stdout: String::new(),
                    stderr: err,
                    success: false,
                    route: "child_writer:rejected_or_failed".to_string(),
                }
            }
        }
    // --- B2.1 L2 只读侦察 worker：异步派发独立动作-观测循环，结果经 durable inbox 回收 ---
    } else if inst.get_tool().eq_ignore_ascii_case("child_worker")
        || inst.get_action().eq_ignore_ascii_case("child_worker")
    {
        let routing = crate::agent::router::AgentRouter::route_for_role(
            context,
            model_routing,
            step_id,
            crate::agent::router::roles::SCOUT,
        );
        let reasoning_effort = model_routing.reasoning_effort_for(&routing.tier).to_string();
        let items = inst.get_dispatch_items();
        let total = items.len();
        let is_batch = total > 1;

        let mut item_reports = Vec::new();
        let mut spawned_ids = Vec::new();
        let mut success_count = 0;
        let mut fail_count = 0;

        for (idx, worker_params) in items.iter().enumerate() {
            let item_num = idx + 1;
            let mut scout_persona = String::new();
            let mut scout_model = routing.model_id.clone();
            let mut effective_worker_params = worker_params.clone();
            if let Some(obj) = effective_worker_params.as_object_mut() {
                obj.remove("max_concurrency");
            }

            if let Some(role_name) = worker_params.get("role").and_then(|v| v.as_str()) {
                match crate::agent::tools::role_manager::resolve_enabled_card(state.inner(), role_name) {
                    Ok(Some(card)) => {
                        if !card.allows_kind("worker") {
                            item_reports.push(format!(
                                "[{}/{}] ❌ 角色卡「{}」未授权挂载为 worker（允许类型: {}）。",
                                item_num, total, role_name, card.allowed_kinds
                            ));
                            fail_count += 1;
                            continue;
                        }
                        scout_persona = crate::agent::tools::role_manager::persona_block(&card);
                        if !card.model_id.is_empty() {
                            scout_model = card.model_id.clone();
                        }
                        if !card.hard_scope.is_empty() {
                            effective_worker_params["hard_scope"] = json!(card.hard_scope);
                        }
                        if card.max_concurrency > 0 {
                            effective_worker_params["max_concurrency"] = json!(card.max_concurrency.min(crate::agent::child_reviewer::MAX_TOTAL_CHILDREN_PER_RUN as i64));
                        }
                    }
                    Ok(None) => {
                        item_reports.push(format!(
                            "[{}/{}] ❌ 角色卡不存在或已禁用：{}。",
                            item_num, total, role_name
                        ));
                        fail_count += 1;
                        continue;
                    }
                    Err(error) => {
                        item_reports.push(format!(
                            "[{}/{}] ❌ 角色卡查询失败：{}。",
                            item_num, total, error
                        ));
                        fail_count += 1;
                        continue;
                    }
                }
            }

            match crate::agent::child_worker::spawn_child_worker(
                Some(app.clone()),
                state.inner(),
                parent_run_id,
                final_session_id,
                workspace_dir,
                &scout_model,
                &reasoning_effort,
                &effective_worker_params,
                scout_persona,
                interrupt_rx.clone(),
            )
            .await
            {
                Ok(child_run_id) => {
                    let task_str = effective_worker_params.get("task").and_then(|v| v.as_str()).unwrap_or("");
                    let task_brief = if task_str.chars().count() > 40 {
                        format!("{}...", task_str.chars().take(40).collect::<String>())
                    } else {
                        task_str.to_string()
                    };
                    item_reports.push(format!(
                        "[{}/{}] ✅ 已派发 child_run_id={} 任务: {}",
                        item_num, total, child_run_id, task_brief
                    ));
                    spawned_ids.push(child_run_id);
                    success_count += 1;
                }
                Err(error) => {
                    item_reports.push(format!(
                        "[{}/{}] ❌ 派发失败：{}",
                        item_num, total, error
                    ));
                    fail_count += 1;
                }
            }
        }

        if !is_batch {
            if success_count == 1 {
                let child_run_id = &spawned_ids[0];
                let worker_params = &items[0];
                let mut stdout = format!(
                    "侦察 worker 已异步派发，child_run_id: {}。worker 拥有独立上下文并自行执行只读侦察（读文件/列目录/只读命令），完成后以结构化结论（findings/confidence/evidence_refs/missing_evidence）回到你的上下文；需要结论时调用 wait_jobs(job_ids:[\"{}\"]) 挂起等待，或先推进其他独立操作。finish 前须确认其已结束。",
                    child_run_id, child_run_id
                );
                let task_str = worker_params.get("task").and_then(|v| v.as_str()).unwrap_or("");
                let has_excerpts = worker_params.get("context_excerpts").is_some();
                if crate::agent::types::is_task_thin(task_str) && !has_excerpts {
                    stdout = format!("⚠️ 本次派发任务描述较简略，子 Agent 可能缺乏必要背景，建议在后续派发时携带 context_excerpts（包含 background、file_hints 或 todo_slice）。\n\n{}", stdout);
                }
                if inst.is_mixed_dispatch_format() {
                    stdout = format!("⚠️ 检测到混合格式（同时包含 dispatches 与单发字段），系统已优先采用 dispatches。\n\n{}", stdout);
                }
                DispatchResult {
                    stdout,
                    stderr: String::new(),
                    success: true,
                    route: "child_worker:spawned".to_string(),
                }
            } else {
                let err_msg = item_reports.into_iter().next().unwrap_or_else(|| "派发失败".to_string());
                DispatchResult {
                    stdout: String::new(),
                    stderr: err_msg,
                    success: false,
                    route: "child_worker:rejected_or_failed".to_string(),
                }
            }
        } else {
            let mut summary = format!(
                "已批量处理 {} 个 worker 派发请求（{} 成功，{} 失败）：\n{}",
                total, success_count, fail_count, item_reports.join("\n")
            );
            if !spawned_ids.is_empty() {
                let ids_formatted: Vec<String> = spawned_ids.iter().map(|id| format!("\"{}\"", id)).collect();
                summary.push_str(&format!(
                    "\n\n已成功派发的 worker 均在后台异步执行；需要其结论时调用 wait_jobs(job_ids:[{}]) 挂起等待，或先推进其他独立操作。",
                    ids_formatted.join(", ")
                ));
            }
            if inst.is_mixed_dispatch_format() {
                summary = format!("⚠️ 检测到混合格式（同时包含 dispatches 与单发字段），系统已优先采用 dispatches 批量派发。\n\n{}", summary);
            }
            let breaker_alert = context.record_batch_dispatch_outcome(success_count > 0);
            if success_count > 0 {
                DispatchResult {
                    stdout: summary,
                    stderr: String::new(),
                    success: true,
                    route: format!("child_worker:batch:{}ok{}fail", success_count, fail_count),
                }
            } else {
                let mut err = format!("批量派发全部失败（共 {} 项）：\n{}", total, item_reports.join("\n"));
                if let Some(alert) = breaker_alert {
                    err = format!("{}\n\n{}", err, alert);
                }
                DispatchResult {
                    stdout: String::new(),
                    stderr: err,
                    success: false,
                    route: "child_worker:rejected_or_failed".to_string(),
                }
            }
        }
    // --- B2.5 特殊工具：wait_jobs 反应式零 Token 挂起等待 ---
    } else if inst.get_tool().eq_ignore_ascii_case("wait_jobs")
        || inst.get_tool().eq_ignore_ascii_case("wait_job")
        || inst.get_action().eq_ignore_ascii_case("wait_jobs")
    {
        let params = inst.get_params();
        let target_ids: Option<Vec<String>> = params
            .get("job_ids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            });
        let timeout_secs = params
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(45);

        let _ = app.emit(
            &format!("agent-log-{}", final_session_id),
            format!("⏳ 正在挂起等待后台异步任务完成 (超时限制: {}s)...", timeout_secs),
        );
        let _ = app.emit(
            &format!("agent-progress-{}", final_session_id),
            json!({
                "type": "agent_waiting_jobs",
                "message": "正在挂起等待后台任务完成...",
                "job_ids": target_ids.clone().unwrap_or_default(),
            }),
        );

        let mut rx_watch = interrupt_rx.clone();
        match crate::agent::job_engine::wait_for_jobs(
            state.inner(),
            parent_run_id,
            target_ids.as_deref(),
            timeout_secs,
            Some(&mut rx_watch),
        )
        .await
        {
            Ok(completed_jobs) => {
                let mut stage_notes = String::new();
                for job in completed_jobs.iter() {
                    // 阶段审查任务走独立语义：不置 plan consumed、基础设施失败退还阶段次数
                    if Some(job.job_id.as_str()) == checkpoint.stage_review_run_id.as_deref()
                        || Some(job.job_id.as_str())
                            == checkpoint.stage_review_rebuttal_run_id.as_deref()
                    {
                        let value = serde_json::from_str::<serde_json::Value>(&job.result_json)
                            .unwrap_or(serde_json::json!({}));
                        let error_kind = value.get("error_kind").and_then(|e| e.as_str()).map(|s| s.to_string());
                        if matches!(
                            error_kind.as_deref(),
                            Some("timed_out") | Some("interrupted") | Some("cancelled")
                        ) {
                            checkpoint.stage_review_run_id = None;
                            checkpoint.stage_review_rebuttal_run_id = None;
                            if checkpoint.reviewer_budget_refunds < 3 && checkpoint.stage_review_budget_used > 0 {
                                checkpoint.stage_review_budget_used -= 1;
                                checkpoint.reviewer_budget_refunds += 1;
                            }
                            stage_notes.push_str("\n⚠️ 阶段审查未正常完成（次数已退还），该阶段视为未审。");
                            continue;
                        }
                        let withdrawn: Vec<String> = value
                            .get("withdrawn_findings")
                            .and_then(|w| w.as_array())
                            .map(|arr| arr.iter().filter_map(|i| i.as_str().map(|s| s.to_string())).collect())
                            .unwrap_or_default();
                        let is_blocked = |f: &serde_json::Value| {
                            f.get("severity").and_then(|s| s.as_str()).map(|s| s.eq_ignore_ascii_case("blocking")).unwrap_or(false)
                                && f.get("finality").and_then(|s| s.as_str()).map(|s| s != "final_only").unwrap_or(true)
                                && !f.get("id").and_then(|i| i.as_str()).map(|id| withdrawn.iter().any(|w| w == id)).unwrap_or(false)
                        };
                        let findings_arr = value.get("findings").and_then(|f| f.as_array()).cloned().unwrap_or_default();
                        checkpoint.stage_review_run_id = None;
                        checkpoint.stage_review_rebuttal_run_id = None;
                        // 基线已在派发时记录；此处不得覆盖（防覆盖记账竞态，同 stage_review::apply_stage_result）
                        checkpoint.stage_review_blockers_open = findings_arr.iter().filter(|f| is_blocked(f)).count() as u32;
                        checkpoint.stage_review_final_only = findings_arr.iter()
                            .filter(|f| {
                                f.get("severity").and_then(|s| s.as_str()).map(|s| s.eq_ignore_ascii_case("blocking")).unwrap_or(false)
                                    && f.get("finality").and_then(|s| s.as_str()).map(|s| s == "final_only").unwrap_or(false)
                                    && !f.get("id").and_then(|i| i.as_str()).map(|id| withdrawn.iter().any(|w| w == id)).unwrap_or(false)
                            })
                            .map(|f| format!("{}: {}", f.get("id").and_then(|i| i.as_str()).unwrap_or("?"), f.get("why").and_then(|w| w.as_str()).unwrap_or("")))
                            .collect();
                        stage_notes.push_str(&format!(
                            "\n📋 阶段审查结论已回收：blocking_regardless 未解决 {} 条 / final_only 记账 {} 条。{}",
                            checkpoint.stage_review_blockers_open,
                            checkpoint.stage_review_final_only.len(),
                            if checkpoint.stage_review_blockers_open > 0 { "后续写入已被拦截，请用 resume_child 对质解套。" } else { "" }
                        ));
                        continue;
                    }
                    if !(job.job_type == "child_reviewer" || job.job_type.contains("reviewer")) {
                        continue;
                    }
                    // 基础设施失败（超时/取消/中断）不是实质性审查：退款且不算消费过
                    let infra_kind = serde_json::from_str::<serde_json::Value>(&job.result_json)
                        .ok()
                        .and_then(|v| v.get("error_kind").and_then(|e| e.as_str()).map(|s| s.to_string()));
                    let is_infra = matches!(
                        infra_kind.as_deref(),
                        Some("timed_out") | Some("interrupted") | Some("cancelled")
                    );
                    if is_infra {
                        checkpoint.refund_infra_failed_review_round(infra_kind.as_deref());
                        continue;
                    }
                    checkpoint.plan_reviewer_consumed = true;
                    // wait_jobs 交付路径同样要回填评分卡与 blocking 计数，
                    // 否则门禁在“走 wait_jobs 收取”的正常链路上会因缺评分卡而误拦（fail-closed 的对称补齐）。
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&job.result_json) {
                        if let Some(sc) = value.get("scorecard").and_then(|v| serde_json::from_value::<crate::agent::child_reviewer::ReviewScorecard>(v.clone()).ok()) {
                            checkpoint.last_review_scorecard = Some(sc);
                        }
                        // 异议闭环：撤回的 blocking 异议不计入拦截判据
                        let withdrawn: Vec<String> = value
                            .get("withdrawn_findings")
                            .and_then(|w| w.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|i| i.as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default();
                        checkpoint.last_review_blocking_count = value
                            .get("findings")
                            .and_then(|f| f.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter(|f| {
                                        let is_blocking = f
                                            .get("severity")
                                            .and_then(|s| s.as_str())
                                            .map(|s| s.eq_ignore_ascii_case("blocking"))
                                            .unwrap_or(false);
                                        let id = f.get("id").and_then(|i| i.as_str()).unwrap_or("");
                                        is_blocking && !withdrawn.iter().any(|w| w == id)
                                    })
                                    .count() as u32
                            })
                            .unwrap_or(0);
                        // 异议动态三计数（膨胀观测）
                        let findings_arr = value.get("findings").and_then(|f| f.as_array()).cloned().unwrap_or_default();
                        let maintained = value
                            .get("objection_verdicts")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter(|v| {
                                        v.get("verdict").and_then(|s| s.as_str()).map(|s| s.eq_ignore_ascii_case("maintain")).unwrap_or(false)
                                            && findings_arr.iter().any(|f| {
                                                f.get("id").and_then(|i| i.as_str()) == v.get("finding_id").and_then(|i| i.as_str())
                                                    && f.get("severity").and_then(|s| s.as_str()).map(|s| s.eq_ignore_ascii_case("blocking")).unwrap_or(false)
                                            })
                                    })
                                    .count() as u32
                            })
                            .unwrap_or(0);
                        checkpoint.review_objection_withdrawn = withdrawn.len() as u32;
                        checkpoint.review_objection_maintained = maintained;
                        checkpoint.review_total_withdrawn += withdrawn.len() as u32;
                        checkpoint.review_total_maintained += maintained;
                        checkpoint.review_objection_added = checkpoint
                            .last_review_blocking_count
                            .saturating_sub(checkpoint.review_objection_withdrawn);
                    }
                }
                let mut out = String::new();
                if completed_jobs.is_empty() {
                    out.push_str("等待结束：当前无新完成的后台任务，或所有后台任务已结束。");
                } else {
                    out.push_str(&stage_notes);
                    out.push_str(&format!("✅ 已成功等待并接收到 {} 个后台任务结果：\n", completed_jobs.len()));
                    for job in completed_jobs {
                        out.push_str(&format!(
                            "\n- 【任务 ID: {} | 类型: {}】状态: {}, 耗时: {}ms\n  描述: {}\n  输出: {}\n",
                            job.job_id,
                            job.job_type,
                            job.status,
                            job.duration_ms,
                            job.title,
                            if job.result_json.is_empty() { "（无附加输出）" } else { &job.result_json }
                        ));
                    }
                }
                DispatchResult {
                    stdout: out,
                    stderr: String::new(),
                    success: true,
                    route: "wait_jobs:resolved".to_string(),
                }
            }
            Err(err) => DispatchResult {
                stdout: String::new(),
                stderr: err,
                success: false,
                route: "wait_jobs:interrupted_or_failed".to_string(),
            },
        }
    // --- B2.6 特殊工具：resume_child 追问已完成的子任务 ---
    } else if inst.get_tool().eq_ignore_ascii_case("resume_child")
        || inst.get_action().eq_ignore_ascii_case("resume_child")
    {
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
            DispatchResult {
                stdout: String::new(),
                stderr: "❌ resume_child 缺少必要参数 child_run_id。".to_string(),
                success: false,
                route: "host_rejection:missing_child_run_id".to_string(),
            }
        } else if followup_msg.is_empty() {
            DispatchResult {
                stdout: String::new(),
                stderr: "❌ resume_child 缺少必要参数 message（对该子任务的追问或补充要求）。".to_string(),
                success: false,
                route: "host_rejection:missing_followup_message".to_string(),
            }
        } else {
            let child_data: Result<(String, String, Option<String>, Option<String>, String, String, Option<String>), String> = {
                let conn = state.0.lock().map_err(|e| e.to_string())?;
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
                Err(err) => DispatchResult {
                    stdout: String::new(),
                    stderr: err,
                    success: false,
                    route: "host_rejection:child_not_found".to_string(),
                },
                Ok((status, kind, ws_copy, hist_json, child_model, root_str, prev_result_json)) => {
                    if status != "succeeded" {
                        DispatchResult {
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
                            DispatchResult {
                                stdout: String::new(),
                                stderr: "❌ 该审查任务未留存历史对话记录，无法恢复对质。".to_string(),
                                success: false,
                                route: "host_rejection:missing_history".to_string(),
                            }
                        } else {
                            messages.push(json!({
                                "role": "user",
                                "content": format!(
                                    "【父 Agent 反驳与对质】\n{}\n\n【对质研判规则】：\n请针对上述反驳与证据进行重新研判，输出更新后的审查 JSON：\n1. 若父 Agent 反驳与测试证据成立，请将该项缺陷撤回（从 findings 中剔除，并在 missing_evidence 说明已撤回原 finding ID）；\n2. 若仍坚持该缺陷，必须在 why 中给出全新证据与理由；\n3. 重新推导 routing（certain/needs_alignment/blocked）。",
                                    followup_msg
                                )
                            }));

                            let routing = crate::agent::router::AgentRouter::route_for_role(
                                context,
                                model_routing,
                                step_id,
                                crate::agent::router::roles::REVIEWER,
                            );
                            let model_to_use = if !child_model.is_empty() {
                                child_model
                            } else {
                                let (preferred, _note) = crate::agent::router::prefer_cross_family_reviewer(
                                    &*state.inner().0.lock().unwrap(),
                                    &routing.model_id,
                                );
                                preferred
                            };
                            let reasoning_effort = model_routing.reasoning_effort_for(&routing.tier).to_string();

                            let new_child_run_id = match crate::agent::child_reviewer::begin_child_run_extended(
                                state.inner(),
                                parent_run_id,
                                final_session_id,
                                workspace_dir,
                                &model_to_use,
                                0,
                                "review",
                                None,
                                None,
                                Some(target_child_id),
                            ) {
                                Ok(id) => {
                                    // 对质目标是阶段审查任务时，登记复检 run id 以便结果回流阶段语义
                                    if Some(target_child_id) == checkpoint.stage_review_run_id.as_deref() {
                                        checkpoint.stage_review_rebuttal_run_id = Some(id.clone());
                                    }
                                    id
                                }
                                Err(err) => {
                                    return Ok(DispatchResult {
                                        stdout: String::new(),
                                        stderr: format!("❌ 创建审查对质任务失败: {}", err),
                                        success: false,
                                        route: "host_rejection:begin_run_failed".to_string(),
                                    });
                                }
                            };

                            let mut config = match ModelConfig::from_db_state(state.inner(), &model_to_use) {
                                Ok(cfg) => cfg,
                                Err(err) => {
                                    return Ok(DispatchResult {
                                        stdout: String::new(),
                                        stderr: format!("❌ 加载模型配置失败: {}", err),
                                        success: false,
                                        route: "host_rejection:model_config_failed".to_string(),
                                    });
                                }
                            };
                            config.max_tokens = 900;
                            if !checkpoint.plan_phase.is_approved() {
                                // 反驳对质同样消耗审查预算：必须走统一预扣，堵住无上限自增旁路
                                if let Err(reason) = checkpoint.reserve_plan_review_round() {
                                    return Ok(DispatchResult {
                                        stdout: String::new(),
                                        stderr: format!("【审查轮数已达上限】{}\n{}", reason, crate::agent::gate_messages::reviewer_retry_limit()),
                                        success: false,
                                        route: "plan_gatekeeper:reviewer_retry_limit".to_string(),
                                    });
                                }
                            }

                            let db_clone = DbState(state.0.clone());
                            let new_child_run_id_clone = new_child_run_id.clone();
                            let _parent_session_id_clone = final_session_id.to_string();
                            let mut interrupt_rx_clone = interrupt_rx.clone();
                            let _app_clone = app.clone();

                            tokio::spawn(async move {
                                let started_at = std::time::Instant::now();
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
                                    std::time::Duration::from_secs(45),
                                    call,
                                ).await {
                                    Ok(Ok(res)) => res,
                                    Ok(Err(_e)) => {
                                        crate::agent::child_reviewer::finish_child_run(
                                            &db_clone,
                                            &new_child_run_id_clone,
                                            "failed",
                                            started_at.elapsed().as_millis() as u64,
                                            0,
                                            Some("request_or_stream"),
                                            None,
                                        );
                                        return;
                                    }
                                    Err(_) => {
                                        crate::agent::child_reviewer::finish_child_run(
                                            &db_clone,
                                            &new_child_run_id_clone,
                                            "timed_out",
                                            started_at.elapsed().as_millis() as u64,
                                            0,
                                            Some("timeout"),
                                            None,
                                        );
                                        return;
                                    }
                                };

                                let duration_ms = started_at.elapsed().as_millis() as u64;
                                let completion_tokens = stream_result.usage.map(|u| u.completion_tokens).unwrap_or(0);
                                let (findings, confidence, evidence_refs, missing_evidence, routing) = 
                                    match crate::agent::child_reviewer::parse_child_review_rebuttal_output(&stream_result.full_response) {
                                        Ok(res) => res,
                                        Err(_) => {
                                            crate::agent::child_reviewer::finish_child_run(
                                                &db_clone,
                                                &new_child_run_id_clone,
                                                "invalid_output",
                                                duration_ms,
                                                completion_tokens,
                                                Some("invalid_output"),
                                                None,
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
                                    withdrawn_findings: vec![],
                                    objection_verdicts: vec![],
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

                            DispatchResult {
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
                            DispatchResult {
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
                                DispatchResult {
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
                                    context,
                                    model_routing,
                                    step_id,
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
                                let reasoning_effort = model_routing.reasoning_effort_for(&routing.tier).to_string();

                                let new_child_run_id = match crate::agent::child_reviewer::begin_child_run_extended(
                                    state.inner(),
                                    parent_run_id,
                                    final_session_id,
                                    workspace_dir,
                                    &model_to_use,
                                    0,
                                    &kind,
                                    None,
                                    Some(ws_path.to_string_lossy().as_ref()),
                                    Some(target_child_id),
                                ) {
                                    Ok(id) => id,
                                    Err(err) => {
                                        return Ok(DispatchResult {
                                            stdout: String::new(),
                                            stderr: format!("❌ 创建追问子任务失败: {}", err),
                                            success: false,
                                            route: "host_rejection:begin_run_failed".to_string(),
                                        });
                                    }
                                };

                                let config = match ModelConfig::from_db_state(state.inner(), &model_to_use) {
                                    Ok(cfg) => cfg,
                                    Err(err) => {
                                        return Ok(DispatchResult {
                                            stdout: String::new(),
                                            stderr: format!("❌ 加载模型配置失败: {}", err),
                                            success: false,
                                            route: "host_rejection:model_config_failed".to_string(),
                                        });
                                    }
                                };

                                if kind == "writer" {
                                    // 🛑 前置方案门禁：恢复(resume_child)写入型子任务等同于开启新的写入路径，
                                    // 未获批准前必须拦截，防止绕过派发门禁（B1.8）通过旧 writer 子任务改代码。
                                    if let Some(rejection) = check_pre_write_gatekeeper(
                                        checkpoint,
                                        context,
                                        state.inner(),
                                        parent_run_id,
                                        step_id,
                                        true,
                                    ) {
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
                                        );
                                        return Ok(rejection);
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
                                        DbState(state.0.clone()),
                                        Some(app.clone()),
                                        final_session_id.to_string(),
                                        new_child_run_id.clone(),
                                        parent_root,
                                        ws_path,
                                        config,
                                        reasoning_effort,
                                        writer_req,
                                        String::new(),
                                        interrupt_rx.clone(),
                                        Some(messages),
                                        Some(max_steps),
                                    ));

                                    DispatchResult {
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
                                        DbState(state.0.clone()),
                                        Some(app.clone()),
                                        final_session_id.to_string(),
                                        new_child_run_id.clone(),
                                        ws_path,
                                        config,
                                        reasoning_effort,
                                        worker_req,
                                        String::new(),
                                        interrupt_rx.clone(),
                                        Some(messages),
                                        Some(max_steps),
                                    ));

                                    DispatchResult {
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
    } else if inst.get_tool().eq_ignore_ascii_case("ask_human") || inst.get_action().eq_ignore_ascii_case("ask") {
        // 【先监听，后求助】刚被门禁拦截就立刻 ask_human = 把问题甩给人类。
        // 宿主冷却一次：要求先按拦截意见行动（重新取证/修复/重派审查/wait_jobs）。
        if let Some(cooldown_msg) = evaluate_ask_human_cooldown(checkpoint, step_id) {
            crate::agent::dispatcher::emit_tool_event(
                app,
                final_session_id,
                step_id,
                inst,
                "failed",
                Some("ask_human_cooldown".to_string()),
                Some(false),
                &cooldown_msg,
                Some(tool_started_at.elapsed().as_millis()),
            );
            return Ok(DispatchResult {
                stdout: String::new(),
                stderr: cooldown_msg,
                success: false,
                route: "host_rejection:ask_human_cooldown".to_string(),
            });
        }
        let params = inst.get_params();
        let form_val = params.get("form");
        // 与前端 normalizeHumanForm 的别名表保持一致（plan_approval / plan / approval），
        // 防止前端渲染出审批卡片而后端不认，导致用户批准后门禁仍然关闭。
        let is_plan_approval_kind = form_val
            .and_then(|f| f.get("kind"))
            .and_then(|k| k.as_str())
            .map(|k| {
                let normalized = k.trim().to_ascii_lowercase();
                normalized == "plan_approval" || normalized == "plan" || normalized == "approval"
            })
            .unwrap_or(false);

        // 人工仲裁与放行上下文识别：
        // 1. 显式带有 plan_approval/plan/approval kind 表单；
        // 2. 当前方案前置门禁尚未获批（如 Exploring / Reviewing / AwaitingApproval）；
        // 3. 存在未通过的审查评分卡或未闭合的 blocking 异议；
        // 4. 阶段审查存在未解决的 stage_review_blockers_open。
        // 在上述任一情况下，本次 ask_human 均承担向人类请示写入权限或请求人工仲裁放行的语义。
        let is_approval_context = is_plan_approval_kind
            || !checkpoint.plan_phase.is_approved()
            || checkpoint.stage_review_blockers_open > 0
            || checkpoint.last_review_scorecard.as_ref().map_or(false, |s| !s.passes_gate())
            || checkpoint.last_review_blocking_count > 0;

        let ask_msg = params
            .get("message")
            .or_else(|| form_val.and_then(|f| f.get("title")))
            .or_else(|| form_val.and_then(|f| f.get("description")))
            .and_then(|v| v.as_str())
            .unwrap_or("AI 请求人类协助与指示")
            .to_string();

        app.emit(
            &format!("agent-log-{}", final_session_id),
            format!("⏸️ AI 已挂起，等待人类协助: {}", ask_msg),
        )
        .ok();

        // 若处于仲裁/审批上下文但模型未提供 form，自动补齐 plan_approval 结构化表单，
        // 使得前端直接渲染【批准方案】【打回修改】等可点击卡片，提升人机协作效率。
        let final_form_val: Option<serde_json::Value> = if form_val.is_some() {
            form_val.cloned()
        } else if is_approval_context {
            Some(json!({
                "kind": "plan_approval",
                "title": "方案与安全审查人工仲裁",
                "description": ask_msg
            }))
        } else {
            None
        };

        // 通知前端进入暂停状态
        app.emit(
            &format!("agent-progress-{}", final_session_id),
            json!({
                "type": "agent_paused",
                "message": ask_msg,
                "form": final_form_val.unwrap_or(serde_json::Value::Null)
            }),
        )
        .ok();

        println!("⏸️ [阻塞点] 模型触发 ask_human 挂起，正在死等前端人类唤醒...");

        // 方案审批与仲裁挂起时，将阶段设为 AwaitingApproval，供前端及 Checkpoint 呈现等待审批状态
        if is_approval_context {
            checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::AwaitingApproval;
        }

        // 真正的阻塞点：死等人类回复！
        // rx 被我们在外层定义过了，这里可以直接 `.recv().await`
        checkpoint.prepared_instruction = Some(inst.clone());
        checkpoint.wait_for_human(ask_msg.clone(), &context);
        save_checkpoint(state.inner(), &mut checkpoint)?;
        crate::agent::advance_run_state(
            &app,
            &final_session_id,
            &mut run_machine,
            AgentRunEvent::WaitForHuman {
                question: ask_msg.clone(),
            },
        )?;
        let reply = match rx.recv().await {
            Some(HumanControl::Answer(reply)) => reply,
            Some(HumanControl::ReplaceGoal(goal)) => {
                return Ok(DispatchResult {
                    success: true,
                    stdout: format!("用户提交了新的任务目标：{}", goal),
                    stderr: String::new(),
                    route: RUN_INTERRUPTED_ROUTE.to_string(),
                })
            }
            Some(HumanControl::Cancel) => {
                return Ok(DispatchResult {
                    success: true,
                    stdout: "用户请求停止当前任务。".to_string(),
                    stderr: String::new(),
                    route: RUN_INTERRUPTED_ROUTE.to_string(),
                })
            }
            None => "人类未回复，通道异常关闭".to_string(),
        };
        context.add_error_feedback(&format!("【人类回复】{}", reply));
        if is_approval_context {
            match parse_plan_approval_decision(&reply) {
                Some(false) => {
                    // 人类提出修改意见，打回重新推演审查
                    checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Reviewing;
                }
                Some(true) => {
                    // 人类在方案审批卡片中批准放行
                    checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
                    if checkpoint.stage_review_blockers_open > 0 {
                        checkpoint.stage_review_blockers_open = 0;
                    }
                    // 摘要中剥离结构化标记，避免协议噪声污染持久化内容
                    let clean_reply = strip_plan_decision_marker(&reply);
                    checkpoint.approved_plan_summary =
                        Some(clean_reply.chars().take(500).collect());
                    app.emit(
                        &format!("agent-log-{}", final_session_id),
                        "✅ 方案审批与人工仲裁已放行（收到用户批准，写入门禁解除）".to_string(),
                    )
                    .ok();
                }
                None => {
                    // “问一次就好，一次审批全程放权”
                    // 若用户已有非空回复且未包含打回否定词，直接放行写入，彻底避免连环死循环轰炸用户
                    let trimmed = reply.trim();
                    if !trimmed.is_empty()
                        && trimmed != "人类未回复，通道异常关闭"
                        && trimmed != "我再想想"
                    {
                        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
                        if checkpoint.stage_review_blockers_open > 0 {
                            checkpoint.stage_review_blockers_open = 0;
                        }
                        let clean_reply = strip_plan_decision_marker(&reply);
                        checkpoint.approved_plan_summary =
                            Some(clean_reply.chars().take(500).collect());
                        app.emit(
                            &format!("agent-log-{}", final_session_id),
                            "✅ 方案审批与人工仲裁已放行（收到用户非反对回复，门禁解除）".to_string(),
                        )
                        .ok();
                    } else {
                        let msg = "【方案审批结论无法判定】未从回复中识别出明确的批准或打回意图，\
                                   前置写入门禁保持关闭。请重新呈报方案请用户明确确认。";
                        context.add_error_feedback(msg);
                        app.emit(
                            &format!("agent-log-{}", final_session_id),
                            msg.to_string(),
                        )
                        .ok();
                    }
                }
            }
        }
        checkpoint.begin_planning(step_id, &context);
        save_checkpoint(state.inner(), &mut checkpoint)?;
        crate::agent::advance_run_state(
            &app,
            &final_session_id,
            &mut run_machine,
            AgentRunEvent::HumanReplyReceived,
        )?;

        println!("▶️ [唤醒] 人类回复: {}", reply);

        app.emit(
            &format!("agent-log-{}", final_session_id),
            format!("▶️ AI 已被唤醒，人类回复: {}", reply),
        )
        .ok();

        // 伪造一个成功的执行结果，把人类的话当作工具输出返回给大模型
        crate::agent::types::DispatchResult {
            success: true,
            stdout: format!("人类的回复是: {}", reply),
            stderr: String::new(),
            route: "ask_human_resolved".to_string(),
        }
    } else {
        let is_write_tool = is_write_or_side_effect_tool(inst);
        if is_write_tool {

            if let Some(scope_guard) = context
                .resource_scope
                .guard_known_outside_write(&inst.get_tool(), &inst.get_params())
            {
                let result = DispatchResult {
                    success: false,
                    stdout: String::new(),
                    stderr: format!(
                        "{}\nNo tool call was issued; original parameters were not changed.",
                        scope_guard.render()
                    ),
                    route: "scope_guard:outside".to_string(),
                };
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
                );
                return Ok(result);
            }
        }

        // 外部副作用前先持久化 pending；未知/写操作默认 HumanReview。
        let recovery_policy = checkpoint.prepare_tool(inst.clone(), &context);
        if recovery_policy.requires_pending() {
            save_checkpoint(state.inner(), &mut checkpoint)?;
        }

        // 具体和本地工具交互 (非 ask_human 走普通逻辑)
        let inst_c = inst.clone();
        let registry_arc = registry_state.inner().clone();
        let db = state.inner().clone();
        let sid = final_session_id.clone();
        let registry = {
            let guard = registry_arc.lock().await;
            (*guard).clone()
        };

        let mut tool_task = tokio::task::spawn_blocking(move || {
            println!("执行动作: {:?}", inst_c);
            run_agent_step(&db, &sid, &inst_c, &registry)
        });

        let mut dispatch_result = tokio::select! {
            res = &mut tool_task => {
                res.map_err(|e| e.to_string())?
            }
            Some(control) = rx.recv() => {
                let interrupt_reason = match control {
                    HumanControl::Cancel => "用户请求停止当前任务。".to_string(),
                    HumanControl::ReplaceGoal(goal) => format!("用户提交了新的任务目标：{}（已中断前序工具）", goal),
                    HumanControl::Answer(_) => "用户请求中断当前任务。".to_string(),
                };

                if is_write_tool {
                    // 写盘/事务类工具：给予最多 1.5 秒安全落盘缓冲期，避免破坏正在保存的文件
                    app.emit(
                        &format!("agent-log-{}", final_session_id),
                        "⏳ 收到停止指令，正在给予写盘/事务操作 1.5s 安全保存缓冲期...".to_string(),
                    ).ok();

                    tokio::select! {
                        res = &mut tool_task => {
                            app.emit(
                                &format!("agent-log-{}", final_session_id),
                                "✅ 写盘操作已安全完成，任务即将终止。".to_string(),
                            ).ok();
                            res.map_err(|e| e.to_string())?
                        }
                        _ = tokio::time::sleep(tokio::time::Duration::from_millis(1500)) => {
                            app.emit(
                                &format!("agent-log-{}", final_session_id),
                                "⚠️ 写盘操作耗时超 1.5s，已强制安全终止。".to_string(),
                            ).ok();
                            DispatchResult {
                                success: false,
                                stdout: interrupt_reason,
                                stderr: String::new(),
                                route: RUN_INTERRUPTED_ROUTE.to_string(),
                            }
                        }
                    }
                } else {
                    // 只读/网络/查询类工具：0 毫秒立刻中断退出
                    DispatchResult {
                        success: false,
                        stdout: interrupt_reason,
                        stderr: String::new(),
                        route: RUN_INTERRUPTED_ROUTE.to_string(),
                    }
                }
            }
        };

        if dispatch_result.route == RUN_INTERRUPTED_ROUTE {
            return Ok(dispatch_result);
        }
        let output = if dispatch_result.success {
            dispatch_result.stdout.clone()
        } else {
            dispatch_result.stderr.clone()
        };
        if let Some(scope_observation) = context.resource_scope.observe_tool_result(
            &inst.get_tool(),
            &inst.get_params(),
            &output,
            if is_write_tool {
                ResourceOperation::Write
            } else {
                ResourceOperation::Read
            },
        ) {
            let annotation = scope_observation.render();
            if dispatch_result.success {
                dispatch_result.stdout = format!("{}\n\n{}", dispatch_result.stdout, annotation);
            } else {
                dispatch_result.stderr = format!("{}\n\n{}", dispatch_result.stderr, annotation);
            }
        }
        // 无进展重复检测：只在"相同工具+相同参数+相同输出/失败"反复发生时注入
        // 策略级提示；不修改、不补全、不拒绝模型提交的参数。
        if let Some(no_progress_notice) = context.no_progress.record(
            &inst.get_tool(),
            &inst.get_params(),
            dispatch_result.success,
            &output,
        ) {
            context
                .turns_history
                .push(crate::agent::context::types::ChatMessage {
                    role: "user".to_string(),
                    content: serde_json::json!(no_progress_notice),
                });
            println!("🔁 [No-Progress] 注入无进展提示: {}", inst.get_tool());
        }
        // 检索零命中偏航：换措辞换关键词连续零命中不会触发上面的重复检测
        // （签名各不相同）；这里按（工具族×作用域）组计数，只提示一次，
        // 同样只注入事实提示，不修改、不拒绝、不强制 finish。
        if let Some(zero_hit_notice) = context.zero_hit.record(
            &inst.get_tool(),
            &inst.get_params(),
            dispatch_result.success,
            &output,
        ) {
            context
                .turns_history
                .push(crate::agent::context::types::ChatMessage {
                    role: "user".to_string(),
                    content: serde_json::json!(zero_hit_notice),
                });
            println!("🎯 [Zero-Hit] 注入检索零命中偏航提示: {}", inst.get_tool());
        }

        dispatch_result
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
    );

    Ok(dispatch_result)
}

