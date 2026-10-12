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
    // 空转检测（与 finish 质询共用计数字段）：同一组阻塞连续预检仍未变，说明模型在跑无关命令。
    // 只追加提示，不拦截、不动重试预算。
    let spin_hint = if receipt.is_ready() {
        checkpoint.last_finish_blocker_sig = None;
        checkpoint.repeated_blocker_count = 0;
        String::new()
    } else {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        receipt.blockers.hash(&mut h);
        let sig = h.finish();
        if checkpoint.last_finish_blocker_sig == Some(sig) {
            checkpoint.repeated_blocker_count = checkpoint.repeated_blocker_count.saturating_add(1);
        } else {
            checkpoint.last_finish_blocker_sig = Some(sig);
            checkpoint.repeated_blocker_count = 1;
        }
        prepare_spin_hint(checkpoint.repeated_blocker_count)
    };
    let output_text = if receipt.is_ready() {
        format!(
            "✅ 终态预检就绪（receipt {}）：所有门禁已通过且证据已冻结。\n现在调用 finish 将直接复用本收据完成提交——diff/scope/契约未变时不再重复验证与终审。",
            receipt.receipt_id
        )
    } else {
        format!(
            "⚠️ 终态预检未就绪（receipt {}）：\n{}\n请逐项闭合后重新 prepare_finish，或修复后直接调用 finish。{}",
            receipt.receipt_id,
            receipt
                .blockers
                .iter()
                .map(|b| format!("- {}", b))
                .collect::<Vec<_>>()
                .join("\n"),
            spin_hint
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

/// 同一组预检阻塞连续未变 >=3 次时的指引：点破 echo / git diff --stat 这类命令不会改变契约状态。
fn prepare_spin_hint(repeated: u32) -> String {
    if repeated < 3 {
        return String::new();
    }
    format!(
        "\n\n【🚨 同一组阻塞已连续 {} 次预检未变化】你两次预检之间执行的命令（echo、git diff --stat 等）不会改变阻塞项，请不要再用它们“同步/闭合”。对照上方阻塞项选一条：\n① 审查义务缺证据 → 用 `review_contract_update` 把该义务标为 evidenced 并填 evidence_refs（diff 区间内的 文件:行号 或输出 ID），或标 waived 并说明理由；\n② 变更验证缺失 → 用 `change_verification_update` 以 verified + evidence_refs 闭合；\n③ 确实无法闭合 → 调用 `ask_human` 向用户呈报阻塞清单。",
        repeated
    )
}

#[cfg(test)]
mod spin_hint_tests {
    use super::prepare_spin_hint;

    #[test]
    fn hint_only_from_third_repeat() {
        assert!(prepare_spin_hint(1).is_empty());
        assert!(prepare_spin_hint(2).is_empty());
        let h = prepare_spin_hint(3);
        assert!(h.contains("review_contract_update") && h.contains("ask_human"));
    }
}
