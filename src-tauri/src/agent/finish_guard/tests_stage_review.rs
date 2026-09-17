//! 阶段审查 blocker、对质降级与评审预算回滚测试。

#[cfg(test)]
mod tests_inner {
    use super::super::testkit::*;
    use super::super::*;
    use crate::agent::dispatcher::is_write_or_side_effect_tool;
    use crate::agent::review_contract::ReviewObligation;

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
    fn confrontation_maintained_twice_downgrades_blockers_to_final_only() {
        let context = context::SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.stage_review_blockers_open = 1;

        let blocker = vec!["F2: stage-13 仍为存根，父 Agent 宣称已实现与 diff 不符".to_string()];
        // 第 1 轮对质被维持：仅计数，不降级
        assert!(!checkpoint.note_stage_review_confrontation_maintained(blocker.clone(), 2));
        assert_eq!(checkpoint.stage_review_blockers_open, 1);
        assert_eq!(checkpoint.stage_review_confrontation_failures, 1);

        // 第 2 轮仍被维持：触发降级——blockers 清零放行写入，账带入 final_only
        assert!(checkpoint.note_stage_review_confrontation_maintained(blocker.clone(), 2));
        assert_eq!(checkpoint.stage_review_blockers_open, 0);
        assert_eq!(checkpoint.stage_review_final_only.len(), 1);
        assert!(checkpoint.stage_review_final_only[0].contains("对质未撤回自动降级"));
        assert!(checkpoint.stage_review_final_only[0].contains("F2"));
        assert_eq!(checkpoint.stage_review_confrontation_failures, 0);

        // 异议被撤回（空 blocker）时重置计数
        checkpoint.stage_review_confrontation_failures = 1;
        assert!(!checkpoint.note_stage_review_confrontation_maintained(vec![], 2));
        assert_eq!(checkpoint.stage_review_confrontation_failures, 0);
    }

    #[test]
    fn infra_failed_review_rounds_get_refund_with_cap() {
        let context = context::SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);

        use crate::agent::types::ChildErrorKind as K;

        // 预扣 2 轮后两次超时：各退 1 轮
        checkpoint.plan_reviewer_retry_count = 0;
        let _ = checkpoint.reserve_plan_review_round();
        let _ = checkpoint.reserve_plan_review_round();
        assert!(checkpoint.refund_infra_failed_review_round(Some(K::Timeout)));
        assert_eq!(checkpoint.plan_reviewer_retry_count, 1);
        assert!(checkpoint.refund_infra_failed_review_round(Some(K::Interrupted)));
        assert_eq!(checkpoint.plan_reviewer_retry_count, 0);
        assert_eq!(checkpoint.reviewer_budget_refunds, 2);

        // 预算为 0 时不再退款
        assert!(!checkpoint.refund_infra_failed_review_round(Some(K::Timeout)));

        // 审查者技术失败（invalid_output）应退款：技术失败与业务驳回是两笔账
        // （实测教训：error_kind 落库值是 "timeout" 不是 "timed_out"，也要认）
        checkpoint.plan_reviewer_retry_count = 2;
        checkpoint.reviewer_budget_refunds = 0;
        assert!(checkpoint.refund_infra_failed_review_round(Some(K::InvalidOutput)));
        assert_eq!(checkpoint.plan_reviewer_retry_count, 1);
        assert!(checkpoint.refund_infra_failed_review_round(Some(K::Timeout)));
        assert_eq!(checkpoint.plan_reviewer_retry_count, 0);

        // 请求层失败（402/断流等零产出）同属 infra 失败，也应退款
        checkpoint.plan_reviewer_retry_count = 1;
        checkpoint.reviewer_budget_refunds = 0;
        assert!(checkpoint.refund_infra_failed_review_round(Some(K::RequestOrStream)));
        assert_eq!(checkpoint.plan_reviewer_retry_count, 0);

        // 退款封顶 3 次：防持续故障下预算永不耗尽
        checkpoint.plan_reviewer_retry_count = 5;
        checkpoint.reviewer_budget_refunds = 3;
        assert!(!checkpoint.refund_infra_failed_review_round(Some(K::Timeout)));
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
}
