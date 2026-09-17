//! 终态预检（prepare_finish）：与 finish 同一套门禁做交付就绪评估。
//! 就绪收据在 diff/scope/契约未变时被 finish 直接复用，不重复烧验证与终审。
//! 从 run_agent_main_loop_in_workspace_inner 抽出，逻辑不变。

use crate::agent::checkpoint::AgentCheckpoint;
use crate::agent::context::types::SandwichContext;
use crate::agent::reviewer_gatekeeper::GitBaseline;
use crate::agent::router::ModelRoutingConfig;
use crate::agent::types::AgentInstruction;
use crate::db::DbState;
use chrono::{DateTime, Utc};
use std::path::Path;
use tauri::AppHandle;
use tokio::sync::watch;

/// 返回渲染后的 output_text（就绪/未就绪说明），并把收据挂到 checkpoint.pending_finalization。
pub(crate) async fn run_prepare_finish_precheck(
    app: &AppHandle,
    db: &DbState,
    context: &SandwichContext,
    checkpoint: &mut AgentCheckpoint,
    run_id: &str,
    final_session_id: &str,
    step_id: usize,
    workspace_dir: &Path,
    git_baseline: &GitBaseline,
    run_started_at: DateTime<Utc>,
    model_routing: &ModelRoutingConfig,
    interrupt_rx: &mut watch::Receiver<u64>,
    open_todos: &[String],
    gate_notices: &[String],
    instruction: &AgentInstruction,
) -> String {
    let receipt = crate::agent::finalization::evaluate_prepare_finish(
        app,
        db,
        context,
        checkpoint,
        run_id,
        final_session_id,
        step_id,
        workspace_dir,
        git_baseline,
        run_started_at,
        model_routing,
        interrupt_rx,
        open_todos,
        gate_notices,
        instruction.prepare_finish.as_ref(),
    )
    .await;
    checkpoint.pending_finalization = Some(receipt.clone());
    crate::agent::telemetry::record_gate_fire(
        db,
        &crate::agent::telemetry::GateFireRecord {
            run_id,
            session_id: final_session_id,
            step_id: step_id as i64,
            gate: "finalization",
            route: if receipt.is_ready() {
                "finalization:prepared_ready"
            } else {
                "finalization:prepared_blocked"
            },
        },
    );
    let output_text = if receipt.is_ready() {
        format!(
            "✅ 终态预检就绪（receipt {}）：所有门禁已通过且证据已冻结。\n现在调用 finish 将直接复用本收据完成提交——diff/scope/契约未变时不再重复验证与终审。",
            receipt.receipt_id
        )
    } else {
        format!(
            "⚠️ 终态预检未就绪（receipt {}）：\n{}\n请逐项闭合后重新 prepare_finish，或修复后直接调用 finish。",
            receipt.receipt_id,
            receipt
                .blockers
                .iter()
                .map(|b| format!("- {}", b))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    crate::agent::dispatcher::emit_tool_event(
        app,
        final_session_id,
        step_id,
        instruction,
        "succeeded",
        Some("prepare_finish".to_string()),
        Some(receipt.is_ready()),
        &output_text,
        None,
        None,
    );
    output_text
}
