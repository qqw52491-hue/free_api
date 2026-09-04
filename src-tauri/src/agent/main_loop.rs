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
use crate::agent::evaluation_analysis::{assess_complex_delivery_summary, summary_makes_root_cause_claim};
use crate::agent::loop_runner::run_call_llm_and_run_tool_phases;
use crate::agent::mcp::{McpClient, PluginRegistry};
use crate::agent::review_contract::{apply_review_contract_update, ReviewContract};
use crate::agent::run_state::{AgentRunEvent, AgentRunMachine, AgentRunState};
use crate::agent::task_store;
use crate::agent::types::{AgentInstruction, TodoItem, RUN_INTERRUPTED_ROUTE};
use crate::db::DbState;
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, watch};
use tokio::time::{sleep, Duration};

// ---------------------------------------------------------------- 工作区软锁
// 同一项目目录同时只允许一个 run，防止两个任务并发写同一堆文件。
// 仅项目会话参与锁定（含微信 scratch 项目）；纯 GUI 无项目会话保持原有并行行为，
// 评测工作区彼此隔离也不受影响。守卫按 RAII 随 run 结束（含早退/中断）自动释放。

fn workspace_locks() -> &'static std::sync::Mutex<std::collections::HashMap<String, String>> {
    static WORKSPACE_LOCKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, String>>,
    > = std::sync::OnceLock::new();
    WORKSPACE_LOCKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

pub(crate) struct WorkspaceRunGuard {
    key: String,
}

impl Drop for WorkspaceRunGuard {
    fn drop(&mut self) {
        if let Ok(mut locks) = workspace_locks().lock() {
            locks.remove(&self.key);
        }
    }
}

fn acquire_workspace_lock(workspace_dir: &std::path::Path, goal: &str) -> Result<WorkspaceRunGuard, String> {
    let key = workspace_dir
        .canonicalize()
        .unwrap_or_else(|_| workspace_dir.to_path_buf())
        .to_string_lossy()
        .to_string();
    let mut locks = workspace_locks().lock().map_err(|e| e.to_string())?;
    if let Some(occupied_by) = locks.get(&key) {
        return Err(format!(
            "工作区已被占用：{key}（正在执行「{occupied_by}」）。同一项目目录同时只允许一个任务，请等待其完成或先停止该任务。"
        ));
    }
    locks.insert(key.clone(), goal.chars().take(60).collect());
    Ok(WorkspaceRunGuard { key })
}

/// P5: 父 run 终态时自动统一回收该 run 下所有子 writer 副本
struct ChildWorkspaceGuard<'a> {
    db: &'a DbState,
    run_id: &'a str,
}

impl<'a> Drop for ChildWorkspaceGuard<'a> {
    fn drop(&mut self) {
        crate::agent::child_writer::cleanup_child_workspaces(self.db, self.run_id);
    }
}

/// Agent 每个任务最多执行步数（可按项目需求调整）
const MAX_STEPS: usize = 300;
/// 正常会话只做短暂态请求重试；后台回归可显式提高到上限以避免 Provider 波动中断取证。
pub(crate) const DEFAULT_TRANSIENT_LLM_REQUEST_RETRIES: u32 = 3;
const MAX_TRANSIENT_LLM_REQUEST_RETRIES: u32 = 100;

/// 仅将协议明确定义为未完成的 Todo 视为终态质询信号。
/// 未知状态不做猜测，`done` 与 `failed` 则已是模型显式关闭的事项。
fn open_todo_descriptions(todos: &[TodoItem]) -> Vec<String> {
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

fn should_challenge_finish(
    is_finish: bool,
    succeeded: bool,
    finish_reviewed: bool,
    todos: &[TodoItem],
) -> bool {
    is_finish && succeeded && !finish_reviewed && !open_todo_descriptions(todos).is_empty()
}

/// 汇总 finish 时仍未闭合的责任。
///
/// 真实写入只产生一项"最近写入批次待验证"义务，不按文件或编辑次数逐条要求测试；
/// 无成功写入的只读任务不会因此产生 blocker。
pub(crate) fn finish_blockers(
    todos: &[TodoItem],
    review_contract: Option<&ReviewContract>,
    review_contract_started_this_turn: bool,
    side_effect_count: usize,
    change_verification: Option<&ChangeVerification>,
) -> Vec<String> {
    let mut blockers = open_todo_descriptions(todos);
    if review_contract_started_this_turn {
        blockers.push("审查契约刚在本轮建立，尚未执行任何契约义务".to_string());
    }
    if let Some(contract) = review_contract {
        blockers.extend(contract.open_descriptions());
    }
    if let Some(blocker) = change_verification_blocker(side_effect_count, change_verification) {
        blockers.push(blocker);
    }
    blockers
}

fn should_challenge_finish_with_blockers(
    is_finish: bool,
    succeeded: bool,
    finish_reviewed: bool,
    blockers: &[String],
) -> bool {
    is_finish && succeeded && !finish_reviewed && !blockers.is_empty()
}

/// 主根因的证据边界是交付正确性的前置条件，但绝不能造成无上限的死循环死锁。
/// 累计质询达到 MAX_FINISH_CHALLENGES（2 次）时，硬性熔断放行，避免用户任务卡死。
fn should_challenge_finish_with_complex_delivery_blocker(
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
fn complex_delivery_blocker(
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

/// 应用模型本轮提出的状态更新。
/// ReviewContract 与 ChangeVerification 是两套正交账本，必须独立处理：
/// 前者是否存在或是否被拒绝，都不能阻止后者闭合最近写入的验证责任。
fn apply_instruction_state_updates(
    context: &mut context::SandwichContext,
    instruction: &AgentInstruction,
) -> bool {
    if !instruction.todo_update.is_empty() {
        context.todo_list = instruction.todo_update.clone();
    }

    let review_contract_was_present = context.review_contract.is_some();
    if let Some(update) = instruction.review_contract_update.clone() {
        if let Err(error) = apply_review_contract_update(&mut context.review_contract, update) {
            context.add_error_feedback(&format!(
                "【审查契约更新未采纳】{}\n请保留既有义务；每项必须为 open，或以 evidenced + evidence_refs、waived + waived_reason 闭合。",
                error
            ));
        }
    }

    if let Some(update) = instruction.change_verification_update.clone() {
        if let Err(error) =
            apply_change_verification_update(&mut context.change_verification, update)
        {
            context.add_error_feedback(&format!(
                "【写入验证更新未采纳】{}\n真实写入后，必须以 verified + evidence_refs 或 waived + waived_reason 闭合；新写入会自动使旧验证失效。",
                error
            ));
        }
    }

    if let Some(ref blocks) = instruction.target_blocks_update {
        if !blocks.is_empty() {
            context.block_manager.set_blocks(blocks.clone());
        }
    } else if context.block_manager.blocks.is_empty() && !instruction.todo_update.is_empty() {
        // 自动桥接：从 todo_update 自动生成默认 TargetBlocks，平滑接入状态机
        let auto_blocks: Vec<crate::agent::target_block::TargetBlock> = instruction
            .todo_update
            .iter()
            .map(|t| {
                let mut b = crate::agent::target_block::TargetBlock::new(
                    format!("block-{}", t.id),
                    &t.description,
                );
                // 自动识别高难度词根赋权
                let desc_lower = t.description.to_lowercase();
                if desc_lower.contains("重构")
                    || desc_lower.contains("架构")
                    || desc_lower.contains("迁移")
                    || desc_lower.contains("并发")
                {
                    b.tier = "pro".to_string();
                }
                b
            })
            .collect();
        context.block_manager.set_blocks(auto_blocks);
    }

    // 接收执行模型提交的自述
    if let Some(ref report) = instruction.self_report {
        if let Some(block) = context.active_target_block_mut() {
            block.self_report = Some(report.clone());
        }
    }

    // 处理执行模型主动呼叫 Pro 援助（逃生通道）
    if instruction.request_escalation == Some(true) {
        let escalation_result = if let Some(block) = context.active_target_block_mut() {
            let id = block.id.clone();
            Some((id, block.request_pro_escalation()))
        } else {
            None
        };

        if let Some((id, res)) = escalation_result {
            match res {
                Ok(_) => {
                    context.add_error_feedback(&format!(
                        "【🚀 逃生通道已激活】目标块 [{}] 已成功申请临时 Pro 强模型支援，下一步骤将使用 Pro 执行复杂推理。",
                        id
                    ));
                }
                Err(err) => {
                    context.add_error_feedback(&format!("【⚠️ 逃生通道申请驳回】{}", err));
                }
            }
        }
    }

    // 处理执行模型申请动态扩展修改边界 (Scope)
    if let Some(ref ext) = instruction.scope_extension {
        if !ext.paths.is_empty() {
            if ext.reason.trim().is_empty() {
                context.add_error_feedback("【⚠️ Scope 扩展驳回】申请扩展修改边界时必须在 reason 字段中提供正当的技术联动理由，不得空置。");
            } else {
                let expanded_info = if let Some(block) = context.active_target_block_mut() {
                    block.expand_scope(&ext.paths);
                    Some((block.id.clone(), ext.paths.clone(), ext.reason.clone()))
                } else {
                    None
                };
                if let Some((id, paths, reason)) = expanded_info {
                    context.add_error_feedback(&format!(
                        "【✅ Scope 边界扩展已放行】目标块 [{}] 允许修改范围已扩充包含：{:?}（理由：{}）",
                        id, paths, reason
                    ));
                }
            }
        }
    }

    !review_contract_was_present && context.review_contract.is_some()
}

#[cfg(test)]
mod finish_guard_tests {
    use super::*;

    /// 测试夹具：直接向 agent_child_runs 写入/更新/清理子任务行，用于还原门禁的真实判定场景。
    fn insert_child_run(
        db: &DbState,
        id: &str,
        parent_run_id: &str,
        status: &str,
        result_json: Option<&str>,
        consumed_at: Option<&str>,
    ) {
        let conn = db.0.lock().unwrap();
        let result = result_json
            .map(|r| format!("'{}'", r))
            .unwrap_or_else(|| "NULL".to_string());
        let consumed = consumed_at
            .map(|c| format!("'{}'", c))
            .unwrap_or_else(|| "NULL".to_string());
        conn.execute(
            &format!(
                "INSERT OR REPLACE INTO agent_child_runs
                 (id, parent_run_id, parent_session_id, workspace_root, model_id, status, started_at, kind, result_json, consumed_at)
                 VALUES ('{id}', '{parent}', 'sess', '{ws}', 'model', '{status}', '2026-01-01T00:00:00Z', 'review', {result}, {consumed})",
                id = id,
                parent = parent_run_id,
                ws = std::env::temp_dir().display(),
                status = status,
                result = result,
                consumed = consumed,
            ),
            [],
        )
        .unwrap();
    }

    fn mark_child_run(db: &DbState, id: &str, status: &str) {
        let conn = db.0.lock().unwrap();
        conn.execute(
            &format!(
                "UPDATE agent_child_runs SET status = '{status}' WHERE id = '{id}'",
                status = status,
                id = id,
            ),
            [],
        )
        .unwrap();
    }

    fn delete_child_runs(db: &DbState, parent_run_id: &str) {
        let conn = db.0.lock().unwrap();
        conn.execute(
            &format!("DELETE FROM agent_child_runs WHERE parent_run_id = '{parent}'", parent = parent_run_id),
            [],
        )
        .unwrap();
    }

    fn todo(id: usize, status: &str, description: &str) -> TodoItem {
        TodoItem {
            id,
            status: status.to_string(),
            description: description.to_string(),
            evidence_output_id: None,
        }
    }

    #[test]
    fn only_explicit_open_todos_trigger_a_first_finish_challenge() {
        let todos = vec![
            todo(1, "pending", "核验 dispatcher"),
            todo(2, "in_progress", "核验 checkpoint"),
            todo(3, "done", "已读 main_loop"),
            todo(4, "failed", "已记录不可用命令"),
        ];

        assert_eq!(
            open_todo_descriptions(&todos),
            vec!["核验 dispatcher", "核验 checkpoint"]
        );
        assert!(should_challenge_finish(true, true, false, &todos));
    }

    #[test]
    fn closed_or_unknown_todos_do_not_block_finish() {
        let todos = vec![
            todo(1, "done", "已完成"),
            todo(2, "failed", "已失败并记录"),
            todo(3, "maybe", "旧格式未知状态"),
        ];

        assert!(open_todo_descriptions(&todos).is_empty());
        assert!(!should_challenge_finish(true, true, false, &todos));
    }

    #[test]
    fn a_reviewed_finish_or_failed_finish_is_not_challenged_again() {
        let todos = vec![todo(1, "pending", "尚未核验")];

        assert!(!should_challenge_finish(true, true, true, &todos));
        assert!(!should_challenge_finish(true, false, false, &todos));
        assert!(!should_challenge_finish(false, true, false, &todos));
    }

    #[test]
    fn complex_delivery_blocker_remains_active_after_an_initial_finish_challenge() {
        let blockers = vec!["复杂终态缺少主根因直接证据与结论强度".to_string()];
        // 首次重试（count = 1）依然保持质询
        assert!(should_challenge_finish_with_complex_delivery_blocker(
            true, true, true, 1, &blockers, true
        ));
        // 达到上限（count = 2）硬性熔断放行，绝不无限循环
        assert!(!should_challenge_finish_with_complex_delivery_blocker(
            true, true, true, 2, &blockers, true
        ));
        assert!(!should_challenge_finish_with_complex_delivery_blocker(
            true, true, true, 0, &blockers, false
        ));
        assert!(!should_challenge_finish_with_complex_delivery_blocker(
            true, false, true, 0, &blockers, true
        ));
    }

    #[test]
    fn root_cause_claim_without_contract_still_requires_inplace_calibration() {
        let mut context =
            context::SandwichContext::new("system".to_string(), "请诊断缓存问题".to_string());
        context.side_effects.push(crate::agent::context::SideEffectRecord {
            step_id: 1,
            tool: "write".to_string(),
            description: "fake".to_string(),
            timestamp: "now".to_string(),
        });
        // 无契约 + 交付宣称主根因但未就地校准 → 拦截
        let claiming = AgentInstruction {
            command: Some(serde_json::json!({ "summary": "### 主根因\n缓存模块未设置过期时间导致泄漏。" })),
            ..Default::default()
        };
        let blocker = complex_delivery_blocker(&context, &claiming, true, true);
        assert!(blocker.is_some(), "宣称主根因必须就地校准，即使未建契约");
        // 校准后（直接证据 + 结论强度）放行
        let calibrated = AgentInstruction {
            command: Some(serde_json::json!({ "summary": "### 主根因\n根因证据：CacheService.rs:42 未设置 TTL（直接证据）。结论强度：已证实。\n已核验范围：TTL 路径全部读取；未核验范围：第三方缓存插件；结论强度：已证实" })),
            ..Default::default()
        };
        assert!(complex_delivery_blocker(&context, &calibrated, true, true).is_none());
        // 无契约 + 普通交付（无主根因宣称）→ 不拦截，简单任务保持轻量
        let plain = AgentInstruction {
            command: Some(serde_json::json!({ "summary": "# 已完成文件导出" })),
            ..Default::default()
        };
        assert!(complex_delivery_blocker(&context, &plain, true, true).is_none());
    }

    #[test]
    fn complex_delivery_uses_explicit_review_contract_instead_of_goal_keywords() {
        let instruction = AgentInstruction {
            command: Some(serde_json::json!({ "summary": "# 简短交付" })),
            ..Default::default()
        };
        let mut context =
            context::SandwichContext::new("system".to_string(), "请诊断缓存问题".to_string());
        context.side_effects.push(crate::agent::context::SideEffectRecord {
            step_id: 1,
            tool: "write".to_string(),
            description: "fake".to_string(),
            timestamp: "now".to_string(),
        });
        assert!(complex_delivery_blocker(&context, &instruction, true, true).is_none());

        context.review_contract = Some(ReviewContract {
            scope_statement: "核验缓存调用链".to_string(),
            obligations: vec![crate::agent::review_contract::ReviewObligation {
                id: "cache".to_string(),
                question: "缓存失效是否真实触发？".to_string(),
                status: "evidenced".to_string(),
                evidence_refs: vec!["output_id:cache-read".to_string()],
                waived_reason: None,
            }],
        });
        assert!(complex_delivery_blocker(&context, &instruction, true, true).is_some());
    }

    #[test]
    fn review_contract_blocks_first_finish_without_forcing_step_counts() {
        let open_contract = ReviewContract {
            scope_statement: "审查上下文设计".to_string(),
            obligations: vec![crate::agent::review_contract::ReviewObligation {
                id: "implementation".to_string(),
                question: "实现调用链是否真实接通？".to_string(),
                status: "open".to_string(),
                evidence_refs: Vec::new(),
                waived_reason: None,
            }],
        };
        let blockers = finish_blockers(&[], Some(&open_contract), false, 0, None);
        assert_eq!(
            blockers,
            vec!["审查义务 implementation：实现调用链是否真实接通？"]
        );
        assert!(should_challenge_finish_with_blockers(
            true, true, false, &blockers
        ));

        let closed_contract = ReviewContract {
            scope_statement: "仅解释完整 diff".to_string(),
            obligations: vec![crate::agent::review_contract::ReviewObligation {
                id: "diff".to_string(),
                question: "变更意图是否明确？".to_string(),
                status: "evidenced".to_string(),
                evidence_refs: vec!["git diff --stat".to_string()],
                waived_reason: None,
            }],
        };
        assert!(should_challenge_finish_with_blockers(
            true,
            true,
            false,
            &finish_blockers(&[], Some(&closed_contract), true, 0, None),
        ));
        assert!(!should_challenge_finish_with_blockers(
            true,
            true,
            false,
            &finish_blockers(&[], Some(&closed_contract), false, 0, None),
        ));
    }
    #[test]
    fn change_verification_only_blocks_unclosed_real_writes() {
        assert!(finish_blockers(&[], None, false, 0, None).is_empty());

        let blockers = finish_blockers(&[], None, false, 3, None);
        assert_eq!(blockers.len(), 1);
        assert!(blockers[0].contains("尚无验证证据或明确豁免"));
        assert!(should_challenge_finish_with_blockers(
            true, true, false, &blockers
        ));

        let verified = ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec!["output_id: check-42".to_string()],
            waived_reason: None,
        };
        assert!(finish_blockers(&[], None, false, 3, Some(&verified)).is_empty());

        let waived = ChangeVerification {
            status: "waived".to_string(),
            evidence_refs: Vec::new(),
            waived_reason: Some("当前环境缺少集成服务".to_string()),
        };
        assert!(finish_blockers(&[], None, false, 3, Some(&waived)).is_empty());
    }

    #[test]
    fn change_verification_update_is_independent_of_review_contract_update() {
        let mut context = context::SandwichContext::new("system".to_string(), "goal".to_string());
        let expected = ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec!["output_id: test-42".to_string()],
            waived_reason: None,
        };
        let instruction = AgentInstruction {
            tool: Some("read_text_file".to_string()),
            change_verification_update: Some(expected.clone()),
            ..Default::default()
        };

        let review_started = apply_instruction_state_updates(&mut context, &instruction);

        assert!(!review_started);
        assert!(context.review_contract.is_none());
        assert_eq!(context.change_verification, Some(expected));
    }

    #[test]
    fn mode_switch_from_auto_to_plan_approval_resets_approved_state() {
        let context = context::SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.plan_mode = crate::agent::checkpoint::PlanExecutionMode::Auto;
        checkpoint.auto_pilot = true;
        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
        checkpoint.approved_plan_summary = None;

        checkpoint.reconcile_plan_phase_for_mode_switch(crate::agent::checkpoint::PlanExecutionMode::PlanApproval);
        checkpoint.plan_mode = crate::agent::checkpoint::PlanExecutionMode::PlanApproval;
        checkpoint.auto_pilot = false;

        assert_eq!(checkpoint.plan_phase, crate::agent::checkpoint::PlanPhase::Reviewing);
    }

    #[test]
    fn mode_switch_preserves_human_approved_plan() {
        let context = context::SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.plan_mode = crate::agent::checkpoint::PlanExecutionMode::Auto;
        checkpoint.auto_pilot = true;
        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
        // 人工批准会留下方案摘要痕迹
        checkpoint.approved_plan_summary = Some("用户已批准：调整拖拽遮罩逻辑".to_string());

        checkpoint.reconcile_plan_phase_for_mode_switch(crate::agent::checkpoint::PlanExecutionMode::PlanApproval);
        checkpoint.plan_mode = crate::agent::checkpoint::PlanExecutionMode::PlanApproval;
        checkpoint.auto_pilot = false;

        // 人工确认过的方案不应因模式切换被撤销，避免用户被迫二次审批
        assert!(checkpoint.plan_phase.is_approved());
    }

    #[test]
    fn mode_switch_to_auto_pilot_keeps_phase_untouched() {
        let context = context::SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.plan_mode = crate::agent::checkpoint::PlanExecutionMode::PlanApproval;
        checkpoint.auto_pilot = false;
        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Reviewing;

        // 反向切换（审批 -> 全自动）不应改动阶段，交由全自动链路自行放行
        checkpoint.reconcile_plan_phase_for_mode_switch(crate::agent::checkpoint::PlanExecutionMode::Auto);
        checkpoint.plan_mode = crate::agent::checkpoint::PlanExecutionMode::Auto;
        checkpoint.auto_pilot = true;

        assert_eq!(checkpoint.plan_phase, crate::agent::checkpoint::PlanPhase::Reviewing);
    }

    #[test]
    fn mode_switch_from_auto_to_cross_review_resets_unconsumed_approved_state() {
        let context = context::SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.plan_mode = crate::agent::checkpoint::PlanExecutionMode::Auto;
        checkpoint.auto_pilot = true;
        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
        checkpoint.approved_plan_summary = None;
        checkpoint.plan_reviewer_consumed = false;

        // 从全自动切到交叉审查，如果此前并未消费过审查结论，重置为 Reviewing
        checkpoint.reconcile_plan_phase_for_mode_switch(crate::agent::checkpoint::PlanExecutionMode::CrossReview);
        checkpoint.plan_mode = crate::agent::checkpoint::PlanExecutionMode::CrossReview;

        assert_eq!(checkpoint.plan_phase, crate::agent::checkpoint::PlanPhase::Reviewing);
    }

    #[test]
    fn mode_switch_preserves_cross_reviewed_state_when_consumed() {
        let context = context::SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.plan_mode = crate::agent::checkpoint::PlanExecutionMode::CrossReview;
        checkpoint.auto_pilot = true;
        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
        checkpoint.approved_plan_summary = None;
        checkpoint.plan_reviewer_consumed = true;

        // 已消费过审查结论的 Approved 状态在交叉审查模式下继续保持
        checkpoint.reconcile_plan_phase_for_mode_switch(crate::agent::checkpoint::PlanExecutionMode::CrossReview);

        assert!(checkpoint.plan_phase.is_approved());
    }

    #[test]
    fn cross_review_gatekeeper_enforces_dispatch_then_consume_then_approve() {
        let temp_dir = std::env::temp_dir().join(format!("gatekeeper-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let conn = crate::db::init_db(&temp_dir.join("test.db")).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        let context = context::SandwichContext::new("system".to_string(), "修复拖拽穿透".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.plan_mode = crate::agent::checkpoint::PlanExecutionMode::CrossReview;
        checkpoint.auto_pilot = true;
        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Exploring;
        checkpoint.plan_reviewer_retry_count = 0;
        checkpoint.plan_reviewer_consumed = false;

        // 阶段 1: 尚未派发 reviewer，尝试写代码 -> 拦截 need_reviewer_dispatch
        let rej1 = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint,
            &context,
            &db,
            "test_run",
            1,
            false,
        );
        assert!(rej1.is_some());
        assert_eq!(rej1.unwrap().route, "plan_gatekeeper:need_reviewer_dispatch");
        assert!(!checkpoint.plan_phase.is_approved());

        // 阶段 2: 派发了 reviewer 且子任务正在后台推演 -> 拦截 need_wait_reviewer_jobs
        checkpoint.plan_reviewer_retry_count = 1;
        checkpoint.plan_reviewer_consumed = false;
        insert_child_run(&db, "child-running", "test_run", "running", Some("{}"), None);
        let rej2 = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint,
            &context,
            &db,
            "test_run",
            2,
            false,
        );
        assert!(rej2.is_some());
        assert_eq!(rej2.unwrap().route, "plan_gatekeeper:need_wait_reviewer_jobs");
        assert!(!checkpoint.plan_phase.is_approved());

        // 阶段 2b: 子任务已终态但结论尚未回收 -> 同样拦截 need_wait_reviewer_jobs（wait_jobs 可收取）
        mark_child_run(&db, "child-running", "succeeded");
        let rej2b = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint,
            &context,
            &db,
            "test_run",
            2,
            false,
        );
        assert!(rej2b.is_some());
        assert_eq!(rej2b.unwrap().route, "plan_gatekeeper:need_wait_reviewer_jobs");

        // 阶段 2c: 派发失败场景——后台既无运行中任务、也无待回收结论 ->
        // 必须引导模型重新派发（reviewer_result_missing），否则只会空等 wait_jobs 而永久写不了
        delete_child_runs(&db, "test_run");
        let rej2c = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint,
            &context,
            &db,
            "test_run",
            2,
            false,
        );
        assert!(rej2c.is_some());
        assert_eq!(rej2c.unwrap().route, "plan_gatekeeper:reviewer_result_missing");
        assert!(!checkpoint.plan_phase.is_approved());

        // 阶段 3: 审查结论已消费并注入上下文 -> 门禁放行并将 plan_phase 推进为 Approved
        checkpoint.plan_reviewer_consumed = true;
        let rej3 = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint,
            &context,
            &db,
            "test_run",
            3,
            false,
        );
        assert!(rej3.is_none());
        assert!(checkpoint.plan_phase.is_approved());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn failed_reviewer_dispatch_rolls_back_review_budget() {
        let context = context::SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.plan_mode = crate::agent::checkpoint::PlanExecutionMode::CrossReview;
        checkpoint.plan_reviewer_retry_count = 0;
        checkpoint.plan_reviewer_consumed = true;

        // 模拟一次「派发前消费过结论」的派发：预扣 1 轮
        let pre_consumed = checkpoint.plan_reviewer_consumed;
        checkpoint.plan_reviewer_retry_count += 1;
        checkpoint.plan_reviewer_consumed = false;

        // 派发全部失败 -> 回滚预算，模型可以重新派发，不会耗尽 2 轮额度
        checkpoint.rollback_plan_review_dispatch(pre_consumed);

        assert_eq!(checkpoint.plan_reviewer_retry_count, 0);
        assert!(checkpoint.plan_reviewer_consumed);
    }

    #[test]
    fn rollback_never_underflows_review_budget() {
        let context = context::SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.plan_reviewer_retry_count = 0;

        checkpoint.rollback_plan_review_dispatch(false);

        assert_eq!(checkpoint.plan_reviewer_retry_count, 0);
        assert!(!checkpoint.plan_reviewer_consumed);
    }

    #[test]
    fn plan_approval_mode_gatekeeper_blocks_until_approved() {
        let temp_dir = std::env::temp_dir().join(format!("gatekeeper-plan-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let conn = crate::db::init_db(&temp_dir.join("test.db")).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        let context = context::SandwichContext::new("system".to_string(), "修改重要逻辑".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.plan_mode = crate::agent::checkpoint::PlanExecutionMode::PlanApproval;
        checkpoint.auto_pilot = false;
        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Reviewing;
        checkpoint.plan_reviewer_retry_count = 2;
        checkpoint.plan_reviewer_consumed = true;

        let rej = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint,
            &context,
            &db,
            "test_run",
            3,
            false,
        );
        assert!(rej.is_some());
        assert_eq!(rej.unwrap().route, "plan_gatekeeper:need_human_approval");
        assert!(!checkpoint.plan_phase.is_approved());

        // 人类批准后
        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
        checkpoint.approved_plan_summary = Some("用户批准了方案".to_string());
        let rej_approved = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint,
            &context,
            &db,
            "test_run",
            4,
            false,
        );
        assert!(rej_approved.is_none());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn plan_approval_kind_accepts_frontend_aliases() {
        // 前端 normalizeHumanForm 支持 plan_approval / plan / approval 三种别名，
        // 后端必须同步识别，否则前端渲染审批卡片、用户批准后门禁仍关闭。
        for kind in ["plan_approval", "plan", "approval", "PLAN_APPROVAL", " Plan "] {
            let normalized = kind.trim().to_ascii_lowercase();
            assert!(
                normalized == "plan_approval" || normalized == "plan" || normalized == "approval",
                "别名 {} 未被识别",
                kind
            );
        }
        // 普通提问的 kind 绝不能被误判为方案审批
        for kind in ["choice", "confirm", "text", "planning_note"] {
            let normalized = kind.trim().to_ascii_lowercase();
            assert!(
                !(normalized == "plan_approval" || normalized == "plan" || normalized == "approval"),
                "普通提问 {} 被误判为方案审批",
                kind
            );
        }
    }

    #[test]
    fn non_write_step_distinguishes_read_vs_write_tool() {
        let read_inst = AgentInstruction {
            tool: Some("read_file".to_string()),
            ..Default::default()
        };
        let write_inst = AgentInstruction {
            tool: Some("write_file".to_string()),
            ..Default::default()
        };
        let child_writer_inst = AgentInstruction {
            tool: Some("child_writer".to_string()),
            ..Default::default()
        };

        assert!(!is_write_or_side_effect_tool(&read_inst));
        assert!(is_write_or_side_effect_tool(&write_inst));
        assert!(is_write_or_side_effect_tool(&child_writer_inst));
    }

    fn inst_with_tool(tool: &str) -> AgentInstruction {
        AgentInstruction {
            tool: Some(tool.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn namespaced_mcp_tools_are_classified_by_concrete_tool_name() {
        // MCP 工具常带 "服务名/具体工具" 前缀，分类必须基于剥离命名空间后的具体工具名
        assert!(is_write_or_side_effect_tool(&inst_with_tool("remote-hub/shell_exec")));
        assert!(is_write_or_side_effect_tool(&inst_with_tool("remote-hub/write_file")));
        assert!(!is_write_or_side_effect_tool(&inst_with_tool("remote-hub/read_file")));
        assert!(!is_write_or_side_effect_tool(&inst_with_tool("remote-hub/list_dir")));
    }

    #[test]
    fn write_verb_beats_readonly_prefix() {
        // 形如 get_shell_output 的"取结果"类工具本质是脚本执行，
        // 不能被 get_/list_ 只读前缀误判放行，否则门禁可被绕过。
        assert!(is_write_or_side_effect_tool(&inst_with_tool("get_shell_output")));
        assert!(is_write_or_side_effect_tool(&inst_with_tool("list_running_jobs")));
        // 常规只读工具仍必须判为只读
        assert!(!is_write_or_side_effect_tool(&inst_with_tool("get_file_content")));
        assert!(!is_write_or_side_effect_tool(&inst_with_tool("list_workspaces")));
        assert!(!is_write_or_side_effect_tool(&inst_with_tool("read_multiple_files")));
        assert!(!is_write_or_side_effect_tool(&inst_with_tool("search_files")));
        assert!(!is_write_or_side_effect_tool(&inst_with_tool("web_fetch")));
    }

    #[test]
    fn plan_decision_marker_is_authoritative_over_free_text() {
        use crate::agent::loop_runner::parse_plan_approval_decision;

        // 原始缺陷：用户在「补充执行要求」里写了"打回修改"却点的是批准，
        // 旧逻辑靠文本嗅探会误判为打回。结构化标记必须覆盖文本。
        let tricky = "【方案已批准】批准方案，请开始执行后续写入与测试步骤。\n\
                      【补充执行要求】: 注意别把之前打回修改的那段逻辑又改回来\n\
                      <!--plan-decision:{\"approved\":true}-->";
        assert_eq!(parse_plan_approval_decision(tricky), Some(true));

        let tricky_reject = "【方案打回修改】请重新审视：\n这里写了个「批准方案」的注释\n\
                             <!--plan-decision:{\"approved\":false}-->";
        assert_eq!(parse_plan_approval_decision(tricky_reject), Some(false));
    }

    #[test]
    fn plan_decision_falls_back_to_text_markers_without_structured_marker() {
        use crate::agent::loop_runner::parse_plan_approval_decision;

        // 非 UI 通道（CLI / 直接调用）没有结构化标记，仍需兼容旧的文本约定
        assert_eq!(
            parse_plan_approval_decision("【方案已批准】可以开始改了"),
            Some(true)
        );
        assert_eq!(
            parse_plan_approval_decision("【方案打回修改】换个库再推演一次"),
            Some(false)
        );
        assert_eq!(
            parse_plan_approval_decision("{\"approved\":false}"),
            Some(false)
        );
    }

    #[test]
    fn plan_decision_returns_none_for_ambiguous_reply() {
        use crate::agent::loop_runner::parse_plan_approval_decision;

        // 无法判定时返回 None，调用方应保持门禁关闭，绝不默认放行
        assert_eq!(parse_plan_approval_decision("我再想想"), None);
        assert_eq!(parse_plan_approval_decision(""), None);
    }

    #[test]
    fn strip_plan_decision_marker_cleans_persisted_summary() {
        use crate::agent::loop_runner::{strip_plan_decision_marker, parse_plan_approval_decision};

        let raw = "【方案已批准】开始执行\n<!--plan-decision:{\"approved\":true}-->";
        let cleaned = strip_plan_decision_marker(raw);
        assert_eq!(cleaned, "【方案已批准】开始执行");
        // 清洗不能破坏解析（解析在清洗之前进行）
        assert_eq!(parse_plan_approval_decision(raw), Some(true));
        // 无标记时原样返回
        assert_eq!(strip_plan_decision_marker("普通回复"), "普通回复");
    }
}

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
    )
    .await;

    run_guard.finish(&result);
    result
}

async fn run_agent_main_loop_in_workspace_inner(
    app: AppHandle,
    state: State<'_, DbState>,
    registry_state: State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    model_routing: crate::agent::router::ModelRoutingConfig,
    goal: String,
    auto_pilot: bool,
    final_session_id: String,
    log_context: Option<bool>,
    transient_llm_request_retries: u32,
    attachments: Option<Vec<crate::commands::chat::ParsedAttachmentResult>>,
    workspace_override: Option<std::path::PathBuf>,
    run_id: String,
    plan_mode: Option<String>,
) -> Result<(), String> {
    // 立即注册人类干预与流式中断接收器，确保任务启动第一毫秒即可被前端即时打断
    let (tx, mut rx) = mpsc::channel::<HumanControl>(5);
    let (interrupt_tx, mut interrupt_rx) = watch::channel(0u64);
    {
        let mut channels = intervention_channels().lock().await;
        channels.insert(final_session_id.clone(), tx);
    }
    stream_interrupt_channels()
        .lock()
        .await
        .insert(final_session_id.clone(), interrupt_tx);

    let mut run_trace_steps: Vec<serde_json::Value> =
        crate::agent::run_trace::load_run_steps(state.inner(), &run_id).unwrap_or_default();
    let mut run_machine = AgentRunMachine::new();
    advance_run_state(
        &app,
        &final_session_id,
        &mut run_machine,
        AgentRunEvent::Start,
    )?;
    if let Err(error) = crate::agent::run_trace::upsert_snapshot(
        state.inner(),
        &run_id,
        &final_session_id,
        &goal,
        "running",
        &run_trace_steps,
        None,
    ) {
        eprintln!("[run-trace] 无法创建运行轨迹: {error}");
    }
    let (project_workspace, project_resources) = if workspace_override.is_none() {
        let conn = state.0.lock().map_err(|error| error.to_string())?;
        let ws = crate::commands::projects::project_workspace_for_session(&conn, &final_session_id)?;
        let res = if ws.is_some() {
            crate::commands::projects::project_resources_for_session(&conn, &final_session_id).unwrap_or_default()
        } else {
            Vec::new()
        };
        (ws, res)
    } else {
        (None, Vec::new())
    };
    let is_project_session = project_workspace.is_some();
    let has_workspace_override = workspace_override.is_some();
    let is_explicit_workspace = is_project_session || has_workspace_override;
    // Skill、插件与 shell 工具共享本轮运行时工作目录；恢复时会重新读取并按哈希复用未变更的 Skill。
    let workspace_dir = workspace_override
        .or(project_workspace)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    if !workspace_dir.is_dir() {
        return Err(format!("Agent 工作区不可用：{}", workspace_dir.display()));
    }
    // 运行起始时间：成功完成时自动登记运行期间新生成的工作区产物（微信回推文件链路）
    let run_started_at = chrono::Utc::now();
    // 捕获任务起点 Git 基准快照（用于 finish 时进行精准增量 Diff 审查）
    let git_baseline = crate::agent::reviewer_gatekeeper::GitBaseline::capture(&workspace_dir);
    // 项目会话进入前先拿工作区软锁（独占该目录直到本次 run 结束）
    let _workspace_run_lock = if is_project_session {
        Some(acquire_workspace_lock(&workspace_dir, &goal)?)
    } else {
        None
    };
    // P5 副本生命周期：父 run 终态时自动统一回收所有子 writer 的隔离副本
    let _child_workspaces_guard = ChildWorkspaceGuard {
        db: state.inner(),
        run_id: &run_id,
    };
    // 1. 智能 MCP 插件同步：首次任务全量初始化，后续任务仅增量保活（见 mcp/session_bootstrap.rs）
    crate::agent::mcp::session_bootstrap::sync_mcp_plugins_for_session(
        &app,
        state.inner(),
        registry_state.inner(),
        &workspace_dir,
        &final_session_id,
    )
    .await?;

    // 2. 初始化三明治上下文：加载系统提示词 + 可视化/HTML沙箱 Skill + 动态注入 MCP 工具
    let _ = app.emit(
        &format!("agent-progress-{}", final_session_id),
        json!({
            "type": "action_phase",
            "phase": "skills",
            "description": "📚 正在装配系统提示词与 Skill 缓存..."
        }),
    );
    let base_prompt = crate::agent::prompt_assembly::load_base_prompt(&app);

    // 2. 稳定的内置工具契约首轮常驻；长范例与外部 MCP schema 仍按需加载。
    let local_tools_menu = crate::agent::prompt_assembly::build_local_tools_menu();
    // 角色池清单：主 Agent 由此知道池子里有哪些专精角色可派发（读取失败不阻断运行）
    let role_cards_menu = crate::agent::tools::role_manager::build_role_cards_menu(state.inner())
        .unwrap_or_default();

    // 3. 从当前工作区向上读取 AGENTS.md / HUB.md；未显式选定项目/工作区时保持为空。
    let workspace_skill_context = if is_explicit_workspace {
        crate::agent::skill_context::workspace_skill_context(&workspace_dir)
    } else {
        String::new()
    };
    // 4. 用户在能力管理页维护的条目只作为补充上下文；读取失败不能阻断正常 Agent 运行。
    let custom_instruction_context = crate::commands::load_enabled_custom_instruction_context(
        state.inner(),
    )
    .unwrap_or_else(|error| {
        eprintln!("加载自定义提示词与 Skill 失败: {}", error);
        String::new()
    });
    let conversation_model_id = if !model_routing.pro.trim().is_empty() {
        model_routing.pro.clone()
    } else if !model_routing.flash.trim().is_empty() {
        model_routing.flash.clone()
    } else {
        model_routing.vision.clone()
    };
    let model_cfg_res = crate::llm::ModelConfig::from_db(&state, &conversation_model_id);
    let maybe_model_cfg = model_cfg_res.as_ref().ok();

    // 5. 查询所有 MCP Server 的工具列表并提前预热/升级当前会话的智能能力卡
    let (mcp_tools_menu, compact_tools_index) = {
        let registry = registry_state.lock().await;
        registry
            .prime_tool_catalog_for_session(&final_session_id, maybe_model_cfg)
            .await;
        registry.prewarm_all_for_session(&final_session_id);

        let mut mcp_lines = Vec::new();
        for (p_name, p_client) in &registry.clients {
            let tools = crate::agent::mcp_catalog::cached_tools(&p_client.config)
                .unwrap_or_else(|| p_client.last_known_tools());
            let names = tools
                .iter()
                .map(|t| format!("{}/{}", p_name, t.name))
                .collect::<Vec<_>>();
            if !names.is_empty() {
                mcp_lines.push(format!("  * {}: {}", p_name, names.join(", ")));
            }
        }
        let mcp_part = if mcp_lines.is_empty() {
            "  * （当前未连接任何外部 MCP 插件）".to_string()
        } else {
            mcp_lines.join("\n")
        };
        let builtin_list = crate::agent::prompt_assembly::build_builtin_tools_compact_list();
        let compact = format!(
            "- 原生内置: {}\n- MCP 插件:\n{}",
            builtin_list,
            mcp_part
        );
        (registry.format_tools_menu(), compact)
    };

    // 6. 扫描 prompts/skills/ 目录，生成动态 Skill 目录摘要（不含已固定编译的内置 Skill）
    let skill_catalog = {
        let resource_dir = app.path().resource_dir().unwrap_or_default();
        let skills_dir_prod = resource_dir.join("prompts/skills");
        let skills_dir_dev = std::path::PathBuf::from("prompts/skills");
        let skills_dir = if skills_dir_prod.exists() {
            skills_dir_prod
        } else {
            skills_dir_dev
        };
        // 全量动态扫描 prompts/skills/ 目录下所有 Skill 手册并提取/升级智能能力卡
        crate::agent::skill_context::skill_catalog_string(&skills_dir, &[], maybe_model_cfg)
    };

    let project_resource_boundary = if is_project_session {
        "\n\n## 项目工作区与受管资源\n当前会话已绑定专属项目工作区。你可以在该项目工作区内正常使用 search、外部 MCP 工具以及各类子工兵进行工程分析与开发。同时可调用 `project_resources.list_resources()` 获取重点受管文件，或在生成重要项目产物时调用 `project_resources.register_resource(path, label)` 登记为跨会话共享受管资源。"
    } else {
        ""
    };
    let system_prompt = format!(
        "{}{}{}{}{}\n{}\n{}{}",
        base_prompt,
        project_resource_boundary,
        workspace_skill_context,
        custom_instruction_context,
        skill_catalog,
        local_tools_menu,
        mcp_tools_menu,
        role_cards_menu
    );
    println!(
        "--- 最终系统提示词 (级联菜单模式) ---\n{}\n------------------",
        system_prompt
    );
    // 每轮都是独立 Agent 运行，但同一 session_id 始终归属同一个长期对话。
    // 崩溃恢复会沿用既有 checkpoint，不能重复追加用户消息；只有新轮次才写入消息历史。
    let existing_checkpoint = load_checkpoint(state.inner(), &final_session_id)?;
    let is_resuming_existing =
        !crate::agent::conversation::should_start_new_turn(existing_checkpoint.as_ref());
    if !is_resuming_existing {
        crate::agent::conversation::begin_turn(
            state.inner(),
            &final_session_id,
            &conversation_model_id,
            &goal,
        )?;
    }
    let mut context = context::SandwichContext::new(system_prompt, goal.clone());
    if is_project_session {
        context.resource_scope =
            context::resource_scope::ResourceScopeContract::bind_primary_filesystem(
                &workspace_dir.to_string_lossy(),
                context::resource_scope::ResourceScopeSource::SessionSelected,
            );

        // 动态受管文件清单挂载在底层面包（M_Tail），绝不污染 M0 顶层长前缀缓存
        let mut manifest_text = format!(
            "【项目受管文件清单 (Project Resource Manifest)】\n- 项目物理工作目录: `{}`\n",
            workspace_dir.display()
        );
        if project_resources.is_empty() {
            manifest_text.push_str("- 当前项目暂无登记的受管文件。当在工作区生成重要产物或分析报告时，可调用 `project_resources.register_resource(path, label)` 登记为跨会话共享资源。");
        } else {
            manifest_text.push_str(&format!("- 本项目已登记受管数据文件（共 {} 个，已建立资源句柄可直接读取分析）：\n", project_resources.len()));
            for r in &project_resources {
                let size_kb = (r.size_bytes as f64) / 1024.0;
                manifest_text.push_str(&format!(
                    "  - [{}] `{}` (resource_id: `{}`, 大小: {:.1} KB, 标签: \"{}\")\n",
                    r.kind.to_uppercase(),
                    r.relative_path,
                    r.resource_id,
                    size_kb,
                    r.label
                ));
            }
            manifest_text.push_str("- 提示：调用 `project_resources.read_resource(resource_id)` 可直接读取上述文件内容；在工作区发现或生成新核心交付物时可调用 `project_resources.register_resource(path, label)` 登记为跨会话共享资源。");
        }
        context.project_resource_manifest = Some(manifest_text);
    }
    context.compact_tools_index = Some(compact_tools_index);
    // 注入当前路由模型的上下文窗口大小与压缩配置，用于历史压缩水位动态计算与双层智能归档。
    // 双模型路由下同一段历史会分别发给推理与执行模型，预算必须按两者窗口的
    // 较小者计算，否则执行模型会先于推理模型触碰爆窗边界。
    if let Ok(cfg) = &model_cfg_res {
        let mut effective_window = cfg.context_window as usize;
        if !model_routing.flash.trim().is_empty() {
            if let Ok(flash_cfg) = crate::llm::ModelConfig::from_db(&state, &model_routing.flash) {
                effective_window = effective_window.min(flash_cfg.context_window as usize);
            }
        }
        context.context_window_tokens = effective_window;
        context.compaction_model_config = Some(cfg.clone());
    } else {
        context.context_window_tokens = 32_768;
    }
    context.budget_config = match model_routing.context_budget_mode.as_str() {
        "cost_saving" => crate::agent::context::budget::ContextBudgetConfig::cost_saving(),
        "ultra_fast" => crate::agent::context::budget::ContextBudgetConfig::ultra_fast(),
        _ => crate::agent::context::budget::ContextBudgetConfig::deep_coding(),
    };
    context.turns_history =
        crate::agent::conversation::recent_history(state.inner(), &final_session_id)?;
    // 审查任务在首次模型调用前注入轻量项目地图。它只包含目录、入口和 Git 元数据，
    // 不读取源码正文；恢复中的运行沿用 checkpoint，避免反复追加同一张地图。
    if !is_resuming_existing && is_explicit_workspace {
        if let Some(review_map) = crate::agent::project_map::review_bootstrap(&goal, &workspace_dir)
        {
            context.turns_history.push(context::types::ChatMessage {
                role: "user".to_string(),
                content: json!(review_map),
            });
        }
    }
    context.sync_fetched_tools_from_history();
    // checkpoint 是恢复的权威来源：只要上一轮任务尚未到终态 (Done/Failed)，一律无缝续接！
    let mut checkpoint = match existing_checkpoint {
        Some(existing) if !existing.phase.is_terminal() => {
            existing.restore_context(&mut context);
            let mut cp = existing;
            if cp.phase == AgentPhase::WaitUser {
                if !goal.trim().is_empty() && cp.data.user_goal != goal {
                    context.add_error_feedback(&format!("【人类回复】{}", goal));
                }
                // 保持原本的任务主目标不变
                context.user_goal = cp.data.user_goal.clone();
                cp.begin_planning(cp.step_id, &context);
            }
            cp
        }
        Some(existing) => {
            // 同一 session 上一轮已 finish/终态后，新用户消息仍继承脱水夹心与状态索引（Claude/Codex 会话内前缀追加）。
            existing.data.inherit_filling_into(&mut context);
            let mut fresh = AgentCheckpoint::new(final_session_id.clone(), &context);
            fresh.version = existing.version;
            fresh
        }
        None => AgentCheckpoint::new(final_session_id.clone(), &context),
    };
    // 模式切换守卫：按目标执行模式（全自动/交叉审查/方案审批）规范化方案阶段；人工确认过的方案永久保留。
    let target_plan_mode = crate::agent::checkpoint::PlanExecutionMode::from_str_or_auto_pilot(
        plan_mode.as_deref(),
        auto_pilot,
    );
    checkpoint.reconcile_plan_phase_for_mode_switch(target_plan_mode);
    checkpoint.plan_mode = target_plan_mode;
    checkpoint.auto_pilot = matches!(
        target_plan_mode,
        crate::agent::checkpoint::PlanExecutionMode::Auto
            | crate::agent::checkpoint::PlanExecutionMode::CrossReview
    );
    // 初始工作区是会话选择的已声明资源；恢复时保留 checkpoint 中已有的显式契约。
    // 仅在显式项目会话、存在 workspace_override 或 auto_pilot 模式下才声明主工作区。
    // 在未选项目的通用自由会话中，主工作区保持为 None（自由模式），避免将宿主机启动目录误作为资源约束强塞给 Agent 及外部 MCP。
    if (is_explicit_workspace || auto_pilot)
        && context.resource_scope.primary_filesystem().is_none()
    {
        let validation = if workspace_dir.is_dir() {
            context::resource_scope::ResourceScopeValidation::Valid
        } else {
            context::resource_scope::ResourceScopeValidation::Invalid
        };
        let source = if auto_pilot {
            context::resource_scope::ResourceScopeSource::EvaluationConfig
        } else {
            context::resource_scope::ResourceScopeSource::SessionSelected
        };
        context.resource_scope.bind_filesystem_with_validation(
            context::resource_scope::ResourceScopeRole::Primary,
            &workspace_dir.to_string_lossy(),
            source,
            validation,
        );
    }

    // 🌟 注入本轮用户传入的附件 (图片 Base64 多模态 / 解析文档文本)
    let mut injected_image_paths = std::collections::HashSet::new();

    // 1. 处理显式传递的 attachments 附件列表
    if let Some(ref atts) = attachments {
        for att in atts {
            let is_image = att.content_type.starts_with("image/")
                || att.file_name.ends_with(".jpg")
                || att.file_name.ends_with(".jpeg")
                || att.file_name.ends_with(".png")
                || att.file_name.ends_with(".webp")
                || att.file_name.ends_with(".gif")
                || att.file_name.ends_with(".bmp")
                || att.file_path.ends_with(".jpg")
                || att.file_path.ends_with(".jpeg")
                || att.file_path.ends_with(".png")
                || att.file_path.ends_with(".webp");

            if is_image {
                if let Some(ref data_url) = att.image_data_url {
                    if !data_url.is_empty() {
                        context.add_image_feedback(
                            &format!("【用户已附加图片: `{}`】(文件路径: {}, 大小: {} 字节)", att.file_name, att.file_path, att.size_bytes),
                            data_url,
                        );
                        injected_image_paths.insert(att.file_path.clone());
                        continue;
                    }
                }
                if let Ok(bytes) = std::fs::read(&att.file_path) {
                    use base64::Engine;
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    let mime = if att.content_type.is_empty() || att.content_type == "file" {
                        if att.file_name.ends_with(".png") || att.file_path.ends_with(".png") {
                            "image/png"
                        } else if att.file_name.ends_with(".webp") || att.file_path.ends_with(".webp") {
                            "image/webp"
                        } else {
                            "image/jpeg"
                        }
                    } else {
                        &att.content_type
                    };
                    let data_url = format!("data:{};base64,{}", mime, b64);
                    context.add_image_feedback(
                        &format!("【用户已附加图片: `{}`】(文件路径: {}, 大小: {} 字节)", att.file_name, att.file_path, att.size_bytes),
                        &data_url,
                    );
                    injected_image_paths.insert(att.file_path.clone());
                }
            } else if !att.prompt_text.is_empty() {
                context.turns_history.push(crate::agent::context::ChatMessage {
                    role: "user".to_string(),
                    content: serde_json::json!(att.prompt_text),
                });
            }
        }
    }

    // 2. 兜底扫描：若 goal 文本中带有 📎 [附件: ... | 路径: ...] 标记，自动读取本地图片或文档解析注入
    if goal.contains("📎") || goal.contains("路径:") {
        for segment in goal.split("📎") {
            if let Some(att_start) = segment.find("[附件:") {
                let rest = &segment[att_start + "[附件:".len()..];
                if let Some(end_bracket) = rest.find(']') {
                    let att_body = &rest[..end_bracket];
                    let mut file_name = "file";
                    let mut file_path = "";
                    if let Some((name_part, path_part)) = att_body.split_once("路径:") {
                        file_path = path_part.trim();
                        let name_candidate = name_part.trim();
                        if let Some((name_only, _)) = name_candidate.split_once('(') {
                            file_name = name_only.trim();
                        } else if let Some((name_only, _)) = name_candidate.split_once('|') {
                            file_name = name_only.trim();
                        } else {
                            file_name = name_candidate;
                        }
                    }
                    if !file_path.is_empty() && !injected_image_paths.contains(file_path) {
                        let p = std::path::Path::new(file_path);
                        if p.exists() {
                            let is_image = file_name.ends_with(".jpg")
                                || file_name.ends_with(".jpeg")
                                || file_name.ends_with(".png")
                                || file_name.ends_with(".webp")
                                || file_name.ends_with(".gif")
                                || file_name.ends_with(".bmp")
                                || file_path.ends_with(".jpg")
                                || file_path.ends_with(".jpeg")
                                || file_path.ends_with(".png")
                                || file_path.ends_with(".webp");

                            if is_image {
                                if let Ok(bytes) = std::fs::read(p) {
                                    use base64::Engine;
                                    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                                    let mime = if file_name.ends_with(".png") || file_path.ends_with(".png") {
                                        "image/png"
                                    } else if file_name.ends_with(".webp") || file_path.ends_with(".webp") {
                                        "image/webp"
                                    } else {
                                        "image/jpeg"
                                    };
                                    let data_url = format!("data:{};base64,{}", mime, b64);
                                    context.add_image_feedback(
                                        &format!("【用户已附加图片: `{}`】(文件路径: {}, 大小: {} 字节)", file_name, file_path, bytes.len()),
                                        &data_url,
                                    );
                                    injected_image_paths.insert(file_path.to_string());
                                }
                            } else if let Ok(extracted) = crate::agent::file_parser::parse_file(p, None) {
                                let prompt_text = crate::agent::file_parser::format_for_prompt(&extracted, file_name);
                                context.turns_history.push(crate::agent::context::ChatMessage {
                                    role: "user".to_string(),
                                    content: serde_json::json!(prompt_text),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    // 数据区本体是权威来源；每次启动都刷新目录，checkpoint 仅保留这份目录快照。
    context.set_task_resources(task_store::list_resource_refs(
        state.inner(),
        &final_session_id,
    )?);
    checkpoint.refresh_context(&context);
    save_checkpoint(state.inner(), &mut checkpoint)?;
    // 仅在从 phase=RunTool 且尚无 pending 的 checkpoint 恢复时使用。
    let mut recovered_instruction: Option<AgentInstruction> = None;
    let mut action_history: std::collections::VecDeque<String> = std::collections::VecDeque::new();

    advance_run_state(
        &app,
        &final_session_id,
        &mut run_machine,
        AgentRunEvent::RuntimeReady,
    )?;
    // WaitUser 与 pending 调用不能从普通上下文可靠反推；恢复时显式读取 checkpoint。
    match recovery_decision(&checkpoint) {
        RecoveryDecision::ResumePlanning | RecoveryDecision::Terminal => {}
        RecoveryDecision::ResumeTool => {
            recovered_instruction = checkpoint.prepared_instruction.clone();
            if recovered_instruction.is_none() {
                return Err("checkpoint phase=run_tool 但缺少 prepared instruction".to_string());
            }
        }
        RecoveryDecision::ResumeWaiting
        | RecoveryDecision::RequireHumanReview
        | RecoveryDecision::RetryPending
        | RecoveryDecision::ProbePending => {
            let reason = match checkpoint.pending.as_ref() {
                Some(pending) => format!(
                    "系统从 checkpoint 恢复：工具 {} / 动作 {} 的结果不确定（call_id: {}，已尝试 {} 次）。为避免重复副作用，请确认是否继续。",
                    pending.tool, pending.action, pending.call_id, pending.attempts
                ),
                None => checkpoint
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "系统恢复后正在等待人类确认。".to_string()),
            };

            checkpoint.wait_for_human(reason.clone(), &context);
            save_checkpoint(state.inner(), &mut checkpoint)?;
            advance_run_state(
                &app,
                &final_session_id,
                &mut run_machine,
                AgentRunEvent::InstructionReady {
                    action: "checkpoint_recovery".to_string(),
                },
            )?;
            advance_run_state(
                &app,
                &final_session_id,
                &mut run_machine,
                AgentRunEvent::WaitForHuman {
                    question: reason.clone(),
                },
            )?;
            app.emit(
                &format!("agent-progress-{}", final_session_id),
                json!({ "type": "agent_paused", "message": reason }),
            )
            .map_err(|error| error.to_string())?;

            let reply = match rx.recv().await {
                Some(HumanControl::Answer(reply)) => reply,
                Some(HumanControl::ReplaceGoal(next_goal)) => {
                    let reason = format!("用户提交了新的任务目标：{}", next_goal);
                    finish_interrupted_run(
                        &app,
                        state.inner(),
                        &final_session_id,
                        &run_id,
                        &goal,
                        &run_trace_steps,
                        &mut checkpoint,
                        &context,
                        &mut run_machine,
                        reason,
                    )
                    .await?;
                    return Ok(());
                }
                Some(HumanControl::Cancel) => {
                    finish_interrupted_run(
                        &app,
                        state.inner(),
                        &final_session_id,
                        &run_id,
                        &goal,
                        &run_trace_steps,
                        &mut checkpoint,
                        &context,
                        &mut run_machine,
                        "用户请求停止当前任务。".to_string(),
                    )
                    .await?;
                    return Ok(());
                }
                None => "人类未回复，通道异常关闭".to_string(),
            };
            context.add_error_feedback(&format!("【checkpoint 恢复后的人工确认】{}", reply));
            checkpoint.begin_planning(checkpoint.step_id, &context);
            save_checkpoint(state.inner(), &mut checkpoint)?;
            advance_run_state(
                &app,
                &final_session_id,
                &mut run_machine,
                AgentRunEvent::HumanReplyReceived,
            )?;
        }
    }

    // 🧹 会话启动时，自动收敛历史由于断电或异常退出的孤儿后台任务与子审查任务
    let _ = crate::agent::job_engine::recover_orphaned_jobs_on_session_start(state.inner(), &final_session_id);
    let _ = crate::agent::child_reviewer::recover_orphaned_child_runs_on_session_start(state.inner(), &final_session_id);

    // durable cursor 是调度权威：每个普通步骤提交后推进 step_id；恢复时也从这里继续。
    // 不再使用启动瞬间捕获范围的 for-loop，避免 checkpoint 更新后调度器仍沿旧游标运行。
    while checkpoint.step_id < MAX_STEPS {
        let step_id = checkpoint.step_id;

        // 🔄 异步子任务结果轮询（审查/工兵/侦察）：自动注入父 Agent 上下文
        if let Ok(completed) = crate::agent::child_reviewer::poll_completed_child_runs(state.inner(), &run_id) {
            if !completed.is_empty() {
                let mut injection = String::new();
                let git_available = crate::agent::child_writer::is_git_available(&workspace_dir).await;
                for result in completed {
                    match result.kind.as_deref() {
                        Some("writer") => {
                            let mut writer_res = result.raw_result_json.as_deref()
                                .and_then(|raw| serde_json::from_str::<crate::agent::child_writer::WriterResult>(raw).ok());
                            if let Some(ref mut wr) = writer_res {
                                // 🌟 核心：在父 Agent 消费回执的瞬间现算指纹，闭合 TOCTOU 竞争窗口！
                                crate::agent::child_writer::refresh_file_conflicts(&mut wr.changed_files, &workspace_dir, git_available).await;
                                injection.push_str(&format!(
                                    "\n【🛠️ 写入工兵已完成 (child_run_id: {})】\n状态: {}, 测试: {}\n",
                                    wr.child_run_id, wr.status, wr.test_status
                                ));
                                if !wr.test_output_tail.is_empty() {
                                    injection.push_str(&format!("测试输出摘要:\n{}\n", wr.test_output_tail));
                                }
                                injection.push_str("修改文件与冲突状态（主工作区实时现算）:\n");
                                for cf in &wr.changed_files {
                                    let status_label = cf.conflict_status.map(|s| s.label()).unwrap_or("未知");
                                    injection.push_str(&format!("- {} [{}]\n", cf.path, status_label));
                                    if !cf.diff.is_empty() {
                                        injection.push_str(&format!("```diff\n{}\n```\n", cf.diff));
                                    }
                                }
                                if let Some(ref pr) = wr.patch_ref {
                                    injection.push_str(&format!("完整补丁工件: 可用 artifact_manager(action: \"get\", id: \"{}\") 查看\n", pr));
                                }
                                if !wr.findings.is_empty() {
                                    injection.push_str(&format!("修改说明:\n- {}\n", wr.findings.join("\n- ")));
                                }
                            } else {
                                let formatted_findings = if result.findings.is_empty() {
                                    "（无具体发现）".to_string()
                                } else {
                                    result.findings.iter().map(|f| f.display_line()).collect::<Vec<_>>().join("\n- ")
                                };
                                injection.push_str(&format!(
                                    "\n【🛠️ 写入工兵已完成 (child_run_id: {})】\n结论: {}\n发现:\n- {}\n",
                                    result.child_run_id, result.confidence, formatted_findings
                                ));
                            }
                        }
                        Some("worker") => {
                            let formatted_findings = if result.findings.is_empty() {
                                "（无具体发现）".to_string()
                            } else {
                                result.findings.iter().map(|f| f.display_line()).collect::<Vec<_>>().join("\n- ")
                            };
                            injection.push_str(&format!(
                                "\n【🕵️ 只读侦察任务已完成 (child_run_id: {})】\n结论: {}\n发现:\n- {}\n",
                                result.child_run_id, result.confidence, formatted_findings
                            ));
                        }
                        _ => {
                            let formatted_findings = if result.findings.is_empty() {
                                "（无具体发现）".to_string()
                            } else {
                                result.findings.iter().map(|f| f.display_line()).collect::<Vec<_>>().join("\n- ")
                            };
                            let routing_str = result.routing.as_deref().unwrap_or("certain");
                            injection.push_str(&format!(
                                "\n【🔍 子审查代理已完成 (child_run_id: {})】\n结论: {}\n建议路由: [{}]\n发现:\n- {}\n",
                                result.child_run_id, result.confidence, routing_str, formatted_findings
                            ));
                            checkpoint.plan_reviewer_consumed = true;
                        }
                    }
                    if !result.missing_evidence.is_empty() {
                        injection.push_str(&format!("缺失证据/未完成项: {}\n", result.missing_evidence.join(", ")));
                    }
                    if let Some(ref err) = result.error_kind {
                        injection.push_str(&format!("异常类别: {}（子任务未正常完成，请谨慎对待输出）\n", err));
                    }
                }
                if !injection.is_empty() {
                    context.add_error_feedback(&injection);
                    let _ = app.emit(
                        &format!("agent-log-{}", final_session_id),
                        "🔄 子任务成果（含实时冲突检测）已注入父 Agent 上下文".to_string()
                    );
                }
            }
        }

        // ⚡ 通用后台任务信箱 (Durable Pending Inbox) 轮询：获取任意后台任务完成结果
        if let Ok(jobs) = crate::agent::job_engine::poll_pending_inbox(state.inner(), &run_id, 5) {
            if !jobs.is_empty() {
                let mut job_injection = String::new();
                for job in jobs {
                    if job.job_type == "child_reviewer" || job.job_type.contains("reviewer") {
                        checkpoint.plan_reviewer_consumed = true;
                    }
                    job_injection.push_str(&format!(
                        "\n【⚡ 后台异步任务已完成 (ID: {}, 类型: {})】\n任务描述: {}\n执行状态: {}\n耗时: {}ms\n输出/结论:\n{}\n",
                        job.job_id,
                        job.job_type,
                        job.title,
                        job.status,
                        job.duration_ms,
                        if job.result_json.is_empty() { "（无附加输出）" } else { &job.result_json }
                    ));
                    if let Some(ref err) = job.error_kind {
                        job_injection.push_str(&format!("异常类别: {}\n", err));
                    }
                }
                if !job_injection.is_empty() {
                    context.add_error_feedback(&job_injection);
                    let _ = app.emit(
                        &format!("agent-log-{}", final_session_id),
                        "⚡ 后台异步任务结果已注入父 Agent 上下文".to_string()
                    );
                }
            }
        }
        let resume_instruction = recovered_instruction.take();
        // 每轮规划前把方案门禁状态投影进上下文底部状态卡：模型在决策时即可见
        // 当前模式与放行条件，不再靠写入被拒（一整轮 LLM 往返）反推门禁规则。
        context.plan_gate = Some(checkpoint.plan_gate_projection());
        if resume_instruction.is_none() {
            checkpoint.begin_planning(step_id, &context);
            save_checkpoint(state.inner(), &mut checkpoint)?;
        }
        advance_run_state(
            &app,
            &final_session_id,
            &mut run_machine,
            AgentRunEvent::BeginPlanning { step_id },
        )?;

        // 宿主状态机：追踪当前目标块步数并触发硬顶防翻车警报
        let hard_limit_warning = if let Some(block) = context.active_target_block_mut() {
            block.on_step_start();
            if block.is_hard_limit_exceeded() {
                Some((block.id.clone(), block.steps_count, block.hard_step_limit))
            } else {
                None
            }
        } else {
            None
        };

        if let Some((id, count, limit)) = hard_limit_warning {
            context.add_error_feedback(&format!(
                "【⚠️ 目标块步数超限警报】当前活跃目标块 [{}] 已经执行了 {} 步（达到硬顶上限 {} 步）。\n请停止零散动作，检查当前方案是否受阻。若受阻请在 progress_summary 中指出并申请验收或进入收敛。",
                id, count, limit
            ));
        }

        app.emit(
            &format!("agent-log-{}", final_session_id),
            format!("正在规划第 {} 步...", step_id + 1),
        )
        .map_err(|e| e.to_string())?;

        // 安全边界一：尚未请求模型时直接收束旧运行。
        if let Some(reason) = take_pending_interrupt(&mut rx) {
            finish_interrupted_run(
                &app,
                state.inner(),
                &final_session_id,
                &run_id,
                &goal,
                &run_trace_steps,
                &mut checkpoint,
                &context,
                &mut run_machine,
                reason,
            )
            .await?;
            return Ok(());
        }

        // 修复：绝不可以在这里清空 current_observation，因为这会导致 AI 看不到刚获取的页面数据
        // ================================================================
        // 统一重试循环：整个"规划 + 执行"作为一个原子操作，失败就重试
        // ================================================================
        let outcome = match run_call_llm_and_run_tool_phases(
            &app,
            &state,
            &registry_state,
            &model_routing,
            &run_id,
            &workspace_dir,
            &goal,
            log_context,
            transient_llm_request_retries,
            &final_session_id,
            step_id,
            &mut context,
            &mut checkpoint,
            &mut run_machine,
            &mut rx,
            &mut interrupt_rx,
            resume_instruction,
            &mut run_trace_steps,
        )
        .await
        {
            Ok(res) => res,
            Err(error) => {
                let terminal_error = format!("任务执行失败: {}", error);
                advance_run_state(
                    &app,
                    &final_session_id,
                    &mut run_machine,
                    AgentRunEvent::Fail {
                        reason: terminal_error.clone(),
                    },
                )?;
                checkpoint.mark_failed(terminal_error.clone(), &context);
                save_checkpoint(state.inner(), &mut checkpoint)?;
                let failure_completion = json!({ "summary": terminal_error });
                let _ = crate::agent::run_trace::upsert_snapshot(
                    state.inner(),
                    &run_id,
                    &final_session_id,
                    &goal,
                    "failed",
                    &run_trace_steps,
                    Some(&failure_completion),
                );
                let _ = app.emit(
                    &format!("agent-progress-{}", final_session_id),
                    json!({
                        "type": "complete",
                        "message": terminal_error,
                        "success": false
                    }),
                );
                return Err(error);
            }
        };
        let (mut instruction, result) = (outcome.instruction, outcome.result);
        // 结构化路由信号：按真实 DispatchResult 记录成功/失败（替代对反馈文本 ❌ 的脆弱匹配）
        context.record_step_outcome(result.success);
        if result.route == RUN_INTERRUPTED_ROUTE {
            finish_interrupted_run(
                &app,
                state.inner(),
                &final_session_id,
                &run_id,
                &goal,
                &run_trace_steps,
                &mut checkpoint,
                &context,
                &mut run_machine,
                result.stdout,
            )
            .await?;
            return Ok(());
        }

        // ================================================================
        // 以下是成功跳出规划+执行重试循环后的处理逻辑
        // ================================================================

        // --- 网关层硬拦截：死循环检测 ---
        action_history.push_back(instruction.get_action().to_lowercase());
        if action_history.len() > 4 {
            action_history.pop_front();
        }

        // 连续 3 次 extract 才触发（放宽阈值，避免合理的"extract→等待→extract"被误判）
        let is_loop = if action_history.len() >= 3 {
            let len = action_history.len();
            action_history[len - 3] == "extract"
                && action_history[len - 2] == "extract"
                && action_history[len - 1] == "extract"
        } else {
            false
        };

        // extract → scroll → extract 这个死循环模式依然保留
        let is_loop_3 = if action_history.len() >= 3 {
            let len = action_history.len();
            let last_three = (
                &action_history[len - 3],
                &action_history[len - 2],
                &action_history[len - 1],
            );
            last_three.0 == "extract" && last_three.1 == "scroll" && last_three.2 == "extract"
        } else {
            false
        };

        if is_loop || is_loop_3 {
            let error_detail = "【🚨 网关强制阻断】: 系统检测到你陷入了认知死胡同（连续无意义的 extract 循环）！当前页面的目标元素大概率不存在，或者你理解错了页面状态！请立刻通过 `back` 返回上一页，或者更改搜索关键词重新 `goto`，严禁再次执行 extract！";
            context.add_error_feedback(error_detail);
            app.emit(
                &format!("agent-log-{}", final_session_id),
                "🚨 网关层拦截：发现违规连续 extract 操作，强制注入纠偏机制！",
            )
            .ok();
            action_history.clear(); // 阻断后清空历史，重新积累
        }

        // --- 更新任务规划 + 核心记忆 ---
        // 两类责任账本独立更新，避免审查契约的拒绝路径吞掉写入验证更新。
        let review_contract_started_this_turn =
            apply_instruction_state_updates(&mut context, &instruction);
        if !instruction.memories_update.is_empty() {
            let rejections = context.update_memories(instruction.memories_update.clone());
            if !rejections.is_empty() {
                context.turns_history.push(crate::agent::context::types::ChatMessage {
                    role: "user".to_string(),
                    content: serde_json::json!(format!(
                        "【⚠️ 记忆安全提示】\n以下记忆项未能存入核心记忆卡：\n{}\n（请勿在记忆中保存明文密钥或超长文本；长数据请存入 TaskStore 数据区）",
                        rejections.iter().map(|r| format!("- {}", r)).collect::<Vec<_>>().join("\n")
                    )),
                });
            }
            println!("📝 AI 更新了核心记忆: {:?}", instruction.memories_update);
        }
        if let Some(ref summary) = instruction.progress_summary {
            context.set_progress_summary(summary);
            println!("📝 AI 更新了进度摘要: {}", summary);
        }

        // --- 核心增强：历史截断与缓存优化 ---
        if instruction.clear_history.unwrap_or(false) {
            println!("🧹 AI 主动触发了历史清理 (clear_history: true)!");
            context.turns_history.clear();
        }

        // --- 核心增强：Token 优化与冷存储检索 ---
        // AI 在本轮请求 "require_memory: true" 或触发了清空历史，下一轮组装消息时就会塞入全部 Fact 内容
        context.carry_memories = instruction.require_memory.unwrap_or(false)
            || instruction.clear_history.unwrap_or(false);

        // --- 预加载下一轮工具说明书 ---
        let next_tool = instruction
            .next_tool_hint
            .clone()
            .unwrap_or_else(|| instruction.get_action());
        context.active_tool = Some(next_tool.clone());

        // --- 加载工具说明书（Write-Once 历史事件 + 未命中纠错 + 现场刷新，见 tool_manual.rs）---
        crate::agent::tool_manual::load_tool_manual_for_next_step(
            &app,
            registry_state.inner(),
            &mut context,
            &instruction,
        )
        .await;

        // --- 刷新独立任务数据区目录，再把轻量引用写进 checkpoint ---
        // 数据行早已由 task_store 自己实时持久化，绝不复制到 context。
        context.set_task_resources(task_store::list_resource_refs(
            state.inner(),
            &final_session_id,
        )?);

        // --- 记录历史 + 更新观测 ---
        let final_stdout = result.stdout.clone();
        let output_text = if result.success {
            final_stdout.clone()
        } else {
            format!("❌ {}\n{}", result.stderr, final_stdout)
        };

        let is_finish = is_finish_instruction(&instruction);
        let mut open_todos = finish_blockers(
            &context.todo_list,
            context.review_contract.as_ref(),
            review_contract_started_this_turn,
            context.side_effects.len() - context.inherited_side_effects_count,
            context.change_verification.as_ref(),
        );
        let complex_delivery_blocker =
            complex_delivery_blocker(&context, &instruction, is_finish, result.success);
        let complex_delivery_unresolved = complex_delivery_blocker.is_some();
        if let Some(blocker) = complex_delivery_blocker {
            open_todos.push(blocker);
        }
        if is_finish && (crate::agent::child_reviewer::has_running_child_runs(state.inner(), &run_id) || crate::agent::job_engine::has_running_jobs(state.inner(), &run_id)) {
            open_todos.push("仍有后台异步任务或持续监听正在执行中：后台任务请调用 wait_jobs 挂起等待并接收结论；持续监听（watch）请调用 watch_stop 显式释放后再提交 finish，或执行其他独立操作后重试".to_string());
        }
        let finish_challenged = should_challenge_finish_with_complex_delivery_blocker(
            is_finish,
            result.success,
            checkpoint.finish_reviewed,
            checkpoint.finish_challenge_count,
            &open_todos,
            complex_delivery_unresolved,
        );

        let mut reviewer_challenged = false;
        let mut reviewer_warnings_delivered: Option<crate::agent::reviewer_gatekeeper::ReviewResult> = None;

        if finish_challenged {
            checkpoint.finish_reviewed = true;
            checkpoint.finish_challenge_count = checkpoint.finish_challenge_count.saturating_add(1);
            let blocker_summary = open_todos
                .iter()
                .map(|todo| format!("- {todo}"))
                .collect::<Vec<_>>()
                .join("\n");
            context.add_error_feedback(&format!(
                "【🚨 终态拦截：闭合或证据披露未完成】检测到当前任务尚未完成闭合验证或复杂结论校准（Todo / 审查契约 / 写入后验证 / 主根因证据）：\n{}\n本次 finish 已被系统拦截，任务不会在此终止。请直接针对上述未闭合项进行验证与状态更新：\n- 若为写入操作，请执行测试验证并在 change_verification_update 中以 verified + evidence_refs 闭合；\n- 若某项不再适用，请在 review_contract_update 或 todo_update 中显式标为 waived / failed 并说明理由；\n- 状态更新后请再次提交 finish 交付。",
                blocker_summary
            ));
            crate::agent::dispatcher::emit_tool_event(
                &app,
                &final_session_id,
                step_id,
                &instruction,
                "finish_challenged",
                Some("finish".to_string()),
                Some(false),
                &format!(
                    "终态拦截：检测到 {} 项未闭合事项，请先行验证",
                    open_todos.len()
                ),
                None,
            );
            // 调优：finish 被质询 = 交付未达标，下一轮重写升维处理（规则模式直接升 Pro，
            // 打分模式累加连续失败权重）。实测依据：Flash 档反复守不住交付披露纪律。
            context.record_step_outcome(false);
        } else if is_finish && result.success {
            // Level 0 机械门禁：零 Token 快速拦截（Scope 边界越界等）
            let scope_violations = crate::agent::reviewer_gatekeeper::mechanical_precheck_violations(
                &workspace_dir,
                context.active_target_block(),
            );
            if !scope_violations.is_empty() {
                checkpoint.finish_reviewed = true;
                context.add_error_feedback(&format!(
                    "【🚨 机械门禁拦截：代码改动超出目标块预定边界】\n{}\n本次 finish 已被系统自动打回（0 Token 消耗，免除强模型审查）。请还原无关文件的意外改动，或在方案中扩展该块的 scope 后重试。",
                    scope_violations.join("\n")
                ));
                crate::agent::dispatcher::emit_tool_event(
                    &app,
                    &final_session_id,
                    step_id,
                    &instruction,
                    "scope_challenged",
                    Some("finish".to_string()),
                    Some(false),
                    &format!("机械拦截：改动超出目标块预定范围（{} 项越界）", scope_violations.len()),
                    None,
                );
                context.record_step_outcome(false);
            } else {
                // Level 1: 确定性门禁与机械门禁 100% 通过后，才调用 Reviewer 终审（带 Checklist 证据链严审）！
                let review_res = crate::agent::reviewer_gatekeeper::evaluate_finish_diff(
                    &app,
                    state.inner(),
                    &context,
                    &model_routing,
                    step_id,
                    &workspace_dir,
                    &git_baseline,
                    run_started_at,
                    &final_session_id,
                    &mut interrupt_rx,
                    checkpoint.last_reviewed_diff_hash.as_deref(),
                    checkpoint.last_review_result.as_ref(),
                )
                .await;

                if !review_res.is_passed() {
                // 存在阻塞性缺陷！
                if checkpoint.reviewer_retry_count < 3 {
                    checkpoint.reviewer_retry_count += 1;
                    checkpoint.last_reviewed_diff_hash = Some(review_res.diff_hash.clone());
                    checkpoint.last_review_result = Some(review_res.clone());
                    reviewer_challenged = true;

                    // 目标块审查熔断联动：超限自动升维 Pro
                    let breaker_alert = if let Some(block) = context.active_target_block_mut() {
                        let tripped = block.record_review_failure();
                        if tripped {
                            Some(format!("\n\n【🚨 审查熔断告警】当前目标块 [{}] 连续 3 次未能通过审查，已触发死循环熔断！系统已强制将该块升维至主力强模型（Pro）接管执行，请利用强推理能力重构并彻底解决上述缺陷。", block.id))
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    let issues_text = review_res
                        .blocking_issues
                        .iter()
                        .enumerate()
                        .map(|(i, b)| {
                            format!(
                                "{}. 【{}】{}\n   👉 修复要求: {}",
                                i + 1,
                                b.file,
                                b.issue,
                                b.requirement
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    let nits_text = if !review_res.nit_issues.is_empty() {
                        let nits = review_res
                            .nit_issues
                            .iter()
                            .map(|n| format!("- [{}] {}", n.file, n.suggestion))
                            .collect::<Vec<_>>()
                            .join("\n");
                        format!("\n\n💡 次要建议（供参考，不阻碍交付）：\n{}", nits)
                    } else {
                        String::new()
                    };

                    context.add_error_feedback(&format!(
                        "【🚨 终态代码审查未通过（第 {}/3 次评审）】\n审查模型（{}）评分: {}/100\n审查评价: {}\n\n检测到以下阻塞性缺陷（Blocking Issues）：\n{}{}{}\n\n本次 finish 已被系统拦截，任务不会在此终止。请直接针对上述缺陷进行修改并运行测试验证，修复完成后再次调用 finish 交付。",
                        checkpoint.reviewer_retry_count,
                        review_res.model_id,
                        review_res.score,
                        review_res.summary,
                        issues_text,
                        nits_text,
                        breaker_alert.as_deref().unwrap_or("")
                    ));

                    crate::agent::dispatcher::emit_tool_event(
                        &app,
                        &final_session_id,
                        step_id,
                        &instruction,
                        "finish_challenged",
                        Some("finish".to_string()),
                        Some(false),
                        &format!(
                            "终态审查未通过（{}分）：发现 {} 项阻塞性缺陷，已打回自愈",
                            review_res.score,
                            review_res.blocking_issues.len()
                        ),
                        None,
                    );
                    context.record_step_outcome(false);
                } else {
                    // 达到 3 次熔断上限！进入带警告交付模式（Fail-Open with Warnings / 升级人工）
                    reviewer_warnings_delivered = Some(review_res.clone());
                    crate::agent::dispatcher::emit_tool_event(
                        &app,
                        &final_session_id,
                        step_id,
                        &instruction,
                        "succeeded",
                        Some("finish".to_string()),
                        Some(true),
                        &format!(
                            "终态审查熔断放行（已重试 3 次）：仍有 {} 项未收敛缺陷已附带警告交付",
                            review_res.blocking_issues.len()
                        ),
                        None,
                    );
                }
            } else {
                // 审查 100% 通过（或无代码修改 / 仅次要建议放行）
                checkpoint.last_reviewed_diff_hash = None;
                checkpoint.last_review_result = None;
                // 宿主状态机：复位重试计数并推进当前目标块至 Completed，激活下一块
                if let Some(block) = context.active_target_block_mut() {
                    block.reset_review_retries();
                }
                context.block_manager.advance_to_next();
                crate::agent::dispatcher::emit_tool_event(
                    &app,
                    &final_session_id,
                    step_id,
                    &instruction,
                    "succeeded",
                    Some("finish".to_string()),
                    Some(true),
                    &output_text,
                    None,
                );
            }
            }
        } else if !is_finish {
            // 新的常规步骤提供了新增证据，之后若仍有 open Todo 可重新进行一次质询。
            checkpoint.finish_reviewed = false;
            // 只有当常规步骤产生了新的真实副作用写入时，才视为进入了新的交付周期，重置质询标记；
            // 纯只读或查询步骤不重置 finish_challenge_count，防止模型通过插步（如反复 read_file）逃避熔断导致死循环。
            if result.success && is_write_or_side_effect_tool(&instruction) {
                checkpoint.finish_challenge_count = 0;
            }
        }
        let is_effectively_challenged = finish_challenged || reviewer_challenged;
        let outcome_str = if is_effectively_challenged {
            "finish_challenged"
        } else if result.success {
            "succeeded"
        } else {
            "failed"
        };
        let output_id = crate::agent::run_trace::save_step_output_with_meta(
            state.inner(),
            &final_session_id,
            step_id,
            &instruction.get_tool(),
            &instruction.description,
            outcome_str,
            &output_text,
        )
        .unwrap_or_else(|e| {
            eprintln!("⚠️ 无法保存 step_output: {}", e);
            "unknown_output_id".to_string()
        });

        // 🌟 工具执行失败时，自动沉淀到「避坑教训库 (Failed Attempts)」，供后续步骤长效避坑
        if !result.success {
            let error_summary = if !result.stderr.trim().is_empty() {
                result.stderr.trim().to_string()
            } else {
                result.stdout.lines().take(3).collect::<Vec<_>>().join(" ")
            };
            let short_error = if error_summary.chars().count() > 160 {
                format!("{}...", error_summary.chars().take(160).collect::<String>())
            } else {
                error_summary
            };
            context.record_failed_attempt(
                step_id,
                instruction.get_tool(),
                instruction.get_action(),
                &instruction.get_params(),
                short_error,
                Some(output_id.clone()),
            );
        }

        if let Some(block) = context.active_target_block_mut() {
            block.on_step_finish();
        }

        context.add_step(&instruction, &output_text, &output_id);

        // ── 防虚假勾选：将当步真实 output_id 回填到标记为 done 但缺少证据的 Todo（需在 output_id 已知后执行）
        if !instruction.todo_update.is_empty() {
            for item in &mut instruction.todo_update {
                if item.status == "done" && item.evidence_output_id.is_none() {
                    item.evidence_output_id = Some(output_id.clone());
                }
            }
            // 证据关联后同步更新 todo_list
            context.todo_list = instruction.todo_update.clone();
        }

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
                        state.inner(),
                        &final_session_id,
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
                    state.inner(),
                    &final_session_id,
                    step_id,
                    applied,
                    result.success,
                );
            }
        }
        // ──────────────────────────────────────────────────────────────────

        // --- 记录不可逆副作用安全台账 (Side-Effects Ledger) ---
        // 关键不可逆操作（如写入文件、修改数据库、执行写操作命令等）成功执行后记录台账，
        // 纯只读命令（如 git log/show/diff, search 等）不入台账；参数保持完整合法 JSON，绝不使用 ... 截断元数据
        if result.success
            && crate::agent::context::AuditLedger::is_mutating_side_effect(
                &instruction.get_tool(),
                &instruction.get_params(),
            )
        {
            let safe_params = crate::agent::context::AuditLedger::format_side_effect_params(
                &instruction.get_params(),
            );
            context.record_side_effect(
                step_id,
                instruction.get_tool(),
                format!("{}: {}", instruction.description, safe_params),
            );
        }

        context.current_observation = output_text.clone();
        // 复盘轨迹使用与 step_new 相同的语义字段，但与 Agent prompt/checkpoint 隔离。
        // 这使历史会话既能恢复思考、计划和工具调用，又不会放大运行时上下文。
        let metrics_json = outcome.token_usage.as_ref().map(|u| {
            json!({
                "prompt_tokens": u.prompt_tokens,
                "completion_tokens": u.completion_tokens,
                "total_tokens": u.total_tokens,
                "cached_tokens": u.cached_tokens,
                "cache_hit_rate": u.cache_hit_rate,
                "tokens_per_second": u.tokens_per_second,
                "ttft_ms": u.ttft_ms,
                "thinking_duration_ms": outcome.thinking_duration_ms,
                "tool_duration_ms": outcome.tool_duration_ms,
                "total_duration_ms": outcome.thinking_duration_ms + outcome.tool_duration_ms,
            })
        });

        run_trace_steps.push(json!({
            "id": step_id,
            "description": instruction.description.clone(),
            "reflection": instruction.reflection.clone(),
            "thought": instruction.thought.clone(),
            "tool": instruction.get_tool(),
            "action": instruction.get_action(),
            "command": instruction.get_params(),
            "params": instruction.get_params(),
            "plan_id": context.active_plan_id().unwrap_or_default(),
            "plan_title": context.active_plan_title().unwrap_or(&context.user_goal),
            "status": if finish_challenged { "finish_challenged" } else if result.success { "succeeded" } else { "failed" },
            "output": output_text.clone(),
            "resource_scope_revision": context.resource_scope.revision,
            "metrics": metrics_json,
            "token_usage": metrics_json,
            "timestamp": Utc::now().timestamp_millis(),
        }));
        if let Err(error) = crate::agent::run_trace::upsert_snapshot(
            state.inner(),
            &run_id,
            &final_session_id,
            &goal,
            "running",
            &run_trace_steps,
            None,
        ) {
            eprintln!("[run-trace] 无法保存步骤快照: {error}");
        }
        // 工具结果已经进入可恢复上下文。先提交 durable phase/cursor，再通知前端；
        // 这样崩溃恢复不会把已经完成的普通步骤当作待执行，也不会把 finish 误续跑。
        let is_actually_completed = is_finish && result.success && !is_effectively_challenged;
        if is_actually_completed {
            checkpoint.mark_done(&context);
        } else {
            // 被质询或失败的 finish 也要推进 durable cursor，恢复后继续下一轮而不是误判终态。
            checkpoint.complete_step(step_id, &context);
        }
        save_checkpoint(state.inner(), &mut checkpoint)?;

        // --- 前端通知 + 任务完成判定（合并） ---
        app.emit(&format!("agent-context-{}", final_session_id), &context)
            .map_err(|e| e.to_string())?;

        if is_actually_completed {
            advance_run_state(
                &app,
                &final_session_id,
                &mut run_machine,
                AgentRunEvent::BeginFinalizing,
            )?;
            // finish 指令：直接发 complete，不走普通的 step_done
            // 尝试从 command 里解析 finish 的结构化内容
            let finish_payload = instruction
                .command
                .clone()
                .unwrap_or(serde_json::Value::Null);

            let persisted_summary = finish_payload
                .get("summary")
                .and_then(|value| value.as_str())
                .or_else(|| {
                    finish_payload
                        .get("details")
                        .and_then(|value| value.as_str())
                })
                .unwrap_or(&output_text)
                .to_string();
            crate::agent::conversation::complete_turn(
                state.inner(),
                &final_session_id,
                &conversation_model_id,
                &persisted_summary,
            )?;
            // 完成区域展示模型自己的最终回复；若存在 3 次审查熔断遗留警告，注入到 emitted_finish
            let mut emitted_finish = match finish_payload {
                serde_json::Value::Object(object) => serde_json::Value::Object(object),
                _ => json!({ "summary": persisted_summary }),
            };
            if let Some(ref warnings) = reviewer_warnings_delivered {
                if let serde_json::Value::Object(ref mut map) = emitted_finish {
                    map.insert(
                        "reviewer_warnings".to_string(),
                        json!({
                            "score": warnings.score,
                            "summary": warnings.summary,
                            "blocking_issues": warnings.blocking_issues,
                            "nit_issues": warnings.nit_issues,
                        }),
                    );
                }
            }

            if let Err(error) = crate::agent::run_trace::upsert_snapshot(
                state.inner(),
                &run_id,
                &final_session_id,
                &goal,
                "succeeded",
                &run_trace_steps,
                Some(&emitted_finish),
            ) {
                eprintln!("[run-trace] 无法保存完成快照: {error}");
            }

            let complete_msg = if reviewer_warnings_delivered.is_some() {
                "任务已交付（审查达到重试上限，附带未收敛警告）"
            } else {
                "任务已由 AI 标记完成"
            };

            app.emit(
                &format!("agent-progress-{}", final_session_id),
                json!({
                    "type": "complete",
                    "message": complete_msg,
                    "success": true,
                    "output": output_text,      // 原始 JSON 字符串（兼容旧逻辑）
                    "finish": emitted_finish,   // 结构化成果对象，含 details/summary/artifacts
                }),
            )
            .map_err(|e| e.to_string())?;
            // 自动登记运行期间新生成的工作区文件为产物：微信"完成后回推文件"
            // 不再依赖模型记得调用 register_file（漏登记 = 用户只收到文字收不到文件）
            let registered = if is_explicit_workspace {
                crate::agent::artifact_manager::auto_register_run_artifacts(
                    state.inner(),
                    &final_session_id,
                    Some(&run_id),
                    &workspace_dir,
                    run_started_at,
                )
            } else {
                Vec::new()
            };
            if !registered.is_empty() {
                println!(
                    "[run-trace] 自动登记 {} 个运行期产物: {}",
                    registered.len(),
                    registered
                        .iter()
                        .map(|a| a.display_name.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            break;
        }

        // 普通步骤：发 step_done / step_error
        app.emit(
            &format!("agent-progress-{}", final_session_id),
            json!({
                "type": if result.success { "step_done" } else { "step_error" },
                "step_id": step_id,
                "description": instruction.description,
                "output": output_text,
                "metrics": metrics_json,
                "token_usage": metrics_json
            }),
        )
        .map_err(|e| e.to_string())?;

        sleep(Duration::from_millis(500)).await;
    }

    if run_machine.state() == AgentRunState::Finalizing {
        advance_run_state(
            &app,
            &final_session_id,
            &mut run_machine,
            AgentRunEvent::Succeed {
                summary: "任务已完成".to_string(),
            },
        )?;
    } else if !run_machine.state().is_terminal() {
        let terminal_error = format!("任务在达到最大步数 {} 后未完成", MAX_STEPS);
        advance_run_state(
            &app,
            &final_session_id,
            &mut run_machine,
            AgentRunEvent::Fail {
                reason: terminal_error.clone(),
            },
        )?;
        checkpoint.mark_failed(terminal_error.clone(), &context);
        save_checkpoint(state.inner(), &mut checkpoint)?;
        let failure_completion = json!({ "summary": terminal_error });
        if let Err(error) = crate::agent::run_trace::upsert_snapshot(
            state.inner(),
            &run_id,
            &final_session_id,
            &goal,
            "failed",
            &run_trace_steps,
            Some(&failure_completion),
        ) {
            eprintln!("[run-trace] 无法保存失败快照: {error}");
        }
    }
    {
        let mut channels = intervention_channels().lock().await;
        channels.remove(&final_session_id);
        stream_interrupt_channels()
            .lock()
            .await
            .remove(&final_session_id);
    }

    Ok(())
}
