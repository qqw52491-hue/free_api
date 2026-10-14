//! 改动自动验证：机械 diff 证据与变更确认。

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
use crate::agent::finish_guard::{
    apply_instruction_state_updates, finish_blockers, finish_blockers_with_diff,
    review_contract_evidence_blockers, should_challenge_finish,
    should_challenge_finish_with_blockers, should_challenge_finish_with_unresolved, DiffContext,
};
use crate::agent::loop_runner::run_call_llm_and_run_tool_phases;
use crate::agent::mcp::{McpClient, PluginRegistry};
use crate::agent::review_contract::{apply_review_contract_update, ReviewContract};
use crate::agent::run_state::{AgentRunEvent, AgentRunMachine, AgentRunState};
use crate::agent::task_store;
use crate::agent::types::{AgentInstruction, DispatchResult, TodoItem, RUN_INTERRUPTED_ROUTE};
use crate::agent::workspace_lock::{acquire_workspace_lock, ChildWorkspaceGuard};
use crate::db::DbState;
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, watch};
use tokio::time::{sleep, Duration};

pub(crate) fn try_auto_verify_change(
    context: &mut context::SandwichContext,
    step_id: usize,
    instruction: &AgentInstruction,
    app: &AppHandle,
    session_id: &str,
    command_succeeded: bool,
    workspace: &std::path::Path,
) {
    if context.side_effects.len() <= context.inherited_side_effects_count
        || context.change_verification.as_ref().map_or(
            false,
            crate::agent::change_verification::ChangeVerification::is_resolved,
        )
    {
        return;
    }

    let Some(cmd) =
        crate::agent::change_verification::extract_command_from_params(&instruction.get_params())
    else {
        return;
    };

    // 探针-写入物理绑定：探针命令必须引用待验证副作用记录中的具体对象
    // （路径/名称 token），否则 `which python` 这类无关探针不能替代码写入结账。
    let pending_refs: Vec<String> = context.side_effects[context.inherited_side_effects_count..]
        .iter()
        .flat_map(|r| {
            std::iter::once(r.description.clone()).chain(r.affected_paths.iter().cloned())
        })
        .collect();
    let contract_verify_commands: Vec<&str> = context
        .review_contract
        .as_ref()
        .map(|rc| {
            rc.obligations
                .iter()
                .filter_map(|o| o.verify_command.as_deref())
                .collect()
        })
        .unwrap_or_default();
    let decision = crate::agent::change_verification::decide_auto_verify(
        &cmd,
        command_succeeded,
        &pending_refs,
        &contract_verify_commands,
        crate::agent::change_verification::infer_adaptive_verification_command_in(
            workspace,
            &context.new_affected_paths(),
        )
        .as_deref(),
    );

    // 失败路径不自动闭合：非零退出是否达标取决于任务意图
    // （卸载 vs 安装方向相反），宿主不猜。对绑定的存在性探针发举证指引，
    // 让模型显式声明"该状态即目标"，终审可凭 step_N 核验。
    if matches!(
        decision,
        crate::agent::change_verification::AutoVerifyDecision::AbsenceProbeGuidance
    ) {
        let snippet: String = if cmd.chars().count() > 60 {
            format!("{}...", cmd.chars().take(60).collect::<String>())
        } else {
            cmd.clone()
        };
        context.add_error_feedback(&format!(
            "存在性探针 `{}` 返回非零退出（step_{}）。若该状态正是目标\
            （卸载/禁用/删除类任务），请调用 change_verification_update\
            （status=verified, evidence_refs=[\"step_{}\"]）显式举证闭合验证；\
            若目标相反（安装/启用/部署），请修复后重试。",
            snippet, step_id, step_id
        ));
        let _ = app.emit(
            &format!("agent-log-{}", session_id),
            format!(
                "🧪 存在性断言 `{}` 返回非零退出（step_{}）——是否达标取决于任务意图，已提示显式举证",
                snippet, step_id
            ),
        );
    }

    if matches!(
        decision,
        crate::agent::change_verification::AutoVerifyDecision::AutoVerify
    ) {
        let snippet = if cmd.chars().count() > 60 {
            format!("{}...", cmd.chars().take(60).collect::<String>())
        } else {
            cmd
        };
        let evidence = format!("step_{} ({} → exit 0)", step_id, snippet);
        context.change_verification = Some(crate::agent::change_verification::ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec![evidence],
            waived_reason: None,
        });
        let _ = app.emit(
            &format!("agent-log-{}", session_id),
            format!(
                "🧪 系统自动感知：检测到验证命令 `{}` 成功执行，已自动标记写入验证通过 (verified)",
                snippet
            ),
        );
    }
}
