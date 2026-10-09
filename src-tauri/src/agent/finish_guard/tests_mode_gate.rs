//! 执行模式切换与 cross_review 门禁状态机测试。

#[cfg(test)]
mod tests_inner {
    use super::super::testkit::*;
    use super::super::*;
    use crate::agent::dispatcher::is_write_or_side_effect_tool;
    use crate::agent::review_contract::ReviewObligation;

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
        checkpoint.submitted_plan = Some(crate::agent::types::PlanSubmission {
            plan_summary: "修复拖拽穿透".to_string(),
            target_files: vec!["src/dnd.rs".to_string()],
            root_cause: "命中测试未穿透".to_string(),
            evidence_refs: vec!["src/dnd.rs:10".to_string()],
            assumptions: vec![],
            open_questions: vec![],
            intended_verification: "cargo test dnd".to_string(),
            risk_level: "low".to_string(),
            revision_reason: String::new(),
            ..Default::default()
        });

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
        checkpoint.last_review_scorecard = Some(crate::agent::child_reviewer::ReviewScorecard::new(90, 80, 85, Some(70)));
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
}
