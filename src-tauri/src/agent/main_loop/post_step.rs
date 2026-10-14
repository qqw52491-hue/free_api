//! 单步收尾：todo 证据回填、Lesson 血缘记录、副作用台账/自动验证。
//! 从 run_agent_main_loop_in_workspace_inner 的步尾收尾段抽出，逻辑不变。

use crate::agent::context::types::SandwichContext;
use crate::agent::types::{AgentInstruction, DispatchResult};
use crate::db::DbState;
use tauri::AppHandle;

use super::auto_verify::try_auto_verify_change;

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
    workspace_dir: &std::path::Path,
) {
    let is_tmp_write = instruction.get_tool().eq_ignore_ascii_case("local_file")
        && crate::agent::tools::local_file::is_dispatch_result_tmp(result);
    let tool = instruction.get_tool();
    let local_shell = tool
        .rsplit('/')
        .next()
        .is_some_and(|t| t.eq_ignore_ascii_case("local_shell"))
        || (tool.eq_ignore_ascii_case("mcp")
            && instruction.get_action().eq_ignore_ascii_case("local_shell"));
    let nonmutating_shell = local_shell
        && crate::agent::change_verification::extract_command_from_params(
            &instruction.get_params(),
        )
        .is_some_and(|c| {
            crate::agent::change_verification::is_observation_or_validation_script(&c)
        });
    // 失败的 shell 也可能已经改了文件；实测差集不因非零退出而丢弃。
    let shell_touched = std::mem::take(&mut context.shell_touched_paths);
    let should_record = should_record_effect(
        result.success,
        is_tmp_write,
        nonmutating_shell,
        !shell_touched.is_empty(),
        crate::agent::context::AuditLedger::is_mutating_side_effect_with_action(
            &tool,
            &instruction.get_action(),
            &instruction.get_params(),
        ),
    );
    if should_record {
        let safe_params = crate::agent::context::AuditLedger::format_side_effect_params(
            &instruction.get_params(),
        );
        let affected_paths = {
            let mut paths = crate::agent::change_verification::extract_file_paths_from_params(
                &instruction.get_params(),
            );
            // shell 类命令参数里没有 path 字段——回填「步骤真实触碰的新增脏文件」
            // 差集（脏探针 post−pre），使台账对本 session 的 shell 写入也能按路径对账
            // （git 破坏命令门的「自清理放行」分支依赖这个集合）。
            if paths.is_empty() && !shell_touched.is_empty() {
                paths = shell_touched.clone();
            }
            paths
        };
        // 内容指纹入账：写入时刻的 git blob sha。供「我写的被删/还原/提交」
        // 判定——porcelain 只报脏净，分不清 commit 与 revert，必须比内容。
        for p in &affected_paths {
            let rel = {
                let path = std::path::Path::new(p);
                if path.is_absolute() {
                    path.strip_prefix(workspace_dir)
                        .ok()
                        .map(|r| r.to_string_lossy().to_string())
                } else {
                    Some(p.clone())
                }
            };
            if let Some(rel) = rel {
                if let Some(sha) =
                    crate::agent::review_tools::git_worktree_blob(workspace_dir, &rel)
                {
                    context.written_file_digests.insert(rel, sha);
                }
            }
        }
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
            workspace_dir,
        );
    }
}

/// 记账与命令权限分离；只有确认为观察/验证的脚本免于重复制造验证批次。
fn should_record_effect(
    succeeded: bool,
    temporary: bool,
    nonmutating: bool,
    observed_changes: bool,
    mutating_tool: bool,
) -> bool {
    !temporary && (observed_changes || (succeeded && mutating_tool && !nonmutating))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_shell_writes_are_not_lost_and_queries_do_not_reset_verification() {
        assert!(should_record_effect(false, false, false, true, true));
        assert!(!should_record_effect(true, false, true, false, true));
        assert!(should_record_effect(true, false, true, true, true));
        assert!(should_record_effect(true, false, false, false, true));
        assert!(!should_record_effect(true, true, false, true, true));
    }
}
