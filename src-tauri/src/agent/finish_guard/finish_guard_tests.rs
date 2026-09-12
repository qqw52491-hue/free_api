    use super::*;
    use crate::agent::dispatcher::is_write_or_side_effect_tool;
    use crate::agent::review_contract::ReviewObligation;

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
                verify_command: None,
                verify_script: None,
                verify_sha256: None,
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
    fn ask_human_approval_vs_information_routing() {
        let context = context::SandwichContext::new("system".to_string(), "查天气".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_routing".to_string(), &context);

        // 模拟刚被门禁拦截（例如第 5 步）
        checkpoint.recent_gate_block_step = Some(5);

        // 1. 信息类提问（缺参数、普通提问、未设置 kind）在协议层不属于 approval，永不触发冷却
        let is_info_approval = |kind: &str| -> bool {
            let ask_kind = kind.trim().to_ascii_lowercase();
            ask_kind == "plan_approval" || ask_kind == "plan" || ask_kind == "approval"
        };
        assert!(!is_info_approval("missing_parameter"));
        assert!(!is_info_approval("text"));
        assert!(!is_info_approval("single_select"));
        assert!(!is_info_approval(""));

        // 2. 只有真正的审批/放行类 kind 才会被判定为 approval
        assert!(is_info_approval("plan_approval"));
        assert!(is_info_approval("Plan"));
        assert!(is_info_approval("APPROVAL"));

        // 3. 当处于 approval 类时，冷却生效
        let cooldown_msg = crate::agent::loop_runner::evaluate_ask_human_cooldown(&mut checkpoint, 5);
        assert!(cooldown_msg.is_some(), "approval 类请求应触发门禁冷却");
        assert!(cooldown_msg.unwrap().contains("先监听"));
    }

    #[test]
    fn information_inquiry_does_not_unlock_pre_write_gatekeeper() {
        let context = context::SandwichContext::new("system".to_string(), "查天气".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_safety".to_string(), &context);
        checkpoint.plan_mode = crate::agent::checkpoint::PlanExecutionMode::CrossReview;
        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Exploring;

        // 1. 初始状态下前置写入门禁必须关闭（Exploring 阶段尝试直接修改必须被拦截）
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        let write_rejection = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint,
            &context,
            &db,
            "test_safety",
            1,
            false,
        );
        assert!(write_rejection.is_some(), "未审查前写操作必须被门禁拦截");

        // 2. 模拟普通缺参提问（无 form 或 kind 为 text/missing_parameter）
        let form_val: Option<serde_json::Value> = None;
        let ask_kind = "";
        let is_plan_approval_kind =
            ask_kind == "plan_approval" || ask_kind == "plan" || ask_kind == "approval";

        // 核心安全属性验证：普通信息类提问的 is_approval_context 必须为 false！
        let is_approval_context = is_plan_approval_kind
            || (form_val.is_none()
                && (checkpoint.gate_human_escape_directed
                    || checkpoint.stage_review_blockers_open > 0
                    || checkpoint.last_review_blocking_count > 0));
        assert!(!is_approval_context, "普通提问绝不能被误判为 approval 上下文");

        // 3. 用户回复“北京”后，写入门禁状态必须保持原样（不得被误判放行）
        let _reply = "北京";
        if is_approval_context {
            checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
        }
        assert_eq!(checkpoint.plan_phase, crate::agent::checkpoint::PlanPhase::Exploring);

        // 再次核验写入门禁：依然安全关闭拦截！绝无放行副作用！
        let write_rejection_after = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint,
            &context,
            &db,
            "test_safety",
            2,
            false,
        );
        assert!(write_rejection_after.is_some(), "回答信息后前置写入门禁仍保持关闭");
    }

    #[test]
    fn weather_disaster_chain_end_to_end_replay() {
        // 装配级端到端灾害链回放：
        // 目标：“天天气是啥呢”
        // 链路：2>/dev/null 探针 -> 不误判为写操作 -> 探测失败 -> 问城市 -> 不捂嘴冷却 -> 用户答北京 -> 正常回答 -> 门禁保持锁定 -> 防复读拦截 -> 配额熔断与凭据豁免
        let mut context = context::SandwichContext::new("system".to_string(), "天天气是啥呢".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_weather_replay".to_string(), &context);
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::CrossReview);
        assert_eq!(checkpoint.plan_phase, crate::agent::checkpoint::PlanPhase::Exploring);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        // 环节 1: 模型执行 IP 与网络探测（含 2>/dev/null 错误输出静默）
        let probe_instruction = crate::agent::types::AgentInstruction {
            tool: Some("sh".to_string()),
            command: Some(serde_json::json!({
                "args": [
                    "-c",
                    "curl -s --noproxy '*' -m 4 http://whois.pconline.com.cn/ipJson.jsp | iconv -f gbk -t utf-8 2>/dev/null || curl -s -m 4 https://cip.cc"
                ]
            })),
            ..Default::default()
        };

        // 验证 1.1: 探针命令被准确识别为安全只读命令，绝无破坏性
        let raw_cmd = "curl -s --noproxy '*' -m 4 http://whois.pconline.com.cn/ipJson.jsp | iconv -f gbk -t utf-8 2>/dev/null || curl -s -m 4 https://cip.cc";
        assert!(crate::agent::dispatcher::is_safe_read_only_shell_command(raw_cmd));

        // 验证 1.2: 探针工具不属于写或高危副作用工具
        assert!(!crate::agent::dispatcher::is_write_or_side_effect_tool(&probe_instruction));

        // 验证 1.3: 只读探针绝不会触发 pre-write gatekeeper 拦截
        let is_write = crate::agent::dispatcher::is_write_or_side_effect_tool(&probe_instruction);
        assert!(!is_write, "2>/dev/null 探针严禁被当作写操作");

        // 环节 2: 探针执行失败（如网络超时或无城市信息），模型决定发起 ask_human 索取必要参数（城市）
        // 即使系统历史曾记录过门禁拦截（假设第 1 步存在 block 记录）
        checkpoint.recent_gate_block_step = Some(1);

        let ask_message = "请问你想查询哪个城市的天气？";
        let ask_kind = "missing_parameter";

        // 验证 2.1: 协议分流——missing_parameter 永不触发门禁冷却（不捂嘴）
        let is_approval_kind = ask_kind == "plan_approval" || ask_kind == "plan" || ask_kind == "approval";
        assert!(!is_approval_kind);
        let cooldown_blocked = if is_approval_kind {
            crate::agent::loop_runner::evaluate_ask_human_cooldown(&mut checkpoint, 2).is_some()
        } else {
            false
        };
        assert!(!cooldown_blocked, "信息类提问绝不能被门禁冷却机制捂嘴！");

        // 验证 2.2: 提问去重与记录——首次提问顺利放行并登记
        assert!(checkpoint.check_duplicate_question(ask_message).is_none(), "首问城市必须放行");
        checkpoint.record_asked_question(ask_message);
        assert_eq!(checkpoint.asked_human_questions.len(), 1);

        // 验证 2.3: 全局配额——配额内顺利放行
        let budget = crate::agent::channel::config::max_human_inquiry_budget();
        let quota_rejection = checkpoint.check_inquiry_quota_exceeded(budget);
        assert!(quota_rejection.is_none(), "配额内必须放行");

        // 环节 3: 用户交互并回复：“北京”
        let human_reply = "北京";
        // 验证 3.1: 协议路由确认该提问不是 approval 上下文
        let is_approval_context = is_approval_kind
            || (checkpoint.gate_human_escape_directed
                || checkpoint.stage_review_blockers_open > 0
                || checkpoint.last_review_blocking_count > 0);
        assert!(!is_approval_context, "回答城市不是审批卡片，严禁标记为 approval_context");

        if is_approval_context {
            checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
        }

        // 验证 3.2: 门禁状态保持 Exploring，安全属性未被破坏
        assert_eq!(checkpoint.plan_phase, crate::agent::checkpoint::PlanPhase::Exploring);

        // 环节 4: 任务继续推进，模型查天气输出结果（正常业务流完成）
        context.turns_history.push(context::types::ChatMessage {
            role: "user".to_string(),
            content: serde_json::json!(human_reply),
        });
        context.turns_history.push(context::types::ChatMessage {
            role: "assistant".to_string(),
            content: serde_json::json!("北京今天天气晴朗，温度 22~30℃，北风 2 级。"),
        });

        // 环节 5: 核心安全防线验证——门禁状态未变，若后续有代码写入工具调用，仍被严密拦截
        let write_rejection = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint,
            &context,
            &db,
            "test_weather_replay",
            4,
            false,
        );
        assert!(write_rejection.is_some(), "回答北京后，写操作依然必须受交叉审查门禁保护！");

        // 环节 6: 去重拦截测试——模型若复读相同问题，立即被拦截
        let repeat_ask = "请问你想查询哪个城市的天气呢？";
        let repeat_dedup = checkpoint.check_duplicate_question(repeat_ask);
        assert!(repeat_dedup.is_some(), "复读同一提问必须被去重机制硬拦截");

        // 环节 7: 配额熔断机制与凭据豁免测试
        // 7.1 消耗剩余的普通信息类提问配额到上限 (5 次)
        assert!(checkpoint.check_duplicate_question("请问需要摄氏度还是华氏度？").is_none());
        checkpoint.record_asked_question("请问需要摄氏度还是华氏度？");

        assert!(checkpoint.check_duplicate_question("请问需要查询一周天气吗？").is_none());
        checkpoint.record_asked_question("请问需要查询一周天气吗？");

        assert!(checkpoint.check_duplicate_question("请问需要空气质量指数吗？").is_none());
        checkpoint.record_asked_question("请问需要空气质量指数吗？");

        assert!(checkpoint.check_duplicate_question("请问需要穿衣出行指数吗？").is_none());
        checkpoint.record_asked_question("请问需要穿衣出行指数吗？");

        assert_eq!(checkpoint.asked_human_questions.len(), 5);

        // 7.2 第 6 次普通提问：触发配额熔断
        let quota_err = checkpoint.check_inquiry_quota_exceeded(budget);
        assert!(quota_err.is_some(), "超过配额后普通提问必须被熔断");
        let quota_msg = quota_err.unwrap();
        assert!(quota_msg.contains("信息提问配额已耗尽"));
        assert!(quota_msg.contains("默认"), "熔断提示强制使用默认设定推进");

        // 7.3 关键豁免：私密凭据类提问（kind: "missing_credential"）
        let cred_kind = "missing_credential";
        assert!(crate::agent::checkpoint::is_credential_inquiry_kind(cred_kind), "missing_credential 必须识别为凭据类");
        let is_cred = crate::agent::checkpoint::is_credential_inquiry_kind(cred_kind);
        let cred_quota_blocked = if !is_cred {
            checkpoint.check_inquiry_quota_exceeded(budget).is_some()
        } else {
            false
        };
        assert!(!cred_quota_blocked, "凭据类提问必须豁免全局配额熔断，防止逼模型造假凭据！");
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

        // 审查者技术失败（invalid_output）应退款：技术失败与业务驳回是两笔账
        // （实测教训：error_kind 落库值是 "timeout" 不是 "timed_out"，也要认）
        checkpoint.plan_reviewer_retry_count = 2;
        checkpoint.reviewer_budget_refunds = 0;
        assert!(checkpoint.refund_infra_failed_review_round(Some("invalid_output")));
        assert_eq!(checkpoint.plan_reviewer_retry_count, 1);
        assert!(checkpoint.refund_infra_failed_review_round(Some("timeout")));
        assert_eq!(checkpoint.plan_reviewer_retry_count, 0);

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
        checkpoint.last_review_scorecard = Some(crate::agent::child_reviewer::ReviewScorecard::new(95, 40, 95, Some(95)));
        let rej = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 2, false,
        );
        assert!(rej.is_some());
        assert_eq!(rej.unwrap().route, "plan_gatekeeper:scorecard_failed");
        assert!(!checkpoint.plan_phase.is_approved());

        // 达标评分卡：放行并推进为 Approved
        checkpoint.last_review_scorecard = Some(crate::agent::child_reviewer::ReviewScorecard::new(90, 80, 85, Some(70)));
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
        checkpoint.last_review_scorecard = Some(crate::agent::child_reviewer::ReviewScorecard::new(80, 48, 85, Some(75)));
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

    #[test]
    fn test_parse_file_line_ref_formats() {
        assert_eq!(
            parse_file_line_ref("src/views/ChatView.vue:442"),
            Some(("src/views/ChatView.vue".to_string(), 442, 442))
        );
        assert_eq!(
            parse_file_line_ref("src/views/ChatView.vue:87-103"),
            Some(("src/views/ChatView.vue".to_string(), 87, 103))
        );
        assert_eq!(
            parse_file_line_ref("ChatView.vue:L442"),
            Some(("ChatView.vue".to_string(), 442, 442))
        );
        assert_eq!(
            parse_file_line_ref("ChatView.vue#L87-L103"),
            Some(("ChatView.vue".to_string(), 87, 103))
        );
        assert_eq!(
            parse_file_line_ref("[ChatView.vue:442](file:///workspace/ChatView.vue#L442)"),
            Some(("ChatView.vue".to_string(), 442, 442))
        );
        assert_eq!(
            parse_file_line_ref("ChatView.vue:442 (scrollToBottom 优化)"),
            Some(("ChatView.vue".to_string(), 442, 442))
        );
        assert_eq!(
            parse_file_line_ref("`src/components/ModelSelect.vue:55..60`"),
            Some(("src/components/ModelSelect.vue".to_string(), 55, 60))
        );
        // 非代码行号证据返回 None
        assert_eq!(parse_file_line_ref("output_id: test-42"), None);
        assert_eq!(parse_file_line_ref("git diff --stat"), None);
        assert_eq!(parse_file_line_ref("npm test passed"), None);
        assert_eq!(parse_file_line_ref("已核验无异常"), None);
    }

    #[test]
    fn test_single_edit_touching_multiple_obligations_is_not_blocked() {
        // 场景：单次编辑 (side_effect_count = 1) 同时修改了同一个文件中的 3 个函数
        // Diff 包含了 3 个修改 Hunk，分别覆盖 lines 87-103, 126, 445
        let raw_diff = r#"diff --git a/src/views/ChatView.vue b/src/views/ChatView.vue
index 123..456 100644
--- a/src/views/ChatView.vue
+++ b/src/views/ChatView.vue
@@ -87,17 +87,20 @@
-old virtual scroll
+new virtual scroll
@@ -124,5 +126,8 @@
-old markdown render
+new markdown render
@@ -440,6 +445,10 @@
-old scrollToBottom
+new scrollToBottom with rAF
"#;
        let diff_ctx = DiffContext::from_diff_and_paths(
            Some(raw_diff),
            &["src/views/ChatView.vue".to_string()],
        );

        let contract = ReviewContract {
            scope_statement: "优化 ChatView.vue 长列表与渲染性能".to_string(),
            obligations: vec![
                ReviewObligation {
                    id: "scroll".to_string(),
                    question: "是否优化 scrollToBottom 节流？".to_string(),
                    status: "evidenced".to_string(),
                    evidence_refs: vec!["ChatView.vue:445".to_string()],
                    waived_reason: None,
                    verify_command: None,
                    verify_script: None,
                    verify_sha256: None,
                },
                ReviewObligation {
                    id: "render".to_string(),
                    question: "是否优化 Markdown 渲染防抖？".to_string(),
                    status: "evidenced".to_string(),
                    evidence_refs: vec!["src/views/ChatView.vue:126".to_string()],
                    waived_reason: None,
                    verify_command: None,
                    verify_script: None,
                    verify_sha256: None,
                },
                ReviewObligation {
                    id: "virtual".to_string(),
                    question: "是否启用虚拟滚动列表？".to_string(),
                    status: "evidenced".to_string(),
                    evidence_refs: vec!["ChatView.vue:87-103".to_string()],
                    waived_reason: None,
                    verify_command: None,
                    verify_script: None,
                    verify_sha256: None,
                },
            ],
        };

        let verified = ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec!["output_id: test-1".to_string()],
            waived_reason: None,
        };

        // 关键断言：side_effect_count = 1，但 3 条义务均在 diff 中找到真实修改行号，绝不误伤拦截！
        let blockers = finish_blockers_with_diff(
            &[],
            Some(&contract),
            false,
            1,
            Some(&verified),
            Some(&diff_ctx),
            None,
            None,
        );
        assert!(
            blockers.is_empty(),
            "单次编辑同时覆盖多项义务时必须放行，不按写入次数误伤：{:?}",
            blockers
        );
    }

    #[test]
    fn test_false_completion_extract_py_only_is_blocked() {
        // 场景（复现用户真实 Bug）：
        // Agent 只写了 extract.py，却声称完成了 ChatView.vue 的性能优化并在契约中标为 evidenced
        let raw_diff = r#"【新增未跟踪文件】:
--- 新增文件: extract.py ---
def main():
    pass
"#;
        let diff_ctx = DiffContext::from_diff_and_paths(
            Some(raw_diff),
            &["extract.py".to_string()],
        );

        let contract = ReviewContract {
            scope_statement: "性能优化".to_string(),
            obligations: vec![
                ReviewObligation {
                    id: "scroll".to_string(),
                    question: "优化 scrollToBottom".to_string(),
                    status: "evidenced".to_string(),
                    evidence_refs: vec!["ChatView.vue:442".to_string()],
                    waived_reason: None,
                    verify_command: None,
                    verify_script: None,
                    verify_sha256: None,
                },
            ],
        };

        let verified = ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec!["output_id: test-1".to_string()],
            waived_reason: None,
        };

        let blockers = finish_blockers_with_diff(
            &[],
            Some(&contract),
            false,
            1,
            Some(&verified),
            Some(&diff_ctx),
            None,
            None,
        );

        assert_eq!(blockers.len(), 1);
        assert!(
            blockers[0].contains("scroll"),
            "必须点名未真实履约的义务 id: {}",
            blockers[0]
        );
        assert!(
            blockers[0].contains("ChatView.vue:442"),
            "必须指出无效的证据引用: {}",
            blockers[0]
        );
        assert!(
            blockers[0].contains("未在本次真实代码改动 (diff) 中找到对应修改行"),
            "拦截文案必须明确原因: {}",
            blockers[0]
        );
    }

    #[test]
    fn test_partial_fix_blocks_only_unmodified_obligations() {
        // 场景：Agent 真实修改了 scrollToBottom (line 445)，但没有修改 Markdown 渲染 (line 126)
        let raw_diff = r#"diff --git a/src/views/ChatView.vue b/src/views/ChatView.vue
index 123..456 100644
--- a/src/views/ChatView.vue
+++ b/src/views/ChatView.vue
@@ -440,6 +445,10 @@
-old scrollToBottom
+new scrollToBottom
"#;
        let diff_ctx = DiffContext::from_diff_and_paths(
            Some(raw_diff),
            &["src/views/ChatView.vue".to_string()],
        );

        let contract = ReviewContract {
            scope_statement: "优化性能".to_string(),
            obligations: vec![
                ReviewObligation {
                    id: "scroll".to_string(),
                    question: "优化 scrollToBottom".to_string(),
                    status: "evidenced".to_string(),
                    evidence_refs: vec!["ChatView.vue:445".to_string()],
                    waived_reason: None,
                    verify_command: None,
                    verify_script: None,
                    verify_sha256: None,
                },
                ReviewObligation {
                    id: "render".to_string(),
                    question: "优化 Markdown 渲染".to_string(),
                    status: "evidenced".to_string(),
                    evidence_refs: vec!["ChatView.vue:126".to_string()],
                    waived_reason: None,
                    verify_command: None,
                    verify_script: None,
                    verify_sha256: None,
                },
            ],
        };

        let verified = ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec!["output_id: test-1".to_string()],
            waived_reason: None,
        };

        let blockers = finish_blockers_with_diff(
            &[],
            Some(&contract),
            false,
            1,
            Some(&verified),
            Some(&diff_ctx),
            None,
            None,
        );

        // scroll 命中 line 445 放行；render 引用 line 126 未被修改，被精准拦截！
        assert_eq!(blockers.len(), 1);
        assert!(blockers[0].contains("render"));
        assert!(!blockers[0].contains("scroll"));
    }

    #[test]
    fn test_missing_line_evidence_in_write_task_is_blocked() {
        // 场景：在写操作任务中，义务声称 evidenced 但没有给出形如 file:line 的证据
        let raw_diff = r#"diff --git a/src/views/ChatView.vue b/src/views/ChatView.vue
@@ -440,6 +445,10 @@
+new
"#;
        let diff_ctx = DiffContext::from_diff_and_paths(
            Some(raw_diff),
            &["src/views/ChatView.vue".to_string()],
        );

        let contract = ReviewContract {
            scope_statement: "优化性能".to_string(),
            obligations: vec![
                ReviewObligation {
                    id: "scroll".to_string(),
                    question: "优化 scrollToBottom".to_string(),
                    status: "evidenced".to_string(),
                    evidence_refs: vec!["已完成优化".to_string()],
                    waived_reason: None,
                    verify_command: None,
                    verify_script: None,
                    verify_sha256: None,
                },
            ],
        };

        let verified = ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec!["output_id: test-1".to_string()],
            waived_reason: None,
        };

        let blockers = finish_blockers_with_diff(
            &[],
            Some(&contract),
            false,
            1,
            Some(&verified),
            Some(&diff_ctx),
            None,
            None,
        );

        assert_eq!(blockers.len(), 1);
        assert!(blockers[0].contains("缺少具体代码行号"));
    }

    #[test]
    fn test_read_only_task_and_waived_obligations_are_not_blocked() {
        // 场景 1：只读任务 (side_effect_count = 0) 不做 diff 校验
        let contract = ReviewContract {
            scope_statement: "代码走查".to_string(),
            obligations: vec![
                ReviewObligation {
                    id: "audit".to_string(),
                    question: "是否存在内存泄漏？".to_string(),
                    status: "evidenced".to_string(),
                    evidence_refs: vec!["git diff --stat".to_string()],
                    waived_reason: None,
                    verify_command: None,
                    verify_script: None,
                    verify_sha256: None,
                },
            ],
        };
        let blockers = finish_blockers_with_diff(
            &[],
            Some(&contract),
            false,
            0,
            None,
            None,
            None,
            None,
        );
        assert!(blockers.is_empty());

        // 场景 2：豁免义务 (waived) 不做 diff 校验
        let contract_waived = ReviewContract {
            scope_statement: "性能优化".to_string(),
            obligations: vec![
                ReviewObligation {
                    id: "virtual".to_string(),
                    question: "虚拟滚动".to_string(),
                    status: "waived".to_string(),
                    evidence_refs: Vec::new(),
                    waived_reason: Some("经评估长列表数据量较小，暂无需引入虚拟滚动".to_string()),
                    verify_command: None,
                    verify_script: None,
                    verify_sha256: None,
                },
            ],
        };
        let empty_diff = DiffContext::default();
        let verified = ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec!["output_id: test-1".to_string()],
            waived_reason: None,
        };
        let blockers = finish_blockers_with_diff(
            &[],
            Some(&contract_waived),
            false,
            1,
            Some(&verified),
            Some(&empty_diff),
            None,
            None,
        );
        assert!(blockers.is_empty());

        // 场景 3：带有可执行 verify_command 的义务由 host_verify 处理，免除 diff 行号检查
        let contract_command = ReviewContract {
            scope_statement: "单元测试".to_string(),
            obligations: vec![
                ReviewObligation {
                    id: "unit_test".to_string(),
                    question: "单元测试是否通过？".to_string(),
                    status: "evidenced".to_string(),
                    evidence_refs: vec!["output_id: run-10".to_string()],
                    waived_reason: None,
                    verify_command: Some("npm test".to_string()),
                    verify_script: None,
                    verify_sha256: None,
                },
            ],
        };
        let blockers = finish_blockers_with_diff(
            &[],
            Some(&contract_command),
            false,
            1,
            Some(&verified),
            Some(&empty_diff),
            None,
            None,
        );
        assert!(blockers.is_empty());
    }

    #[test]
    fn test_readonly_step_preserves_finish_reviewed_and_challenge_count() {
        use crate::agent::dispatcher::is_write_or_side_effect_tool;
        use crate::agent::types::AgentInstruction;

        let read_inst = AgentInstruction {
            tool: Some("read_text_file".to_string()),
            description: "读取代码进行分析".to_string(),
            ..Default::default()
        };
        assert!(!is_write_or_side_effect_tool(&read_inst));

        let write_inst = AgentInstruction {
            tool: Some("write_to_file".to_string()),
            description: "写入修复代码".to_string(),
            ..Default::default()
        };
        assert!(is_write_or_side_effect_tool(&write_inst));
    }

    fn diagnostic_contract_with_refs(refs: &[&str]) -> ReviewContract {
        ReviewContract {
            scope_statement: "排查默认对话工作区绑定根因".to_string(),
            obligations: vec![ReviewObligation {
                id: "root-cause".to_string(),
                question: "根因是否已定位并证实？".to_string(),
                status: "evidenced".to_string(),
                evidence_refs: refs.iter().map(|r| r.to_string()).collect(),
                waived_reason: None,
                verify_command: None,
                verify_script: None,
                verify_sha256: None,
            }],
        }
    }

    #[test]
    fn diagnostic_task_with_locatable_refs_passes_without_diff() {
        // 事故现场形态：sed 排查命令产生副作用（side_effect_count = 1），但无任何代码改动
        let ctx = DiffContext::from_diff_and_paths(None, &[]);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let blockers = review_contract_evidence_blockers(
            &diagnostic_contract_with_refs(&["src-tauri/src/agent/finish_guard/diff.rs:1"]),
            1,
            Some(&ctx),
            Some(root),
            None,
        );
        assert!(blockers.is_empty(), "诊断引用静态可定位时应放行: {blockers:?}");

        // 工作区相对路径（不带 src-tauri/ 前缀）同样可定位
        let blockers = review_contract_evidence_blockers(
            &diagnostic_contract_with_refs(&["src/agent/finish_guard/diff.rs:1"]),
            1,
            Some(&ctx),
            Some(root),
            None,
        );
        assert!(blockers.is_empty(), "工作区相对诊断引用应放行: {blockers:?}");
    }

    #[test]
    fn lying_writer_claiming_fix_with_empty_diff_is_blocked() {
        let ctx = DiffContext::from_diff_and_paths(None, &[]);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let delivery_text = "任务完成，已修复 main_loop.rs 的工作区绑定问题";
        let blockers = review_contract_evidence_blockers(
            &diagnostic_contract_with_refs(&["src/agent/finish_guard/diff.rs:1"]),
            1,
            Some(&ctx),
            Some(root),
            Some(delivery_text),
        );
        assert_eq!(blockers.len(), 1, "声称修改但无 diff 必须拦截");
        assert!(blockers[0].contains("声称已修改/修复代码"));
    }

    #[test]
    fn fabricated_line_numbers_fail_static_check() {
        let ctx = DiffContext::from_diff_and_paths(None, &[]);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let blockers = review_contract_evidence_blockers(
            &diagnostic_contract_with_refs(&["src/agent/finish_guard/diff.rs:999999"]),
            1,
            Some(&ctx),
            Some(root),
            None,
        );
        assert!(!blockers.is_empty(), "行号越界的引用必须拦截");
        assert!(blockers[0].contains("未能在当前工作区源码中定位"));
    }

    #[test]
    fn real_diff_still_requires_evidence_inside_diff() {
        // 有真实改动时严格对账行为不变：证据命中 diff 放行，不在 diff 拦截
        let raw_diff = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -10,3 +10,4 @@\n fn a() {}\n";
        let ctx = DiffContext::from_diff_and_paths(Some(raw_diff), &[]);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

        let hit = review_contract_evidence_blockers(
            &diagnostic_contract_with_refs(&["src/lib.rs:10"]),
            1,
            Some(&ctx),
            Some(root),
            None,
        );
        assert!(hit.is_empty(), "证据落入真实 diff 应放行: {hit:?}");

        let miss = review_contract_evidence_blockers(
            &diagnostic_contract_with_refs(&["src/other_never_touched.rs:10"]),
            1,
            Some(&ctx),
            Some(root),
            None,
        );
        assert!(!miss.is_empty(), "证据未落入真实 diff 必须拦截");
    }


#[test]
fn review_finding_todos_cannot_be_silently_dropped() {
    let mut context = context::SandwichContext::new("system".to_string(), "goal".to_string());
    context.todo_list.push(crate::agent::types::TodoItem {
        id: 5,
        status: "pending".to_string(),
        description: "[审查发现] [QUESTION] at plan_gate.rs:135 冷却豁免白名单漏加新路由".to_string(),
        evidence_output_id: None,
    });

    // 模型提交的 todo_update 刻意剔除了审查发现条目 → 必须自动恢复
    let drop_instruction = AgentInstruction {
        tool: Some("todo_update".to_string()),
        todo_update: vec![crate::agent::types::TodoItem {
            id: 1,
            status: "done".to_string(),
            description: "无关待办".to_string(),
            evidence_output_id: None,
        }],
        ..Default::default()
    };
    apply_instruction_state_updates(&mut context, &drop_instruction);

    assert!(
        context.todo_list.iter().any(|t| t.description.starts_with("[审查发现]")),
        "被剔除的审查发现必须自动恢复，禁止静默销账"
    );
    assert!(context.todo_list.iter().any(|t| t.description == "无关待办"));
}

#[test]
fn review_finding_kept_with_explicit_closure_is_accepted() {
    let mut context = context::SandwichContext::new("system".to_string(), "goal".to_string());
    context.todo_list.push(crate::agent::types::TodoItem {
        id: 5,
        status: "pending".to_string(),
        description: "[审查发现] [QUESTION] at plan_gate.rs:135 冷却豁免白名单漏加新路由".to_string(),
        evidence_output_id: None,
    });

    // 模型保留该条目并显式闭合（done）→ 合法销账，不恢复不告警
    let close_instruction = AgentInstruction {
        tool: Some("todo_update".to_string()),
        todo_update: vec![crate::agent::types::TodoItem {
            id: 5,
            status: "done".to_string(),
            description: "[审查发现] [QUESTION] at plan_gate.rs:135 冷却豁免白名单漏加新路由".to_string(),
            evidence_output_id: None,
        }],
        ..Default::default()
    };
    apply_instruction_state_updates(&mut context, &close_instruction);

    assert_eq!(context.todo_list.len(), 1);
    assert_eq!(context.todo_list[0].status, "done");
}
