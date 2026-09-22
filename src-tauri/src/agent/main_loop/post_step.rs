//! 单步收尾：todo 证据回填、Lesson 血缘记录、副作用台账/自动验证。
//! 从 run_agent_main_loop_in_workspace_inner 的步尾收尾段抽出，逻辑不变。

use crate::agent::context::types::SandwichContext;
use crate::agent::types::{AgentInstruction, DispatchResult};
use crate::db::DbState;
use tauri::AppHandle;

use super::auto_verify::try_auto_verify_change;
use crate::agent::context::semantic_stall::StallSignal;

/// 语义打转收尾检测：连续纯调查（复读已知路径、无状态变更）累计到阈值时
/// 注入收敛提示；深度打转时宿主代花一次 Pro 升维配额——模型卡死时往往
/// 没有"该呼救"的元认知，触发权由宿主接管（fail-open，永不拦截）。
pub(crate) fn detect_semantic_stall(
    context: &mut SandwichContext,
    instruction: &AgentInstruction,
    result: &DispatchResult,
    workspace_dir: &std::path::Path,
) {
    match context
        .semantic_stall
        .record(instruction, result.success, workspace_dir)
    {
        Some(StallSignal::Notice(streak)) => {
            context.add_error_feedback(&format!(
                "【🧭 收敛提示】你已连续 {} 步纯调查（未触及新路径证据、未产生任何状态变更）。\n\
                 继续复读同一批文件不会产生新信息。【必须采取的行动】：下一步先综合当前证据——\n\
                 列出已确证的事实、候选根因假设与各自缺口，然后收敛为一个可执行动作\
                 （submit_plan / 写入修改 / 派发侦察 / 验证命令），不要继续无目标泛读。",
                streak
            ));
        }
        Some(StallSignal::Escalate(streak)) => {
            // 宿主代花升维配额：配额耗尽或没有活跃目标块时降级为纯提示。
            let escalated = context
                .active_target_block_mut()
                .map(|b| b.request_pro_escalation().is_ok())
                .unwrap_or(false);
            let suffix = if escalated {
                "宿主已代你申请一次 Pro 强模型支援，下一步将自动路由 Pro 执行收敛推理。"
            } else {
                "升维配额不可用（无活跃目标块或配额已用尽）——请用当前模型立即收敛为可执行动作。"
            };
            context.add_error_feedback(&format!(
                "【🚨 深度打转】你已连续 {} 步纯调查未收敛——证据面大概率已足够，缺的\
                 是把证据组装成假设。{}\n【必须采取的行动】：停止泛读，立即输出当前最优假设\
                 与最小证伪/修复动作。",
                streak, suffix
            ));
        }
        None => {}
    }
}

/// 防虚假勾选：将当步真实 output_id 回填到标记为 done 但缺少证据的 Todo（需在 output_id 已知后执行）
pub(crate) fn backfill_todo_evidence(
    context: &mut SandwichContext,
    instruction: &mut AgentInstruction,
    output_id: &str,
) {
    if !instruction.todo_update.is_empty() {
        for item in &mut instruction.todo_update {
            if item.status == "done" && item.evidence_output_id.is_none() {
                item.evidence_output_id = Some(output_id.to_string());
            }
        }
        // 证据关联后同步更新 todo_list（走监管继承守卫：[审查发现] 禁止被静默剔除）
        crate::agent::finish_guard::apply_todo_update_guarded(context, &instruction.todo_update);
    }
}

/// Lesson 血缘：search 命中的 lesson 召回记录 + 显式应用后的因果反馈打分
pub(crate) fn record_lesson_lineage(
    db: &DbState,
    final_session_id: &str,
    step_id: usize,
    instruction: &AgentInstruction,
    result: &DispatchResult,
) {
    // ── Lesson 血缘召回记录：若当步工具是 search，将命中的 lesson 类型结果写入血缘表 ───────
    if instruction.get_tool().eq_ignore_ascii_case("search") && result.success {
        if let Ok(search_result) = serde_json::from_str::<serde_json::Value>(&result.stdout) {
            let recalled_lesson_ids: Vec<String> = search_result
                .get("hits")
                .and_then(|h| h.as_array())
                .map(|hits| {
                    hits.iter()
                        .filter_map(|h| {
                            let kind = h.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                            if kind == "lesson" || kind == "summary" {
                                h.get("id_or_path")
                                    .and_then(|id| id.as_str())
                                    .map(String::from)
                            } else {
                                None
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            if !recalled_lesson_ids.is_empty() {
                crate::agent::lesson::record_lesson_recall(
                    db,
                    final_session_id,
                    step_id,
                    &recalled_lesson_ids,
                );
            }
        }
    }

    // ── Lesson 显式血缘因果反馈打分 ────────────────────────────────────
    if let Some(ref applied) = instruction.applied_lessons {
        if !applied.is_empty() {
            crate::agent::lesson::feedback_on_applied_lessons(
                db,
                final_session_id,
                step_id,
                applied,
                result.success,
            );
        }
    }
}

/// 记录不可逆副作用安全台账 (Side-Effects Ledger)：
/// 关键不可逆操作成功执行后记录台账；纯只读命令及临时草稿 (session_tmp) 不入台账；
/// 否则走写入验证自愈（测试命令成功自动闭合 unverified 写入）。
pub(crate) fn record_side_effect_or_verify(
    context: &mut SandwichContext,
    step_id: usize,
    instruction: &AgentInstruction,
    result: &DispatchResult,
    app: &AppHandle,
    final_session_id: &str,
) {
    let is_tmp_write = instruction.get_tool().eq_ignore_ascii_case("local_file")
        && crate::agent::tools::local_file::is_dispatch_result_tmp(result);
    // 纯验证/类型检查命令不算副作用——它们的退出码本身就是判据，不是待验证的写入。
    // 若把 `npx vue-tsc` 记成一笔副作用，验证命令自己制造挂账，永远无法自动闭合。
    // 复合命令（测试 + 其它动作拼接）仍按副作用入账，宁可保守也不漏记。
    let is_verification_cmd = crate::agent::change_verification::extract_command_from_params(
        &instruction.get_params(),
    )
    .map(|c| crate::agent::change_verification::is_test_or_verification_command(&c))
    .unwrap_or(false);
    if result.success
        && !is_tmp_write
        && !is_verification_cmd
        && crate::agent::context::AuditLedger::is_mutating_side_effect_with_action(
            &instruction.get_tool(),
            &instruction.get_action(),
            &instruction.get_params(),
        )
    {
        let safe_params = crate::agent::context::AuditLedger::format_side_effect_params(
            &instruction.get_params(),
        );
        let affected_paths = crate::agent::change_verification::extract_file_paths_from_params(
            &instruction.get_params(),
        );
        context.record_side_effect_with_paths(
            step_id,
            instruction.get_tool(),
            format!("{}: {}", instruction.description, safe_params),
            affected_paths,
        );
    } else {
        // 🌟 写入验证智能自愈与自动感知 (Auto-verification):
        // 当存在未闭合的本地写入，且当前执行的命令是测试/验证命令且执行成功时，系统自动闭合为 verified！
        // 失败的命令同样进入判定：存在性探针（which/test -e/pacman -Q 等）的
        // 非零退出本身就是"目标已不存在"的状态断言——运维任务的天然验证证据。
        try_auto_verify_change(
            context,
            step_id,
            instruction,
            app,
            final_session_id,
            result.success,
        );
    }
}
