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
pub fn advance_run_state<R: tauri::Runtime>(
    app: &AppHandle<R>,
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
    task_start_mode: Option<crate::agent::conversation::TaskStartMode>,
) -> Result<(), String> {
    {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        model_routing.validate_desktop_start(&conn)?;
    }
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
        task_start_mode,
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
    task_start_mode: Option<crate::agent::conversation::TaskStartMode>,
) -> Result<(), String> {
    let final_session_id = session_id.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis().to_string())
            .unwrap_or_else(|_| "default".to_string())
    });
    let (goal, attachments) = crate::commands::chat::attachment_storage::adopt_draft_attachments(
        &final_session_id, goal, attachments,
    )?;
    let run_id = run_id_override.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let retries = transient_llm_request_retries
        .unwrap_or(DEFAULT_TRANSIENT_LLM_REQUEST_RETRIES)
        .clamp(1, MAX_TRANSIENT_LLM_REQUEST_RETRIES);
    crate::agent::reviewer_gatekeeper::LLM_REQUEST_ATTEMPTS
        .store(retries, std::sync::atomic::Ordering::Relaxed);

    let run_guard = crate::agent::channel::SessionRunGuard::new(
        &app,
        &final_session_id,
        &run_id,
        &goal,
        retries,
    );

    let cleanup_db = DbState(state.0.clone());
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
        task_start_mode,
    )
    .await;

    // 其他宿主错误经 ? 提前返回时也必须收停；已走失败出口的再次调用是幂等的。
    if result.is_err() {
        for warning in crate::agent::failure::stop_run_runtime(&cleanup_db, &run_guard.session_id, &run_guard.run_id).await {
            eprintln!("[failure-cleanup] {warning}");
        }
    }
    run_guard.finish(&result);
    result
}

/// 仅使用显式备用重派失败的同一道审查；按 child_run_id 取快照，避免联合会审串题。
pub(crate) async fn try_plan_reviewer_failover(
    app: Option<&AppHandle>, state: &DbState, run_id: &str, session_id: &str,
    workspace_dir: &std::path::Path, checkpoint: &mut AgentCheckpoint,
    interrupt_rx: watch::Receiver<u64>, backup_model: &str,
    _error_kind: &crate::agent::types::ChildErrorKind, failed_run_id: Option<&str>,
) -> Result<Option<String>, String> {
    if backup_model.trim().is_empty() { return Ok(None); }
    let Some(failed_id) = failed_run_id else { return Ok(None) };
    let mut dispatch = checkpoint.reviewer_dispatches.get(failed_id).cloned()
        .ok_or_else(|| format!("审查 {failed_id} 缺少原始派发快照，不能猜测或借用别人的审查题目"))?;
    let selected = {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        crate::agent::router::configured_reviewer_backup(&conn, &dispatch.model, backup_model)?
    };
    let Some((model, name)) = selected else { return Ok(None) };
    dispatch.model = model;
    if let Some(obj) = dispatch.params.as_object_mut() {
        obj.insert("role_label".into(), json!(format!("🛡️ 用户配置的备用审查官 ({name})")));
    }
    // 继承 task_id/attempt/batch，spawn 内认养已经存在的更晚 attempt，崩溃重投不重复派发。
    let new_id = dispatch.spawn(app.cloned(), state, run_id, session_id, workspace_dir,
        interrupt_rx, Some(failed_id)).await?;
    checkpoint.plan_reviewer_failover_count += 1;
    if dispatch.purpose.as_deref() == Some("plan_review") {
        checkpoint.set_plan_reviewer_dispatch_snapshot(&dispatch);
    }
    checkpoint.reviewer_dispatches.insert(new_id.clone(), dispatch);
    Ok(Some(new_id))
}
