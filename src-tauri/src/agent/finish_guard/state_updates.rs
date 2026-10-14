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
    current_step_id: usize,
) -> bool {
    if !instruction.todo_update.is_empty() {
        apply_todo_update_guarded(context, &instruction.todo_update);
    }

    let review_contract_was_present = context.review_contract.is_some();
    if let Some(update) = instruction.review_contract_update.clone() {
        let ids = update
            .obligations
            .iter()
            .map(|o| o.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        match apply_review_contract_update(&mut context.review_contract, update) {
            Ok(()) => context.add_host_feedback(&format!(
                "【审查契约申报已保存】本次更新义务：{}；未提交的义务保持原值。evidenced/waived 仅为完成/豁免申报，不代表验收通过；被打回的项可按 id 补正证据或退回 open。最终验收仍检查完整契约、当前改动和终审结果。",
                ids
            )),
            Err(error) => context.add_host_feedback(&format!(
                "【审查契约更新未采纳】{}\n本次契约更新未保存，原有契约状态保持不变；不表示同轮工具未执行。可仅提交需要补正的义务 id + status + 对应 evidence_refs/waived_reason；未提交项自动保留。已有项的 question、scope_statement 和冻结字段可省略，不能静默改写原问题或冻结验证标准。",
                error
            )),
        }
    }

    if let Some(update) = instruction.change_verification_update.clone() {
        let evidence_window = crate::agent::change_verification::EvidenceWindow {
            last_write_step: context.side_effects[context.inherited_side_effects_count..]
                .iter()
                .map(|r| r.step_id)
                .max(),
            current_step: current_step_id.saturating_add(1),
        };
        if let Err(error) = apply_change_verification_update(
            &mut context.change_verification,
            update,
            &evidence_window,
        ) {
            context.add_host_feedback(&format!(
                "【写入验证更新未采纳】{}\n最近已记录副作用之后，可用 verified + evidence_refs=[\"step_N\"] 申报验证（本步工具已执行完，可引用本步），或用 waived + waived_reason 说明豁免；旧凭据不能覆盖新副作用。",
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
                    context.add_host_feedback(&format!(
                        "【✅ Scope 边界扩展已放行】目标块 [{}] 允许修改范围已扩充包含：{:?}（理由：{}）",
                        id, paths, reason
                    ));
                }
            }
        }
    }

    !review_contract_was_present && context.review_contract.is_some()
}

/// 应用 todo_update 并执行监管继承守卫。
/// [审查发现] 前缀的待办是审查环节的监管产出，不允许被 todo_update 静默剔除——
/// 更新中未保留的发现按原样恢复为 pending 并附恢复告警；
/// 销账的唯一途径：在更新中保留该条目并显式置为 done/failed（附处理说明）。
/// 所有 todo_list 的 todo_update 应用路径（loop_runner 状态更新 / main_loop 证据回填）
/// 必须统一走本函数，杜绝旁路。
pub(crate) fn apply_todo_update_guarded(
    context: &mut context::SandwichContext,
    todo_update: &[crate::agent::types::TodoItem],
) {
    if todo_update.is_empty() {
        return; // 空更新 = 未尝试管理待办，保持原状
    }
    let mut incoming = todo_update.to_vec();
    // 先收集被剔除的审查发现（不可变借用），再统一恢复（可变借用）
    let mut dropped_findings = Vec::new();
    for existing in &context.todo_list {
        if !existing.description.starts_with("[审查发现]") {
            continue;
        }
        let kept = incoming
            .iter()
            .any(|t| t.id == existing.id || t.description == existing.description);
        if !kept {
            dropped_findings.push(existing.clone());
        }
    }
    for restored in dropped_findings {
        context.add_error_feedback(&format!(
            "【审查发现待办被剔除，已自动恢复】\"{}\" 是审查环节的监管产出，禁止静默删除。要销账请在 todo_update 中保留该条目并置为 done（附处理说明）。",
            restored.description
        ));
        incoming.push(restored);
    }
    context.todo_list = incoming;
}

#[cfg(test)]
mod scope_feedback_tests {
    use super::*;
    use crate::agent::target_block::TargetBlock;
    use serde_json::json;

    fn context_with_block() -> context::SandwichContext {
        let mut ctx = context::SandwichContext::new("sys".into(), "goal".into());
        let mut block = TargetBlock::new("b1", "修复前端");
        block.scope = vec!["src/a.rs".into()];
        ctx.block_manager.set_blocks(vec![block]);
        ctx
    }

    fn instruction(reason: &str) -> AgentInstruction {
        serde_json::from_value(json!({"tool":"core", "action":"continue", "command":"", "params":{}, "scope_extension":{"paths":["src/b.rs"],"reason":reason}})).unwrap()
    }

    #[test]
    fn approved_scope_feedback_is_notification_not_format_error() {
        let mut ctx = context_with_block();
        apply_instruction_state_updates(&mut ctx, &instruction("必要的类型联动"), 3);
        assert!(ctx
            .active_target_block()
            .unwrap()
            .scope
            .contains(&"src/b.rs".to_string()));
        let text = ctx.turns_history.last().unwrap().content.as_str().unwrap();
        assert!(text.contains("Scope 边界扩展已放行"));
        assert!(text.starts_with("【宿主反馈】"));
        assert!(!text.contains("格式或执行错误"));
        assert!(!text.contains("纯 JSON"));
    }

    #[test]
    fn rejected_scope_stays_rejected_and_does_not_expand() {
        let mut ctx = context_with_block();
        apply_instruction_state_updates(&mut ctx, &instruction(""), 3);
        assert!(!ctx
            .active_target_block()
            .unwrap()
            .scope
            .contains(&"src/b.rs".to_string()));
        let text = ctx.turns_history.last().unwrap().content.as_str().unwrap();
        assert!(text.contains("Scope 扩展驳回"));
        assert!(text.contains("reason"));
    }
}

#[cfg(test)]
mod verification_order_tests {
    use super::*;
    use crate::agent::change_verification::ChangeVerification;
    #[test]
    fn completed_current_probe_is_valid_but_current_write_cannot_verify_itself() {
        let instruction = AgentInstruction {
            change_verification_update: Some(ChangeVerification {
                status: "verified".into(),
                evidence_refs: vec!["step_12".into()],
                waived_reason: None,
            }),
            ..Default::default()
        };
        let mut context = context::SandwichContext::new("system".into(), "goal".into());
        context.record_side_effect(10, "local_file".into(), "source write".into());
        apply_instruction_state_updates(&mut context, &instruction, 12);
        assert!(context
            .change_verification
            .as_ref()
            .is_some_and(ChangeVerification::is_resolved));
        context.record_side_effect(12, "local_file".into(), "later write".into());
        apply_instruction_state_updates(&mut context, &instruction, 12);
        assert!(context.change_verification.is_none());
    }
}
