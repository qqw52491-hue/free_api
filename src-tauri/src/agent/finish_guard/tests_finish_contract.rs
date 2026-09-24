//! 终态质询与审查/变更核验契约测试。

#[cfg(test)]
mod tests_inner {
    use super::super::testkit::*;
    use super::super::*;
    use crate::agent::dispatcher::is_write_or_side_effect_tool;
    use crate::agent::review_contract::ReviewObligation;

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
            affected_paths: Vec::new(),
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
            affected_paths: Vec::new(),
        });
        assert!(complex_delivery_blocker(&context, &instruction, true, true).is_none());

        context.review_contract = Some(ReviewContract {
            scope_statement: "核验缓存调用链".to_string(),
            obligations: vec![crate::agent::review_contract::ReviewObligation {
                verify_command: None,
                verify_script: None,
                verify_sha256: None,
                closure_form: None,
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
    fn unreviewed_release_requires_disclosure_in_summary() {
        let mut context =
            context::SandwichContext::new("system".to_string(), "修复图片上传".to_string());
        context.side_effects.push(crate::agent::context::SideEffectRecord {
            step_id: 1,
            tool: "write".to_string(),
            description: "fake".to_string(),
            timestamp: "now".to_string(),
            affected_paths: Vec::new(),
        });
        context.plan_gate = Some(crate::agent::context::types::PlanGateProjection {
            mode: "cross_review".to_string(),
            phase: "approved".to_string(),
            unreviewed_release: true,
            ..Default::default()
        });
        // 未经审查放行 + 摘要未披露 → 拦截
        let silent = AgentInstruction {
            command: Some(serde_json::json!({ "summary": "# 修复完成\n已修改图片上传逻辑。" })),
            ..Default::default()
        };
        assert!(complex_delivery_blocker(&context, &silent, true, true).is_some());
        // 摘要如实披露「未经独立审查」→ 放行
        let disclosed = AgentInstruction {
            command: Some(serde_json::json!({ "summary": "# 修复完成\n已修改图片上传逻辑。注意：本方案未经独立审查（审查员全部失败，经人工放行）。" })),
            ..Default::default()
        };
        assert!(complex_delivery_blocker(&context, &disclosed, true, true).is_none());
        // 审查正常消费过的运行不受影响
        context.plan_gate = Some(crate::agent::context::types::PlanGateProjection {
            mode: "cross_review".to_string(),
            phase: "approved".to_string(),
            unreviewed_release: false,
            reviewer_consumed: true,
            ..Default::default()
        });
        assert!(complex_delivery_blocker(&context, &silent, true, true).is_none());
    }

    #[test]
    fn review_contract_blocks_first_finish_without_forcing_step_counts() {
        let open_contract = ReviewContract {
            scope_statement: "审查上下文设计".to_string(),
            obligations: vec![crate::agent::review_contract::ReviewObligation {
                verify_command: None,
                verify_script: None,
                verify_sha256: None,
                closure_form: None,
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
                verify_command: None,
                verify_script: None,
                verify_sha256: None,
                closure_form: None,
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

        let review_started = apply_instruction_state_updates(&mut context, &instruction, 10);

        assert!(!review_started);
        assert!(context.review_contract.is_none());
        assert_eq!(context.change_verification, Some(expected));
    }
}
