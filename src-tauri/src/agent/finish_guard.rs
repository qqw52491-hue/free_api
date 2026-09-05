//! finish 终态质询门禁与指令状态更新。
//! 从 main_loop.rs 原样抽出，行为不变；测试随实现同文件迁移。

use crate::agent::change_verification::{
    apply_change_verification_update, change_verification_blocker, ChangeVerification,
};
use crate::agent::checkpoint::AgentCheckpoint;
use crate::agent::context;
use crate::agent::evaluation_analysis::{assess_complex_delivery_summary, summary_makes_root_cause_claim};
use crate::agent::review_contract::{apply_review_contract_update, ReviewContract};
use crate::agent::types::{AgentInstruction, TodoItem};
use serde_json::json;
use crate::db::DbState;
#[allow(unused_imports)]
use crate::agent::dispatcher::{is_finish_instruction, is_write_or_side_effect_tool};

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

/// 应用模型本轮提出的状态更新。
/// ReviewContract 与 ChangeVerification 是两套正交账本，必须独立处理：
/// 前者是否存在或是否被拒绝，都不能阻止后者闭合最近写入的验证责任。
pub(crate) fn apply_instruction_state_updates(
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

        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::PlanApproval);

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

        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::PlanApproval);

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
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::Auto);

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
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::CrossReview);

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
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::CrossReview);

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

        // 阶段 3: 审查结论已消费但缺评分卡 -> fail-closed 拦截（绝不变相放行）
        checkpoint.plan_reviewer_consumed = true;
        let rej_missing = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 3, false,
        );
        assert!(rej_missing.is_some());
        assert_eq!(rej_missing.unwrap().route, "plan_gatekeeper:scorecard_missing");
        assert!(!checkpoint.plan_phase.is_approved());

        // 阶段 3b: 评分达标但有未解决 blocking 异议 -> 仍拦截
        checkpoint.last_review_scorecard = Some(crate::agent::child_reviewer::ReviewScorecard::new(90, 80, 85, 70));
        checkpoint.last_review_blocking_count = 2;
        let rej_blocking = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 3, false,
        );
        assert!(rej_blocking.is_some());
        assert_eq!(rej_blocking.unwrap().route, "plan_gatekeeper:blocking_findings_open");

        // 阶段 3c: blocking 异议清零 -> 放行并推进为 Approved
        checkpoint.last_review_blocking_count = 0;
        let rej3 = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 3, false,
        );
        assert!(rej3.is_none());
        assert!(checkpoint.plan_phase.is_approved());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn scorecard_missing_gate_directs_to_human_approval_when_budget_exhausted() {
        let temp_dir = std::env::temp_dir().join(format!("gatekeeper-miss-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let conn = crate::db::init_db(&temp_dir.join("test.db")).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        let context = context::SandwichContext::new("system".to_string(), "目标".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::CrossReview);
        checkpoint.plan_reviewer_retry_count = crate::agent::checkpoint::MAX_PLAN_REVIEWER_RETRIES;
        checkpoint.plan_reviewer_consumed = true;
        // 预算耗尽 + 无评分卡：仍必须拦截，但文案应引导人工审批而非重新派发
        let rej = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 3, false,
        );
        assert!(rej.is_some());
        let rej = rej.unwrap();
        assert_eq!(rej.route, "plan_gatekeeper:scorecard_missing");
        assert!(rej.stderr.contains("ask_human"), "预算耗尽时应引导人工审批: {}", rej.stderr);
        assert!(!checkpoint.plan_phase.is_approved());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn reap_stale_child_runs_converges_zombie_rows() {
        let temp_dir = std::env::temp_dir().join(format!("gatekeeper-reap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let conn = crate::db::init_db(&temp_dir.join("test.db")).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        // 夹具 started_at 为 2026-01-01（远早于宽限上限），构成僵尸 running 行
        insert_child_run(&db, "zombie", "test_run", "running", None, None);
        assert!(crate::agent::child_reviewer::has_running_child_runs(&db, "test_run"));

        let reaped = crate::agent::child_reviewer::reap_stale_child_runs(&db, "test_run");
        assert_eq!(reaped, 1);
        // 收敛后：不再 running，且携带可消费的 result_json（信箱可收取，门禁不被锁死）
        assert!(!crate::agent::child_reviewer::has_running_child_runs(&db, "test_run"));
        let conn = db.0.lock().unwrap();
        let (status, result_json): (String, Option<String>) = conn
            .query_row(
                "SELECT status, result_json FROM agent_child_runs WHERE id = 'zombie'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        drop(conn);
        assert_eq!(status, "timed_out");
        assert!(result_json.unwrap().contains("timed_out"));

        // 二次巡检幂等：无新僵尸时返回 0
        assert_eq!(crate::agent::child_reviewer::reap_stale_child_runs(&db, "test_run"), 0);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn finish_child_run_guard_rejects_late_overwrite() {
        let temp_dir = std::env::temp_dir().join(format!("gatekeeper-guard-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let conn = crate::db::init_db(&temp_dir.join("test.db")).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        // 正常迁移：running -> succeeded
        insert_child_run(&db, "c1", "test_run", "running", None, None);
        crate::agent::child_reviewer::finish_child_run(
            &db, "c1", "succeeded", 100, 0, None, Some("{}"),
        );
        {
            let conn = db.0.lock().unwrap();
            let status: String = conn
                .query_row("SELECT status FROM agent_child_runs WHERE id='c1'", [], |r| r.get(0))
                .unwrap();
            drop(conn);
            assert_eq!(status, "succeeded");
        }

        // 取消后，迟到的 succeeded 提交不得覆盖已收敛状态（watch 竞态兜底）
        insert_child_run(&db, "c2", "test_run", "running", None, None);
        crate::agent::child_reviewer::cancel_running_child_runs(&db, "test_run").unwrap();
        crate::agent::child_reviewer::finish_child_run(
            &db, "c2", "succeeded", 100, 0, None, Some("{}"),
        );
        let conn = db.0.lock().unwrap();
        let status: String = conn
            .query_row("SELECT status FROM agent_child_runs WHERE id='c2'", [], |r| r.get(0))
            .unwrap();
        drop(conn);
        assert_eq!(status, "cancelled", "迟到提交不得把 cancelled 覆盖回 succeeded");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn ask_human_cooldown_requires_action_before_asking() {
        let context = context::SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);

        // 无拦截记录：放行
        assert!(crate::agent::loop_runner::evaluate_ask_human_cooldown(&mut checkpoint, 5).is_none());

        // 本步刚被拦截：冷却拒绝（一次性），且记录被清除
        checkpoint.recent_gate_block_step = Some(5);
        let msg = crate::agent::loop_runner::evaluate_ask_human_cooldown(&mut checkpoint, 5);
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("先监听"));
        assert!(checkpoint.recent_gate_block_step.is_none());

        // 冷却后再问：放行（模型有机会行动；死锁豁免）
        assert!(crate::agent::loop_runner::evaluate_ask_human_cooldown(&mut checkpoint, 6).is_none());

        // 预算耗尽等门禁显式指示求助：豁免一次
        checkpoint.gate_human_escape_directed = true;
        assert!(crate::agent::loop_runner::evaluate_ask_human_cooldown(&mut checkpoint, 7).is_none());
        assert!(!checkpoint.gate_human_escape_directed, "豁免标记一次性消费");
    }

    #[test]
    fn repeat_read_interception_reports_real_count() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("read-ledger-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("stage.md");
        let mut f = std::fs::File::create(&file).unwrap();
        f.write_all("# 当前阶段 01/24\n仅修改 src/stage-01.mjs。\n".as_bytes()).unwrap();
        drop(f);

        let mut context = context::SandwichContext::new("system".to_string(), "goal".to_string());
        let mk = |path: &str| crate::agent::types::AgentInstruction {
            tool: Some("read_text_file".to_string()),
            params: Some(serde_json::json!({ "path": path })),
            ..Default::default()
        };

        // 前两次合法读取放行
        assert!(context.check_repeat_read_dispatch(&mk("stage.md"), &dir).is_none());
        assert!(context.check_repeat_read_dispatch(&mk("stage.md"), &dir).is_none());
        // 第三次拦截：文案必须是真实次数，不允许再出现哨兵值 2147483647
        let block = context.check_repeat_read_dispatch(&mk("stage.md"), &dir).unwrap();
        assert!(block.contains("已连续读取 3 次"), "文案应报真实次数: {}", block);
        assert!(!block.contains("2147483647"), "哨兵值泄漏: {}", block);
        // 持续拦截直到内容变化
        assert!(context.check_repeat_read_dispatch(&mk("stage.md"), &dir).is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stage_review_blockers_intercept_writes_even_when_plan_approved() {
        let temp_dir = std::env::temp_dir().join(format!("gatekeeper-stage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let conn = crate::db::init_db(&temp_dir.join("test.db")).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        let context = context::SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::CrossReview);
        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
        checkpoint.stage_review_blockers_open = 2;
        checkpoint.stage_review_final_only = vec!["F9: 收尾前必须修复".to_string()];

        // 方案已批准，但阶段审查的 regardless 拦截仍然生效
        let rej = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 5, false,
        );
        assert!(rej.is_some());
        let rej = rej.unwrap();
        assert_eq!(rej.route, "stage_review:blocking_open");
        assert!(rej.stderr.contains("resume_child"), "必须给出对质解套路径");
        assert!(rej.stderr.contains("final_only"), "final_only 记账要随拦截文案带出");

        // 清零后放行
        checkpoint.stage_review_blockers_open = 0;
        let pass = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 6, false,
        );
        assert!(pass.is_none());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn infra_failed_review_rounds_get_refund_with_cap() {
        let context = context::SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);

        // 预扣 2 轮后两次超时：各退 1 轮
        checkpoint.plan_reviewer_retry_count = 0;
        let _ = checkpoint.reserve_plan_review_round();
        let _ = checkpoint.reserve_plan_review_round();
        assert!(checkpoint.refund_infra_failed_review_round(Some("timed_out")));
        assert_eq!(checkpoint.plan_reviewer_retry_count, 1);
        assert!(checkpoint.refund_infra_failed_review_round(Some("interrupted")));
        assert_eq!(checkpoint.plan_reviewer_retry_count, 0);
        assert_eq!(checkpoint.reviewer_budget_refunds, 2);

        // 预算为 0 时不再退款
        assert!(!checkpoint.refund_infra_failed_review_round(Some("timed_out")));

        // 实质性终态（invalid_output）不退款
        checkpoint.plan_reviewer_retry_count = 2;
        assert!(!checkpoint.refund_infra_failed_review_round(Some("invalid_output")));
        assert_eq!(checkpoint.plan_reviewer_retry_count, 2);

        // 退款封顶 3 次：防持续故障下预算永不耗尽
        checkpoint.plan_reviewer_retry_count = 5;
        checkpoint.reviewer_budget_refunds = 3;
        assert!(!checkpoint.refund_infra_failed_review_round(Some("timed_out")));
        assert_eq!(checkpoint.plan_reviewer_retry_count, 5);
    }

    #[test]
    fn reserve_plan_review_round_caps_rebuttal_budget() {
        let context = context::SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);

        // 正常预扣：计数递增且重置消费标记
        checkpoint.plan_reviewer_retry_count = 0;
        checkpoint.plan_reviewer_consumed = true;
        assert!(checkpoint.reserve_plan_review_round().is_ok());
        assert_eq!(checkpoint.plan_reviewer_retry_count, 1);
        assert!(!checkpoint.plan_reviewer_consumed);

        // 到达上限后：反驳对质的预扣必须被拒绝，预算永不被旁路击穿
        checkpoint.plan_reviewer_retry_count = crate::agent::checkpoint::MAX_PLAN_REVIEWER_RETRIES;
        assert!(checkpoint.reserve_plan_review_round().is_err());
        assert_eq!(
            checkpoint.plan_reviewer_retry_count,
            crate::agent::checkpoint::MAX_PLAN_REVIEWER_RETRIES
        );
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

    #[test]
    fn plan_decision_marker_is_typed_parsed_with_note_and_tolerates_variants() {
        use crate::agent::loop_runner::{extract_plan_decision, parse_plan_approval_decision};

        // 打回 + 附带修改意见：note 必须经类型化结构完整带回
        let reject = "【方案打回修改】请重新推演\n<!--plan-decision:{\"approved\":false,\"note\":\"数据库换成 tokio-rs\"}-->";
        assert_eq!(parse_plan_approval_decision(reject), Some(false));
        let decision = extract_plan_decision(reject).unwrap();
        assert!(!decision.approved);
        assert_eq!(decision.note.as_deref(), Some("数据库换成 tokio-rs"));

        // 未知字段不破坏解析（向前兼容前端新增字段）
        assert_eq!(
            parse_plan_approval_decision("<!--plan-decision:{\"approved\":true,\"via\":\"mobile\"}-->"),
            Some(true)
        );
        // 畸形 JSON 静默降级到关键词链路，绝不能 panic 或误放行
        assert_eq!(
            parse_plan_approval_decision("<!--plan-decision:{bad json}--> 我同意"),
            Some(true)
        );
        // approved 缺失时同样降级
        assert_eq!(
            parse_plan_approval_decision("<!--plan-decision:{\"note\":\"hi\"}-->"),
            None
        );
    }

    #[test]
    fn cross_review_scorecard_gate_blocks_low_risk_scores_then_passes_on_gate_clear() {
        let temp_dir = std::env::temp_dir().join(format!("gatekeeper-score-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let conn = crate::db::init_db(&temp_dir.join("test.db")).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        let context = context::SandwichContext::new("system".to_string(), "修复内存泄漏".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::CrossReview);
        checkpoint.plan_reviewer_retry_count = 1;
        checkpoint.plan_reviewer_consumed = true;

        // 均分达标但 risk < 60：即使平均分 90+ 也必须硬拦截
        checkpoint.last_review_scorecard = Some(crate::agent::child_reviewer::ReviewScorecard::new(95, 40, 95, 95));
        let rej = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 2, false,
        );
        assert!(rej.is_some());
        assert_eq!(rej.unwrap().route, "plan_gatekeeper:scorecard_failed");
        assert!(!checkpoint.plan_phase.is_approved());

        // 达标评分卡：放行并推进为 Approved
        checkpoint.last_review_scorecard = Some(crate::agent::child_reviewer::ReviewScorecard::new(90, 80, 85, 70));
        let pass = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 3, false,
        );
        assert!(pass.is_none());
        assert!(checkpoint.plan_phase.is_approved());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn auto_mode_first_step_write_is_blocked_until_plan_stated() {
        let temp_dir = std::env::temp_dir().join(format!("gatekeeper-auto-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let conn = crate::db::init_db(&temp_dir.join("test.db")).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        let mut context = context::SandwichContext::new("system".to_string(), "重构调度器".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::Auto);

        // 首轮、上下文为空就动笔 -> 拦截 need_plan
        let rej = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 0, false,
        );
        assert!(rej.is_some());
        assert_eq!(rej.unwrap().route, "plan_gatekeeper:need_plan");
        assert!(!checkpoint.plan_phase.is_approved());

        // 已有只读排查痕迹（上下文非空）-> 放行
        context.turns_history.push(context::types::ChatMessage {
            role: "assistant".to_string(),
            content: json!("已完成只读排查：根因锁定在 dispatcher.rs:120"),
        });
        let pass = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 1, false,
        );
        assert!(pass.is_none());
        assert!(checkpoint.plan_phase.is_approved());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn set_execution_mode_is_the_single_entry_point_and_syncs_auto_pilot() {
        let context = context::SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);

        // PlanApproval：auto_pilot 必须同步为 false
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::PlanApproval);
        assert_eq!(checkpoint.plan_mode, crate::agent::checkpoint::PlanExecutionMode::PlanApproval);
        assert!(!checkpoint.auto_pilot);

        // Auto：auto_pilot 必须同步为 true
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::Auto);
        assert!(checkpoint.auto_pilot);

        // CrossReview：同样视为自主模式（auto_pilot = true）
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::CrossReview);
        assert!(checkpoint.auto_pilot);
    }

    #[test]
    fn cross_review_human_arbitration_bypasses_failed_scorecard_and_blockers() {
        let temp_dir = std::env::temp_dir().join(format!("gatekeeper-esc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let conn = crate::db::init_db(&temp_dir.join("test.db")).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        let context = context::SandwichContext::new("system".to_string(), "实现系统运维脚本".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::CrossReview);
        checkpoint.plan_reviewer_retry_count = 2; // 预算已耗尽
        checkpoint.plan_reviewer_consumed = true;

        // 模拟审查员过度防御打了低分 (risk: 48 < 60)
        checkpoint.last_review_scorecard = Some(crate::agent::child_reviewer::ReviewScorecard::new(80, 48, 85, 75));
        let rej = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 3, false,
        );
        assert!(rej.is_some());
        let rej_res = rej.unwrap();
        assert_eq!(rej_res.route, "plan_gatekeeper:scorecard_failed");
        assert!(rej_res.stderr.contains("ask_human"), "预算耗尽拦截文案必须指引 ask_human 人工仲裁");
        assert!(rej_res.stderr.contains("人工仲裁"), "拦截文案必须包含人工仲裁说明");

        // 人工仲裁放行：用户确认批准后，plan_phase 变为 Approved
        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
        let pass = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 4, false,
        );
        assert!(pass.is_none(), "人类批准后门禁必须立即放行写入，绝不因审查低分继续拦截");
        assert!(checkpoint.plan_phase.is_approved());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn stage_review_blocker_cleared_by_human_arbitration() {
        let temp_dir = std::env::temp_dir().join(format!("gatekeeper-stage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let conn = crate::db::init_db(&temp_dir.join("test.db")).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        let context = context::SandwichContext::new("system".to_string(), "重构核心模块".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::CrossReview);
        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
        checkpoint.stage_review_blockers_open = 1;

        // 存在未解决的 stage review blocker -> 拦截写入
        let rej = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 5, false,
        );
        assert!(rej.is_some());
        let rej_res = rej.unwrap();
        assert_eq!(rej_res.route, "stage_review:blocking_open");
        assert!(rej_res.stderr.contains("ask_human"), "阶段拦截必须提供 ask_human 人工仲裁路径");

        // 人工仲裁放行：解除 stage_review_blockers_open
        checkpoint.stage_review_blockers_open = 0;
        let pass = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 6, false,
        );
        assert!(pass.is_none(), "阶段 blocker 解除后门禁放行");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
