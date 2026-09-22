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

pub use diff::{
    file_paths_match, parse_file_line_ref, parse_unified_diff, DiffContext,
};
pub(crate) use diff::{
    delivery_claims_code_modification, noncode_evidence_skip_reasons,
    review_contract_evidence_blockers,
};
pub(crate) use state_updates::{apply_todo_update_guarded, apply_instruction_state_updates};

use crate::agent::change_verification::{change_verification_blocker, ChangeVerification};
use crate::agent::checkpoint::AgentCheckpoint;
use crate::agent::context;
use crate::agent::dispatcher::is_finish_instruction;
use crate::agent::evaluation_analysis::{
    assess_complex_delivery_summary, summary_makes_root_cause_claim,
};
use crate::agent::review_contract::ReviewContract;
use crate::agent::types::{AgentInstruction, TodoItem};
use crate::db::DbState;
use serde_json::json;

pub(crate) fn open_todo_descriptions(todos: &[TodoItem]) -> Vec<String> {
    todos
        .iter()
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
    finish_reviewed: bool,
    todos: &[TodoItem],
) -> bool {
    is_finish && succeeded && !finish_reviewed && !open_todo_descriptions(todos).is_empty()
}

/// 汇总 finish 时仍未闭合的责任（带代码 Diff 上下文）。
/// `diagnostic_questions`：triage 下发的取证问题清单——命中的义务按
/// 诊断义务处理（静态定位即可），其余义务维持 diff 对账。
pub(crate) fn finish_blockers_with_diff(
    todos: &[TodoItem],
    review_contract: Option<&ReviewContract>,
    review_contract_started_this_turn: bool,
    side_effect_count: usize,
    change_verification: Option<&ChangeVerification>,
    diff_context: Option<&DiffContext>,
    workspace_root: Option<&std::path::Path>,
    delivery_text: Option<&str>,
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
                delivery_text,
                diagnostic_questions,
            ));
        }
    }
    if let Some(blocker) = change_verification_blocker(side_effect_count, change_verification) {
        blockers.push(blocker);
    }
    blockers
}

/// 汇总 finish 时仍未闭合的责任。
///
/// 保持向下兼容既有 5 参数签名，默认无 diff 上下文。
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
        None,
        &[],
    )
}

pub(crate) fn should_challenge_finish_with_blockers(
    is_finish: bool,
    succeeded: bool,
    finish_reviewed: bool,
    blockers: &[String],
) -> bool {
    is_finish && succeeded && !finish_reviewed && !blockers.is_empty()
}

/// 主根因的证据边界是交付正确性的前置条件，但绝不能造成无上限的死循环死锁。
/// 累计质询达到 MAX_FINISH_CHALLENGES（2 次）时，硬性熔断放行，避免用户任务卡死。
pub(crate) fn should_challenge_finish_with_complex_delivery_blocker(
    is_finish: bool,
    succeeded: bool,
    finish_reviewed: bool,
    finish_challenge_count: u32,
    blockers: &[String],
    complex_delivery_unresolved: bool,
) -> bool {
    const MAX_FINISH_CHALLENGES: u32 = 2;
    if !is_finish || !succeeded || blockers.is_empty() {
        return false;
    }
    // 防死循环硬性熔断保护：累计质询已达 2 次时，绝不再次死循环拦截
    if finish_challenge_count >= MAX_FINISH_CHALLENGES {
        return false;
    }
    !finish_reviewed || complex_delivery_unresolved
}

/// 复杂诊断 finish 缺少边界披露时复用既有可恢复拦截。
/// 该检查只消费当前内存中的 summary 生成布尔结论，既不持久化也不回显用户交付正文。
pub(crate) fn complex_delivery_blocker(
    context: &context::SandwichContext,
    instruction: &AgentInstruction,
    is_finish: bool,
    succeeded: bool,
) -> Option<String> {
    if !is_finish || !succeeded {
        return None;
    }
    // 对于纯对话、解释答疑、或没有任何新增代码文件修改的任务，免除严苛的主根因四件套拦截
    if context.side_effects.len() == context.inherited_side_effects_count {
        return None;
    }
    // ReviewContract 是模型首轮规划后持久化的显式复杂任务声明。
    // 不再依赖少数自然语言关键词：复杂 Bug、架构变更、公式证明等跨领域任务
    // 只要建立契约，就共享同一套依据—验证—交付闭环；普通任务保持轻量。
    if context.review_contract.is_none() {
        // 未建契约的交付：不要求完整四件套，但只要交付自身宣称了主根因，
        // 就必须就地校准（直接证据 + 结论强度）——"谁宣称根因，谁就校准"，
        // 防止把复杂诊断的边界披露漏成无契约即免检。
        let Some(summary) = instruction
            .command
            .as_ref()
            .and_then(|command| command.get("summary"))
            .and_then(|summary| summary.as_str())
        else {
            return None;
        };
        return if summary_makes_root_cause_claim(summary)
            && !assess_complex_delivery_summary(summary).root_cause_calibrated
        {
            Some("交付提出了主根因结论，但缺少就地证据与结论强度（需在主根因小节内给出直接证据并标注 已证实/部分支持/未验证）".to_string())
        } else {
            None
        };
    }
    let Some(summary) = instruction
        .command
        .as_ref()
        .and_then(|command| command.get("summary"))
        .and_then(|summary| summary.as_str())
    else {
        return Some("复杂终态缺少 summary，无法核验主根因的证据边界".to_string());
    };
    let assessment = assess_complex_delivery_summary(summary);
    (!assessment.compliant).then_some(
        "复杂终态缺少已核验范围、未核验范围或关键假设、结论强度，或主根因就地的直接证据与结论强度"
            .to_string(),
    )
}

