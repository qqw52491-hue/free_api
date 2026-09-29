//! 主循环入口：run_agent_main_loop 包装、工作区锁定与方案审查员故障转移。

use super::*;
use crate::agent::change_verification::{
    apply_change_verification_update, change_verification_blocker, ChangeVerification,
};
use crate::agent::checkpoint::{
    load_checkpoint, recovery_decision, save_checkpoint, AgentCheckpoint, AgentPhase,
    RecoveryDecision,
};
use crate::agent::context;
use crate::agent::control::{
    finish_interrupted_run, intervention_channels, stream_interrupt_channels,
    take_pending_interrupt, HumanControl,
};
use crate::agent::dispatcher::{is_finish_instruction, is_write_or_side_effect_tool};
use crate::agent::loop_runner::run_call_llm_and_run_tool_phases;
use crate::agent::mcp::{McpClient, PluginRegistry};
use crate::agent::review_contract::{apply_review_contract_update, ReviewContract};
use crate::agent::run_state::{AgentRunEvent, AgentRunMachine, AgentRunState};
use crate::agent::task_store;
use crate::agent::types::{AgentInstruction, DispatchResult, TodoItem, RUN_INTERRUPTED_ROUTE};
use crate::db::DbState;
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, watch};
use tokio::time::{sleep, Duration};
use crate::agent::finish_guard::{
    apply_instruction_state_updates, finish_blockers, finish_blockers_with_diff,
    review_contract_evidence_blockers, should_challenge_finish,
    should_challenge_finish_with_blockers, should_challenge_finish_with_unresolved, DiffContext,
};
use crate::agent::workspace_lock::{acquire_workspace_lock, ChildWorkspaceGuard};

/// 推进后端运行状态，并向前端发送独立于旧进度事件的权威状态快照。
/// 旧的 agent-progress-* 事件保持不变，以便前端逐步迁移。
pub fn advance_run_state(
    app: &AppHandle,
    session_id: &str,
    machine: &mut AgentRunMachine,
    event: AgentRunEvent,
) -> Result<(), String> {
    let snapshot = machine.transition(event).map_err(|err| err.to_string())?;
    app.emit(&format!("agent-run-state-{}", session_id), snapshot)
        .map_err(|err| err.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn run_agent_main_loop(
    app: AppHandle,
    state: State<'_, DbState>,
    registry_state: State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    model_routing: crate::agent::router::ModelRoutingConfig,
    goal: String,
    auto_pilot: bool,
    session_id: Option<String>,
    log_context: Option<bool>,
    transient_llm_request_retries: Option<u32>,
    attachments: Option<Vec<crate::commands::chat::ParsedAttachmentResult>>,
    plan_mode: Option<String>,
) -> Result<(), String> {
    run_agent_main_loop_in_workspace(
        app,
        state,
        registry_state,
        model_routing,
        goal,
        auto_pilot,
        session_id,
        log_context,
        transient_llm_request_retries,
        attachments,
        None,
        None,
        plan_mode,
        None,
    )
    .await
}

/// 仅供受宿主校验的后台评测使用；普通会话始终沿用进程当前工作区。
pub(crate) async fn run_agent_main_loop_in_workspace(
    app: AppHandle,
    state: State<'_, DbState>,
    registry_state: State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    model_routing: crate::agent::router::ModelRoutingConfig,
    goal: String,
    auto_pilot: bool,
    session_id: Option<String>,
    log_context: Option<bool>,
    transient_llm_request_retries: Option<u32>,
    attachments: Option<Vec<crate::commands::chat::ParsedAttachmentResult>>,
    workspace_override: Option<std::path::PathBuf>,
    run_id_override: Option<String>,
    plan_mode: Option<String>,
    run_budget: Option<AgentRunBudget>,
) -> Result<(), String> {
    let final_session_id = session_id.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis().to_string())
            .unwrap_or_else(|_| "default".to_string())
    });
    let run_id = run_id_override.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let retries = transient_llm_request_retries
        .unwrap_or(DEFAULT_TRANSIENT_LLM_REQUEST_RETRIES)
        .clamp(1, MAX_TRANSIENT_LLM_REQUEST_RETRIES);

    let run_guard = crate::agent::channel::SessionRunGuard::new(
        &app,
        &final_session_id,
        &run_id,
        &goal,
        retries,
    );

    let result = run_agent_main_loop_in_workspace_inner(
        app,
        state,
        registry_state,
        model_routing,
        goal,
        auto_pilot,
        final_session_id,
        log_context,
        retries,
        attachments,
        workspace_override,
        run_id,
        plan_mode,
        run_budget,
    )
    .await;

    run_guard.finish(&result);
    result
}

/// 审查者故障转移重派（invalid_output / timed_out 时走这里）。
/// 返回 Ok(Some(new_run_id)) = 已重派；Ok(None) = 不满足重派条件（封顶/已批准/无派发记录）。
/// 【冒烟实测抓到的坑】failover 原来只挂在 wait_jobs 结果路径上——超时结果若经
/// 通用后台信箱（poll_pending_inbox）路径消费，会绕过 failover 且无条件置 consumed，
/// 主 Agent 将永远等不到备用审查员的结论。所有消费路径必须共用本函数。
///
/// failed_run_id：被替换的失败任务 id。重派成功后本函数内部完成
/// 齐套屏障的预期 id 替换（replace_expected_reviewer_run_id）——该步骤收编在此
/// 而非留给调用方：曾因 wait_jobs 路径换 id 而自动注入路径漏换，旧 id 永远
/// 无法交付，齐套屏障死锁。收编后结构性杜绝调用方遗忘。
pub(crate) async fn try_plan_reviewer_failover(
    app: Option<&AppHandle>,
    state: &DbState,
    run_id: &str,
    session_id: &str,
    workspace_dir: &std::path::Path,
    checkpoint: &mut AgentCheckpoint,
    interrupt_rx: watch::Receiver<u64>,
    // 当前仅 invalid_output/timeout/request_or_stream 共用同一重派动作；保留参数供
    // 后续"按 kind 分化重派策略"设计（届时启用）
    _error_kind: &crate::agent::types::ChildErrorKind,
    failed_run_id: Option<&str>,
) -> Result<Option<String>, String> {
    if checkpoint.plan_reviewer_failover_count >= 2
        || checkpoint.plan_reviewer_last_dispatch.is_none()
        || checkpoint.plan_phase.is_approved()
    {
        return Ok(None);
    }
    let mut dispatch = match checkpoint.plan_reviewer_dispatch_snapshot() {
        Some(d) => d,
        None => return Ok(None),
    };
    let (failover_model, model_name) = {
        let conn = state.0.lock().map_err(|err| err.to_string())?;
        let m = crate::agent::router::reviewer_failover_model(&conn, &dispatch.model);
        let name = m.as_deref().and_then(|id| {
            conn.query_row("SELECT name FROM models WHERE id = ?1", rusqlite::params![id], |r| r.get::<_, String>(0)).ok()
        });
        (m, name)
    };
    if let Some(model) = failover_model {
        dispatch.model = model;
    }
    if let Some(obj) = dispatch.params.as_object_mut() {
        let label = if let Some(ref mn) = model_name {
            format!("🛡️ 故障转移备选审查官 ({})", mn)
        } else {
            "🛡️ 故障转移备选审查官".to_string()
        };
        obj.insert("role_label".to_string(), serde_json::json!(label));
    }
    // retry_of 传入失败 run：备胎继承同一 task_id，attempt 由落库侧自动递增——
    // 台账里"同一道题换了模型重答"一脉相承，成本可归因到同一逻辑任务。
    match dispatch
        .spawn(
            app.cloned(),
            state,
            run_id,
            session_id,
            workspace_dir,
            interrupt_rx,
            failed_run_id,
        )
        .await
    {
        Ok(new_run_id) => {
            checkpoint.plan_reviewer_failover_count += 1;
            checkpoint.set_plan_reviewer_dispatch_snapshot(&dispatch);
            // 齐套屏障自动接管：备胎行自带原 batch_id（snapshot 盖章继承），
            // 台账推导下天然属于本批待决成员——不再需要手动换名单
            let _ = failed_run_id;
            Ok(Some(new_run_id))
        }
        Err(spawn_err) => Err(spawn_err),
    }
}


