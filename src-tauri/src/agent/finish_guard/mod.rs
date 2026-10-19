//! finish 终态质询门禁与指令状态更新。
//!
//! 拆分为独立子模块：
//! - `diff`: 真实 Git Diff 解析与行号证据对账；
//! - `state_updates`: 指令执行后的状态更新与 TargetBlock/Scope 桥接；
//! - `finish_guard_tests`: 终态门禁全量测试套件。

pub mod diff;
pub mod state_updates;

#[cfg(test)]
#[cfg(test)]
mod testkit;
#[cfg(test)]
mod tests_finish_contract;
#[cfg(test)]
mod tests_lifecycle;
#[cfg(test)]
mod tests_mode_gate;
#[cfg(test)]
mod tests_obligation;
#[cfg(test)]
mod tests_plan_gate;
#[cfg(test)]
mod tests_stage_review;

pub use diff::{file_paths_match, parse_file_line_ref, parse_unified_diff, DiffContext};
pub(crate) use diff::{
    noncode_evidence_skip_reasons, obligation_is_diagnostic, review_contract_evidence_blockers,
    undiffed_locatable_notices,
};
pub(crate) use state_updates::{apply_instruction_state_updates, apply_todo_update_guarded};

use crate::agent::change_verification::{change_verification_blocker, ChangeVerification};
use crate::agent::checkpoint::AgentCheckpoint;
use crate::agent::context;
use crate::agent::review_contract::ReviewContract;
use crate::agent::types::{AgentInstruction, TodoItem};
use crate::db::DbState;
use serde_json::json;

pub(crate) fn open_todo_descriptions(todos: &[TodoItem]) -> Vec<String> {
    todos
        .iter()
        .filter(|item| !item.is_review_reference())
        .filter(|item| {
            matches!(
                item.status.trim().to_ascii_lowercase().as_str(),
                "pending" | "in_progress"
            )
        })
        .map(|item| {
            let description = item.description.trim();
            if description.is_empty() {
                format!("Todo #{}", item.id)
            } else {
                description.to_string()
            }
        })
        .collect()
}

pub(crate) fn should_challenge_finish(
    is_finish: bool,
    succeeded: bool,
    _finish_reviewed: bool,
    todos: &[TodoItem],
) -> bool {
    is_finish && succeeded && !open_todo_descriptions(todos).is_empty()
}

/// 汇总 finish 时仍未闭合的责任（带代码 Diff 上下文）。
/// `diagnostic_questions`：triage 下发的取证问题清单——命中的义务按
/// 诊断义务处理（静态定位即可），其余义务维持 diff 对账。
pub(crate) fn completion_claim_blockers_with_diff(
    todos: &[TodoItem],
    review_contract: Option<&ReviewContract>,
    review_contract_started_this_turn: bool,
    side_effect_count: usize,
    diff_context: Option<&DiffContext>,
    workspace_root: Option<&std::path::Path>,
    diagnostic_questions: &[String],
) -> Vec<String> {
    let mut blockers = open_todo_descriptions(todos);
    if review_contract_started_this_turn {
        blockers.push("审查契约刚在本轮建立，尚未执行任何契约义务".to_string());
    }
    if let Some(contract) = review_contract {
        blockers.extend(contract.open_descriptions());
        if side_effect_count > 0 {
            blockers.extend(review_contract_evidence_blockers(
                contract,
                side_effect_count,
                diff_context,
                workspace_root,
                diagnostic_questions,
            ));
        }
    }
    blockers
}

/// 汇总 finish 时仍未闭合的责任。
///
/// 保持向下兼容既有 5 参数签名，默认无 diff 上下文。
pub(crate) fn finish_blockers_with_diff(
    todos: &[TodoItem],
    review_contract: Option<&ReviewContract>,
    review_contract_started_this_turn: bool,
    side_effect_count: usize,
    change_verification: Option<&ChangeVerification>,
    diff_context: Option<&DiffContext>,
    workspace_root: Option<&std::path::Path>,
    diagnostic_questions: &[String],
) -> Vec<String> {
    let mut blockers = completion_claim_blockers_with_diff(
        todos,
        review_contract,
        review_contract_started_this_turn,
        side_effect_count,
        diff_context,
        workspace_root,
        diagnostic_questions,
    );
    if let Some(b) = change_verification_blocker(side_effect_count, change_verification) {
        blockers.push(b);
    }
    blockers
}

pub(crate) fn finish_blockers(
    todos: &[TodoItem],
    review_contract: Option<&ReviewContract>,
    review_contract_started_this_turn: bool,
    side_effect_count: usize,
    change_verification: Option<&ChangeVerification>,
) -> Vec<String> {
    finish_blockers_with_diff(
        todos,
        review_contract,
        review_contract_started_this_turn,
        side_effect_count,
        change_verification,
        None,
        None,
        &[],
    )
}

pub(crate) fn should_challenge_finish_with_blockers(
    is_finish: bool,
    succeeded: bool,
    _finish_reviewed: bool,
    blockers: &[String],
) -> bool {
    is_finish && succeeded && !blockers.is_empty()
}

/// 兼容旧调用签名；检查次数不再被用作验收结果。
pub(crate) fn should_challenge_finish_with_unresolved(
    is_finish: bool,
    succeeded: bool,
    _finish_reviewed: bool,
    _finish_challenge_count: u32,
    blockers: &[String],
    _unresolved: bool,
) -> bool {
    is_finish && succeeded && !blockers.is_empty()
}
