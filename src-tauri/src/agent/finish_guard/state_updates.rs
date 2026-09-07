//! 指令执行后的状态更新、TargetBlocks 自动桥接、权限提升与 Scope 扩展。

use crate::agent::change_verification::apply_change_verification_update;
use crate::agent::context;
use crate::agent::review_contract::apply_review_contract_update;
use crate::agent::types::AgentInstruction;

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

