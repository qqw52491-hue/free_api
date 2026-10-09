    use super::*;

    /// ── 台账测试辅助：名单退役后，批次/阶段成员身份由 agent_child_runs 行承担 ──
    fn mk_db(tag: &str) -> (std::path::PathBuf, crate::db::DbState) {
        let base = std::env::temp_dir().join(format!("cp-ledger-{}-{}", tag, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let state = crate::db::DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("t.db")).unwrap(),
        )));
        (base, state)
    }

    /// 插一条带 batch/purpose 盖章的子任务台账行；consumed=true 表示回执已被消费
    ///（终态+已取件），false 表示仍在途/待决。
    fn ledger_run(
        db: &crate::db::DbState,
        parent_run_id: &str,
        batch: &str,
        purpose: &str,
        status: &str,
        consumed: bool,
    ) -> String {
        let id = format!("run-{}", uuid::Uuid::new_v4());
        db.0.lock().unwrap().execute(
            "INSERT INTO agent_child_runs (id, parent_run_id, parent_session_id, workspace_root, model_id, status, started_at, kind, task_id, attempt_id, purpose, batch_id, consumed_at)
             VALUES (?1, ?2, 'sess', '/tmp', 'm', ?3, 't', 'review', ?1, 1, ?4, ?5, ?6)",
            rusqlite::params![
                id,
                parent_run_id,
                status,
                purpose,
                batch,
                if consumed { Some("2024-01-01T00:00:00Z".to_string()) } else { None }
            ],
        ).unwrap();
        id
    }

    /// 把在途成员标记为"已终态且已消费"（等价旧名单的 remove/delivered 语义）
    fn ledger_consume(db: &crate::db::DbState, run_id: &str, status: &str) {
        db.0.lock().unwrap().execute(
            "UPDATE agent_child_runs SET status=?1, consumed_at='2024-01-01T00:00:00Z' WHERE id=?2",
            rusqlite::params![status, run_id],
        ).unwrap();
    }

    /// 方案审查批次夹具：n 个在途成员 + checkpoint 锚点
    fn plan_batch_fixture(cp: &mut AgentCheckpoint, db: &crate::db::DbState, batch: &str, n: usize) -> Vec<String> {
        let ids: Vec<String> = (0..n)
            .map(|_| ledger_run(db, &cp.run_id, batch, "plan_review", "running", false))
            .collect();
        cp.set_plan_review_batch(batch, ids.len());
        ids
    }

    #[test]
    fn stale_stage_blocker_clears_when_oracle_passes_birth_stage() {
        // 09-07 pro 终局实测：阶段 07 已过宿主判分，旧 blocker 仍全局拦写（僵尸拦截）。
        // 规则：宿主判分越过 blocker 出生索引 → 自动清账（final_only 记账）；未越过或
        // 无出生索引（旧快照）→ 不动，对质与仲裁仍是唯一出口。
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("run-zombie".to_string(), &context);
        cp.stage_review_blocker_stage = Some(2);
        cp.stage_review_blockers_open = 1;

        assert!(cp.clear_stale_stage_blockers(2).is_none(), "未越过不清");
        assert_eq!(cp.stage_review_blockers_open, 1);

        let note = cp.clear_stale_stage_blockers(3).expect("越过应清账");
        assert_eq!(cp.stage_review_blockers_open, 0);
        assert!(cp.stage_review_blocker_stage.is_none());
        assert!(cp.stage_review_final_only.iter().any(|l| l.contains("僵尸 blocker")));
        assert!(note.contains("僵尸拦截"));

        cp.stage_review_blockers_open = 1;
        cp.stage_review_blocker_stage = None;
        assert!(cp.clear_stale_stage_blockers(9).is_none(), "无出生索引不清");
        assert_eq!(cp.stage_review_blockers_open, 1);
    }

    #[test]
    fn save_checkpoint_conflict_retry_does_not_relock_self() {
        // 回归：09-07 pro 实弹全局冻结案。save_checkpoint 曾在持有 db.0 锁的 scope 内
        // 调 load_checkpoint（同线程二次加锁 std Mutex）→ 永久自死锁，全进程 DB 使用者
        // 排队冻结。本测试走乐观锁冲突重试路径：修复前悬挂，修复后应正常保存。
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE agent_checkpoints (
                run_id TEXT PRIMARY KEY, phase TEXT NOT NULL, snapshot_json TEXT NOT NULL,
                version INTEGER NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);",
        )
        .unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-conflict".to_string(), &context);
        save_checkpoint(&db, &mut checkpoint).unwrap();

        // 模拟另一保存者（如 wait_user 时的仲裁观察器）并发把库中 version 顶到 5。
        {
            let conn = db.0.lock().unwrap();
            conn.execute(
                "UPDATE agent_checkpoints SET version = 5 WHERE run_id = 'run-conflict'",
                [],
            )
            .unwrap();
        }

        // 本方仍持内存 version=1 → UPDATE WHERE version=1 不命中 → 冲突 → 锁外重试。
        save_checkpoint(&db, &mut checkpoint).unwrap();

        let reloaded = load_checkpoint(&db, "run-conflict")
            .unwrap()
            .expect("checkpoint 应存在");
        assert_eq!(reloaded.version, 6, "合并后应基于库中 v5 再写 v6");
    }

    #[test]
    fn plan_gate_projection_mirrors_checkpoint_facts() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-test".to_string(), &context);
        checkpoint.plan_mode = PlanExecutionMode::CrossReview;
        checkpoint.plan_phase = PlanPhase::Reviewing;
        checkpoint.plan_reviewer_retry_count = 1;
        checkpoint.plan_reviewer_consumed = false;

        let proj = checkpoint.plan_gate_projection();
        assert_eq!(proj.mode, "cross_review");
        assert_eq!(proj.phase, "reviewing");
        assert!(proj.reviewer_dispatched);
        assert!(!proj.reviewer_consumed);
        assert_eq!(proj.reviewer_budget_used, 1);
        assert_eq!(proj.reviewer_budget_max, MAX_PLAN_REVIEWER_RETRIES);
        assert_eq!(proj.blocking_count, 0);
        assert!(!proj.batch_incomplete);
        assert!(!proj.budget_exhausted);
    }

    #[test]
    fn plan_gate_projection_reflects_blocking_and_batch_barrier() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-test".to_string(), &context);
        checkpoint.plan_mode = PlanExecutionMode::CrossReview;
        checkpoint.plan_phase = PlanPhase::Reviewing;
        checkpoint.plan_reviewer_retry_count = 2;
        let (_b, db) = mk_db("proj");

        // 模拟批次已派发 2 个审查员（台账行盖批次章），回传 1 个且带有 1 个 blocking
        let r1 = ledger_run(&db, &checkpoint.run_id, "b", "plan_review", "running", false);
        let _r2 = ledger_run(&db, &checkpoint.run_id, "b", "plan_review", "running", false);
        checkpoint.set_plan_review_batch("b", 2);
        ledger_consume(&db, &r1, "succeeded");
        let delivery = crate::agent::child_reviewer::ReviewerDeliveryData {
            findings: vec![crate::agent::child_reviewer::ReviewFinding {
                id: "F1".to_string(),
                severity: "blocking".to_string(),
                finality: "regardless".to_string(),
                location: "src/main.js:10".to_string(),
                current: "window.addEventListener".to_string(),
                why: "内存泄漏隐患".to_string(),
                suggestion: "解除事件监听".to_string(),
                evidence_strength: "confirmed".to_string(),
            }],
            confidence: "high".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: None,
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: None,
            extra: std::collections::HashMap::new(),
        };
        checkpoint.record_reviewer_delivery_for_run(&db, Some(&r1), &delivery);

        let proj = checkpoint.plan_gate_projection();
        assert_eq!(proj.blocking_count, 1);
        assert!(proj.blocking_summary.is_some());
        assert!(proj.blocking_summary.unwrap().contains("[F1] src/main.js:10"));
        assert!(proj.batch_incomplete, "尚有 rev-2 未齐套，batch_incomplete 应为 true");
        assert_eq!(proj.delivered_reviewers, 1);
        assert_eq!(proj.expected_reviewers, 2);
        assert!(proj.budget_exhausted, "已用 2 轮重试，预算应标记耗尽");
    }

    /// 审查员自洽性机械校验：root_cause_verdict=insufficient（审查员自己宣布
    /// 根因修好后症状仍会复现）与 decision=approved 互斥——宿主按 escalate→rebuttal
    /// 同款模式降级，不放行一个自认不成立的根因方向。
    #[test]
    fn insufficient_root_cause_verdict_downgrades_approved_to_rebuttal() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("run-rcv".to_string(), &context);
        cp.plan_mode = PlanExecutionMode::CrossReview;
        cp.plan_phase = PlanPhase::Reviewing;
        let (_b, db) = mk_db("rcv");
        let r1 = ledger_run(&db, &cp.run_id, "b", "plan_review", "running", false);
        cp.set_plan_review_batch("b", 1);

        let mut delivery = crate::agent::child_reviewer::ReviewerDeliveryData {
            findings: vec![],
            confidence: "confirmed".to_string(),
            evidence_refs: vec![],
            missing_evidence: vec![],
            routing: None,
            withdrawn_findings: vec![],
            objection_verdicts: vec![],
            scorecard: None,
            extra: std::collections::HashMap::new(),
        };
        delivery
            .extra
            .insert("decision".to_string(), serde_json::json!("approved"));
        delivery
            .extra
            .insert("root_cause_verdict".to_string(), serde_json::json!("insufficient"));

        cp.record_reviewer_delivery_for_run(&db, Some(&r1), &delivery);
        let stored = cp.plan_review_deliveries.get(&r1).expect("delivery 应入账");
        assert_eq!(
            stored.get_extra_str("decision"),
            Some("rebuttal"),
            "insufficient 与 approved 矛盾必须降级为 rebuttal"
        );
        assert_eq!(
            stored.get_extra_str("original_decision"),
            Some("approved"),
            "原始裁决应留档供复盘"
        );
        assert!(
            stored.scorecard.is_none(),
            "降级为 rebuttal 后不得合成通过评分卡"
        );

        // 对照组：verdict=explains 时 approved 照常生效
        let mut cp2 = AgentCheckpoint::new("run-rcv2".to_string(), &context);
        cp2.plan_mode = PlanExecutionMode::CrossReview;
        cp2.plan_phase = PlanPhase::Reviewing;
        let r2 = ledger_run(&db, &cp2.run_id, "b2", "plan_review", "running", false);
        cp2.set_plan_review_batch("b2", 1);
        let mut ok_delivery = delivery;
        ok_delivery
            .extra
            .insert("root_cause_verdict".to_string(), serde_json::json!("explains"));
        cp2.record_reviewer_delivery_for_run(&db, Some(&r2), &ok_delivery);
        let stored2 = cp2.plan_review_deliveries.get(&r2).unwrap();
        assert_eq!(stored2.get_extra_str("decision"), Some("approved"));
        assert!(
            stored2.scorecard.is_some(),
            "正常 approved 应补齐基线评分卡"
        );
    }

    #[test]
    fn plan_gate_projection_defaults_to_auto_exploring_undispatched() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let checkpoint = AgentCheckpoint::new("run-test".to_string(), &context);

        let proj = checkpoint.plan_gate_projection();
        assert_eq!(proj.mode, "auto");
        assert_eq!(proj.phase, "exploring");
        assert!(!proj.reviewer_dispatched);
        assert!(!proj.reviewer_consumed);
        assert_eq!(proj.reviewer_budget_used, 0);
    }

    fn instruction(action: &str) -> AgentInstruction {
        AgentInstruction {
            action: Some(action.to_string()),
            tool: Some("browser".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn unknown_actions_default_to_human_review() {
        assert_eq!(
            recovery_policy_for(&instruction("submit_order")),
            RecoveryPolicy::HumanReview
        );
    }

    #[test]
    fn readonly_actions_are_safe_to_retry() {
        assert_eq!(
            recovery_policy_for(&instruction("extract")),
            RecoveryPolicy::SafeRetry
        );
        assert_eq!(
            recovery_policy_for(&instruction("get_status")),
            RecoveryPolicy::SafeRetry
        );
    }

    #[test]
    fn pending_call_reuses_a_stable_id_across_recovery_attempts() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-1".to_string(), &context);
        checkpoint.prepare_tool(instruction("submit_order"), &context);
        let before = checkpoint.pending.as_ref().unwrap().call_id.clone();
        checkpoint.begin_recovery_attempt();
        let pending = checkpoint.pending.as_ref().unwrap();

        assert_eq!(pending.call_id, before);
        assert_eq!(pending.attempts, 2);
        assert_eq!(
            recovery_decision(&checkpoint),
            RecoveryDecision::RequireHumanReview
        );
    }

    #[test]
    fn safe_actions_keep_a_prepared_instruction_without_pending() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-1".to_string(), &context);
        checkpoint.prepare_tool(instruction("extract"), &context);

        assert!(checkpoint.pending.is_none());
        assert_eq!(checkpoint.phase, AgentPhase::RunTool);
        assert_eq!(
            checkpoint
                .prepared_instruction
                .as_ref()
                .map(AgentInstruction::get_action),
            Some("extract".to_string())
        );
        assert_eq!(recovery_decision(&checkpoint), RecoveryDecision::ResumeTool);
    }

    #[test]
    fn completed_step_advances_durable_cursor_before_next_planning_cycle() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-1".to_string(), &context);
        checkpoint.prepare_tool(instruction("submit_order"), &context);
        checkpoint.complete_step(7, &context);

        assert_eq!(checkpoint.phase, AgentPhase::CallLlm);
        assert_eq!(checkpoint.step_id, 8);
        assert!(checkpoint.pending.is_none());
        assert_eq!(
            recovery_decision(&checkpoint),
            RecoveryDecision::ResumePlanning
        );
    }

    #[test]
    fn wait_user_without_pending_resumes_waiting() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-1".to_string(), &context);
        checkpoint.wait_for_human("need confirmation".to_string(), &context);

        assert_eq!(checkpoint.phase, AgentPhase::WaitUser);
        assert!(checkpoint.pending.is_none());
        assert_eq!(
            recovery_decision(&checkpoint),
            RecoveryDecision::ResumeWaiting
        );
    }

    #[test]
    fn terminal_phases_never_resume_after_checkpoint_recovery() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut done = AgentCheckpoint::new("done".to_string(), &context);
        done.mark_done(&context);
        assert_eq!(recovery_decision(&done), RecoveryDecision::Terminal);

        let mut failed = AgentCheckpoint::new("failed".to_string(), &context);
        failed.mark_failed("terminal failure".to_string(), &context);
        assert_eq!(recovery_decision(&failed), RecoveryDecision::Terminal);
    }

    #[test]
    fn safe_read_recovery_resumes_the_saved_instruction_without_a_new_llm_call() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-1".to_string(), &context);
        checkpoint.prepare_tool(instruction("extract"), &context);

        assert!(checkpoint.pending.is_none());
        assert_eq!(checkpoint.phase, AgentPhase::RunTool);
        assert!(checkpoint.prepared_instruction.is_some());
        assert_eq!(recovery_decision(&checkpoint), RecoveryDecision::ResumeTool);
    }

    #[test]
    fn llm_instruction_is_persisted_before_tool_execution_and_cleared_on_completion() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-1".to_string(), &context);
        checkpoint.record_instruction(instruction("submit_order"), &context);

        assert_eq!(checkpoint.phase, AgentPhase::RunTool);
        assert!(checkpoint.pending.is_none());
        assert_eq!(recovery_decision(&checkpoint), RecoveryDecision::ResumeTool);

        checkpoint.complete_step(3, &context);
        assert_eq!(checkpoint.phase, AgentPhase::CallLlm);
        assert_eq!(checkpoint.step_id, 4);
        assert!(checkpoint.prepared_instruction.is_none());
        assert_eq!(
            recovery_decision(&checkpoint),
            RecoveryDecision::ResumePlanning
        );
    }

    #[test]
    fn task_store_resource_references_survive_checkpoint_context_recovery() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.set_task_resources(vec![crate::agent::task_store::TaskResourceRef {
            resource_id: "people-dataset".to_string(),
            kind: "people".to_string(),
            label: "候选人资料".to_string(),
            summary: "已持久化 100 人，已补全 42 人".to_string(),
            status: "active".to_string(),
            row_count: 100,
            cursor: Some(serde_json::json!({"enriched": 42})),
            updated_at: "2026-08-14T00:00:00Z".to_string(),
        }]);
        let checkpoint = AgentCheckpoint::new("run-1".to_string(), &context);

        assert_eq!(checkpoint.data.task_resources.len(), 1);
        assert_eq!(checkpoint.data.task_resources[0].row_count, 100);

        let mut restored =
            SandwichContext::new("other-system".to_string(), "other-goal".to_string());
        checkpoint.restore_context(&mut restored);
        assert_eq!(restored.task_resources, checkpoint.data.task_resources);
        assert_eq!(restored.task_resources[0].resource_id, "people-dataset");
        assert_eq!(
            restored.task_resources[0].cursor,
            Some(serde_json::json!({"enriched": 42}))
        );
    }

    #[test]
    fn task_store_upsert_uses_idempotent_pending_recovery() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut instruction = instruction("upsert_records");
        instruction.tool = Some("task_store".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-1".to_string(), &context);

        assert_eq!(
            recovery_policy_for(&instruction),
            RecoveryPolicy::Idempotent
        );
        checkpoint.prepare_tool(instruction, &context);
        assert_eq!(
            checkpoint.pending.as_ref().map(|pending| pending.policy),
            Some(RecoveryPolicy::Idempotent)
        );
        assert_eq!(
            recovery_decision(&checkpoint),
            RecoveryDecision::RetryPending
        );
    }

    #[test]
    fn inherit_filling_keeps_dehydrated_tool_trace_and_new_user_goal() {
        use crate::agent::context::{FailedAttempt, SideEffectRecord};
        use serde_json::json;

        let mut previous =
            SandwichContext::new("system".to_string(), "配置 OpenRouter".to_string());
        previous.turns_history = vec![
            ChatMessage {
                role: "user".to_string(),
                content: json!("配置 OpenRouter"),
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: json!("计划: 配置 | 工具: model_manager | 动作: list_models"),
            },
            ChatMessage {
                role: "user".to_string(),
                content: json!("【执行输出 (v4)】\noutput_id: out_err\n❌ 未知动作 list_models"),
            },
        ];
        previous.failed_attempts.push(FailedAttempt {
            step_id: 1,
            tool: "model_manager".to_string(),
            action: "list_models".to_string(),
            params_fingerprint: String::new(),
            error_summary: "未知动作".to_string(),
            output_id: Some("out_err".to_string()),
        });
        previous.side_effects.push(SideEffectRecord {
            step_id: 4,
            tool: "edit_file".to_string(),
            description: "model_manager.rs 新增 add_platform".to_string(),
            timestamp: "2026-08-24T00:00:00Z".to_string(),
            affected_paths: Vec::new(),
        });
        previous
            .memories
            .insert("platform".to_string(), "OpenRouter".to_string());
        previous.todo_list = vec![crate::agent::types::TodoItem {
            id: 1,
            description: "旧任务待办".to_string(),
            status: "completed".to_string(),
            evidence_output_id: None,
        }];
        let mut done = AgentCheckpoint::new("session-a".to_string(), &previous);
        done.mark_done(&previous);

        let mut next =
            SandwichContext::new("system".to_string(), "为什么 list_models 报错".to_string());
        next.turns_history = vec![
            ChatMessage {
                role: "user".to_string(),
                content: json!("配置 OpenRouter"),
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: json!("OpenRouter 已就绪"),
            },
            ChatMessage {
                role: "user".to_string(),
                content: json!("为什么 list_models 报错"),
            },
        ];
        done.data.inherit_filling_into(&mut next);

        assert_eq!(
            next.user_goal, "配置 OpenRouter",
            "M1 静态锚点跨轮不变，确保 100% 前缀缓存命中"
        );
        // 当前轮目标必须保持为新任务——终审/阶段审查按 effective_goal 判定，
        // 若被旧锚点污染会拿上一轮目标审本轮产物（实测 finish_review 误判 blocking）
        assert_eq!(
            next.current_goal, "为什么 list_models 报错",
            "current_goal 不随 M1 锚点继承，保持本轮新目标"
        );
        assert_eq!(next.effective_goal(), "为什么 list_models 报错");
        assert!(next.todo_list.is_empty(), "新任务不继承旧 todo");
        assert_eq!(
            next.memories.get("platform").map(String::as_str),
            Some("OpenRouter")
        );
        assert_eq!(next.failed_attempts.len(), 1);
        assert_eq!(next.failed_attempts[0].action, "list_models");
        assert_eq!(
            next.failed_attempts[0].output_id.as_deref(),
            Some("out_err")
        );
        assert_eq!(next.side_effects.len(), 1);
        assert!(next
            .turns_history
            .iter()
            .any(|m| m.content.as_str()
                == Some("计划: 配置 | 工具: model_manager | 动作: list_models")));
        assert!(next
            .turns_history
            .iter()
            .any(|m| m.content.as_str() == Some("为什么 list_models 报错")));
        assert_eq!(
            next.turns_history
                .iter()
                .filter(|m| m.content.as_str() == Some("配置 OpenRouter"))
                .count(),
            1,
            "已在夹心里的 user 消息不重复追加"
        );
    }

    #[test]
    fn old_checkpoint_without_finish_reviewed_defaults_to_false() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let checkpoint = AgentCheckpoint::new("run-1".to_string(), &context);
        let mut legacy = serde_json::to_value(checkpoint).unwrap();
        legacy.as_object_mut().unwrap().remove("finish_reviewed");

        let restored: AgentCheckpoint = serde_json::from_value(legacy).unwrap();
        assert!(!restored.finish_reviewed);
    }

    #[test]
    fn review_contract_persists_on_restore_but_does_not_cross_new_task_boundary() {
        let mut source = SandwichContext::new("system".to_string(), "审查上下文".to_string());
        source.review_contract = Some(ReviewContract {
            scope_statement: "核验终态恢复".to_string(),
            obligations: vec![crate::agent::review_contract::ReviewObligation {
                verify_command: None,
                verify_script: None,
                verify_sha256: None,
                closure_form: None,
                id: "restore".to_string(),
                question: "checkpoint 是否恢复审查契约？".to_string(),
                status: "evidenced".to_string(),
                evidence_refs: vec!["checkpoint.rs:168-204".to_string()],
                waived_reason: None,
            }],
        });
        let checkpoint = AgentCheckpoint::new("run-contract".to_string(), &source);

        let mut resumed = SandwichContext::new("system".to_string(), "resume".to_string());
        checkpoint.data.restore_into(&mut resumed);
        assert_eq!(resumed.review_contract, source.review_contract);

        let mut legacy = serde_json::to_value(&checkpoint).unwrap();
        legacy
            .get_mut("data")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap()
            .remove("review_contract");
        let restored_legacy: AgentCheckpoint = serde_json::from_value(legacy).unwrap();
        assert!(restored_legacy.data.review_contract.is_none());

        let mut next_task = SandwichContext::new("system".to_string(), "新任务".to_string());
        checkpoint.data.inherit_filling_into(&mut next_task);
        assert!(next_task.review_contract.is_none());
    }
    #[test]
    fn change_verification_persists_on_restore_but_does_not_cross_new_task_boundary() {
        let mut source = SandwichContext::new("system".to_string(), "修改上下文".to_string());
        source.record_side_effect(1, "edit_file".to_string(), "更新配置".to_string());
        source.change_verification = Some(ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec!["output_id: cargo-test-42".to_string()],
            waived_reason: None,
        });
        let checkpoint = AgentCheckpoint::new("run-verification".to_string(), &source);
        let mut resumed = SandwichContext::new("system".to_string(), "resume".to_string());
        checkpoint.data.restore_into(&mut resumed);
        assert_eq!(resumed.change_verification, source.change_verification);
        let mut legacy = serde_json::to_value(&checkpoint).unwrap();
        legacy
            .get_mut("data")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap()
            .remove("change_verification");
        let restored_legacy: AgentCheckpoint = serde_json::from_value(legacy).unwrap();
        assert!(restored_legacy.data.change_verification.is_none());
        let mut next_task = SandwichContext::new("system".to_string(), "新任务".to_string());
        checkpoint.data.inherit_filling_into(&mut next_task);
        assert!(next_task.change_verification.is_none());
    }

    #[test]
    fn evidence_ledger_and_run_tokens_survive_restore_but_not_new_task() {
        let mut source = SandwichContext::new("system".to_string(), "调查上传链路".to_string());
        source.note_evidence_receipt(
            12,
            &crate::agent::types::AgentInstruction {
                tool: Some("local_file".to_string()),
                action: Some("read".to_string()),
                command: Some(serde_json::json!({
                    "path": "src/views/ChatView.vue", "start_line": 1, "end_line": 120
                })),
                ..Default::default()
            },
            "out-abc",
        );
        source.run_prompt_tokens = 1_234_567;
        source.run_completion_tokens = 45_678;
        let checkpoint = AgentCheckpoint::new("run-ledger".to_string(), &source);

        // 崩溃恢复：台账与累计用量原样回来——脱水后取回原文的索引不能丢
        let mut resumed = SandwichContext::new("system".to_string(), "resume".to_string());
        checkpoint.data.restore_into(&mut resumed);
        assert_eq!(resumed.evidence_ledger.len(), 1);
        let e = resumed
            .evidence_ledger
            .get("read|src/views/ChatView.vue|L1-120")
            .expect("台账条目应跨 checkpoint 保留");
        assert_eq!(e.output_id.as_deref(), Some("out-abc"));
        assert_eq!(resumed.run_prompt_tokens, 1_234_567);
        assert_eq!(resumed.run_completion_tokens, 45_678);

        // 旧 checkpoint 缺字段 → 默认空，不炸
        let mut legacy = serde_json::to_value(&checkpoint).unwrap();
        legacy
            .get_mut("data")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap()
            .remove("evidence_ledger");
        let restored_legacy: AgentCheckpoint = serde_json::from_value(legacy).unwrap();
        assert!(restored_legacy.data.evidence_ledger.is_empty());

        // 新任务不继承：台账标的是「本任务」足迹，跨任务带入会误导
        let mut next_task = SandwichContext::new("system".to_string(), "新任务".to_string());
        checkpoint.data.inherit_filling_into(&mut next_task);
        assert!(next_task.evidence_ledger.is_empty());
        assert_eq!(next_task.run_prompt_tokens, 0);
    }

    #[test]
    fn test_question_deduplication() {
        let context = SandwichContext::new("system".to_string(), "查天气".to_string());
        let mut cp = AgentCheckpoint::new("run-dedup".to_string(), &context);

        // 初始无问题：不重复
        assert!(cp.check_duplicate_question("请问你想查询哪个城市的天气？").is_none());

        // 记录第一个提问
        cp.record_asked_question("请问你想查询哪个城市的天气？");
        assert_eq!(cp.asked_human_questions.len(), 1);

        // 1. 完全相同的问题：拦截
        let dup1 = cp.check_duplicate_question("请问你想查询哪个城市的天气？");
        assert!(dup1.is_some());
        assert!(dup1.unwrap().contains("重复提问拦截"));

        // 2. 标点符号与空格变种：拦截
        let dup2 = cp.check_duplicate_question("  请问你想查询哪个城市的天气!  ");
        assert!(dup2.is_some());

        // 3. 口癖轻微变种（包含关系且差值纯为语气助词）：拦截
        let dup3 = cp.check_duplicate_question("请问你想查询哪个城市的天气呢？");
        assert!(dup3.is_some());

        // 4. 同模板不同槽位提问（城市 vs 日期）：绝不误伤，必须放行！
        assert!(cp.check_duplicate_question("您想查询哪个日期的天气？").is_none());
        assert!(cp.check_duplicate_question("请问你想查询哪个日期的天气？").is_none());

        // 5. 相似指标/参数槽位提问（指标A vs 指标B）：放行
        assert!(cp.check_duplicate_question("请提供要分析的指标A").is_none());
        cp.record_asked_question("请提供要分析的指标A");
        assert!(cp.check_duplicate_question("请提供要分析的指标B").is_none());

        // 6. 提问配额约束（全局封顶，可配默认 5 次）
        assert!(cp.check_inquiry_quota_exceeded(DEFAULT_MAX_HUMAN_INQUIRY_BUDGET).is_none());
        cp.record_asked_question("问题3");
        cp.record_asked_question("问题4");
        cp.record_asked_question("问题5");
        assert_eq!(cp.asked_human_questions.len(), 5);
        let quota_err = cp.check_inquiry_quota_exceeded(DEFAULT_MAX_HUMAN_INQUIRY_BUDGET);
        assert!(quota_err.is_some());
        assert!(quota_err.unwrap().contains("信息提问配额已耗尽"));

        // 7. 凭据类提问识别（豁免配额熔断）
        assert!(is_credential_inquiry_kind("missing_credential"));
        assert!(is_credential_inquiry_kind("credential"));
        assert!(is_credential_inquiry_kind("SECRET"));
        assert!(is_credential_inquiry_kind("api_key"));
        assert!(is_credential_inquiry_kind("password"));
        assert!(!is_credential_inquiry_kind("missing_parameter"));
        assert!(!is_credential_inquiry_kind("text"));
        assert!(!is_credential_inquiry_kind("plan_approval"));
    }

    #[test]
    fn plan_reviewer_dispatch_snapshot_roundtrip_and_defaults() {
        let ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-run-snap".to_string(), &ctx);
        assert!(cp.plan_reviewer_dispatch_snapshot().is_none());

        // 1. 写入强类型快照
        let snap = crate::agent::child_reviewer::PlanReviewerDispatchSnapshot::new(
            "gemini-2.5-pro",
            "pro",
            "high",
            "code_reviewer",
            serde_json::json!({"question": "check bounds"}),
        );
        cp.set_plan_reviewer_dispatch_snapshot(&snap);

        // 2. 验证底层存储为合法 JSON 字符串
        let raw_str = cp.plan_reviewer_last_dispatch.as_ref().unwrap();
        assert!(raw_str.contains("gemini-2.5-pro"));

        // 3. 读取强类型快照无损恢复
        let recovered = cp.plan_reviewer_dispatch_snapshot().unwrap();
        assert_eq!(recovered.model, "gemini-2.5-pro");
        assert_eq!(recovered.tier, "pro");
        assert_eq!(recovered.effort, "high");
        assert_eq!(recovered.persona, "code_reviewer");
        assert_eq!(recovered.params["question"], "check bounds");

        // 4. 兼容老版本/缺省字段：tier 和 effort 缺省时自动补齐默认值
        cp.plan_reviewer_last_dispatch = Some(r#"{"model":"gemini-flash"}"#.to_string());
        let legacy = cp.plan_reviewer_dispatch_snapshot().unwrap();
        assert_eq!(legacy.model, "gemini-flash");
        assert_eq!(legacy.tier, "flash");
        assert_eq!(legacy.effort, "high");

        // 5. 损坏 JSON 安全降级返回 None，绝不 panic
        cp.plan_reviewer_last_dispatch = Some(r#"{"corrupted json"#.to_string());
        assert!(cp.plan_reviewer_dispatch_snapshot().is_none());
    }

    #[test]
    fn test_is_stage_review_target_matches_completed_runs() {
        // 名单退役版：归属由台账 purpose/batch_id 推导——消费与否不影响目标识别
        //（purpose 盖章是永久的，优于旧 completed_run_ids 的 20 条滚动窗口）
        let ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-run-target".to_string(), &ctx);
        let (_base, db) = mk_db("stage-target");
        let e1 = ledger_run(&db, &cp.run_id, "sb", "stage_review", "running", false);
        let e2 = ledger_run(&db, &cp.run_id, "sb", "stage_review", "running", false);
        let other = ledger_run(&db, &cp.run_id, "other", "plan_review", "running", false);
        cp.stage_review_batch_id = Some("sb".to_string());

        assert!(cp.is_stage_review_target(&db, &e1));
        assert!(cp.is_stage_review_target(&db, &e2));
        assert!(!cp.is_stage_review_target(&db, &other));

        // expert-1 交付消费后仍是阶段审查目标（purpose 不随消费消失）
        ledger_consume(&db, &e1, "succeeded");
        assert!(cp.is_stage_review_target(&db, &e1));

        // 对质任务：purpose='rebuttal' 盖章即被识别为阶段审查目标
        let reb = ledger_run(&db, &cp.run_id, "sb", "rebuttal", "running", false);
        assert!(cp.is_stage_review_target(&db, &reb));
        let _ = std::fs::remove_dir_all(_base);
    }

    #[test]
    fn test_confrontation_maintained_isolated_per_expert_and_safe_deduction() {
        let ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-run-isolate".to_string(), &ctx);
        cp.stage_review_blockers_open = 2;

        let blocker_a = vec!["F1: 专家 A 的问题".to_string()];
        let blocker_b = vec!["F2: 专家 B 的问题".to_string()];

        // 专家 A 对质失败第 1 次
        assert!(!cp.note_stage_review_confrontation_maintained_for(Some("expert-a"), blocker_a.clone(), 2));
        assert_eq!(cp.stage_review_confrontation_failures, 1);
        assert_eq!(cp.stage_review_confrontation_target.as_deref(), Some("expert-a"));

        // 专家 B 对质失败第 1 次：隔离重置，不借用专家 A 的失败计数！
        assert!(!cp.note_stage_review_confrontation_maintained_for(Some("expert-b"), blocker_b.clone(), 2));
        assert_eq!(cp.stage_review_confrontation_failures, 1);
        assert_eq!(cp.stage_review_confrontation_target.as_deref(), Some("expert-b"));

        // 专家 B 对质失败第 2 次：达到阈值 2，触发专家 B 降级！
        assert!(cp.note_stage_review_confrontation_maintained_for(Some("expert-b"), blocker_b.clone(), 2));
        // 安全扣减：只扣减专家 B 降级的 1 条，总数 2 - 1 = 1，专家 A 的 blocker 绝不被清除！
        assert_eq!(cp.stage_review_blockers_open, 1);
        assert_eq!(cp.stage_review_final_only.len(), 1);
        assert!(cp.stage_review_final_only[0].contains("专家 B"));
    }

    #[test]
    fn test_human_waiver_recording_and_text_aggregation() {
        let ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-waiver".to_string(), &ctx);
        assert!(cp.human_waiver_text().is_none());

        cp.record_human_waiver("保留本地 if(true) 调试放行，直接交付");
        assert!(cp.human_waiver_text().is_some());
        let text = cp.human_waiver_text().unwrap();
        assert!(text.contains("if(true)"));

        // 重复记录幂等去重
        cp.record_human_waiver("保留本地 if(true) 调试放行，直接交付");
        assert_eq!(cp.human_waivers.len(), 1);

        // 与 approved_plan_summary 联动聚合
        cp.approved_plan_summary = Some("用户批准了案件导出方案".to_string());
        let full_text = cp.human_waiver_text().unwrap();
        assert!(full_text.contains("方案裁决: 用户批准了案件导出方案"));
        assert!(full_text.contains("人工特赦: 保留本地 if(true) 调试放行，直接交付"));
    }

    #[test]
    fn test_git_destructive_confirm_lifecycle() {
        let ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-git-confirm".to_string(), &ctx);
        let fp = "git checkout -- scripts/eval_runner.mjs";

        // 1. 挂起登记（幂等）且未确认
        cp.block_git_destructive(fp);
        cp.block_git_destructive(fp);
        assert_eq!(cp.git_destructive_pending.len(), 1);
        assert!(!cp.is_git_destructive_confirmed(fp));

        // 2. ask_human 问题未引用命令原文 → 不算问过（防串线误确认）
        cp.git_destructive_mark_asked("把无关文件清理一下可以吗");
        assert!(cp.git_destructive_asked.is_empty());
        let n = cp.git_destructive_resolve_reply("可以");
        assert_eq!(n, 0);
        assert!(!cp.is_git_destructive_confirmed(fp));
        assert_eq!(cp.git_destructive_pending.len(), 1); // 未问到的 pending 保留

        // 3. 问题原文引用命令 + 明确同意 → 核销；反问/非表态回复不算确认
        cp.git_destructive_mark_asked("需要执行 `git checkout -- scripts/eval_runner.mjs` 清理无关改动，可以吗？");
        assert_eq!(cp.git_destructive_asked.len(), 1);
        let n = cp.git_destructive_resolve_reply("为什么要删这个文件？");
        assert_eq!(n, 0);
        assert!(!cp.is_git_destructive_confirmed(fp));
        assert_eq!(cp.git_destructive_pending.len(), 1); // 未表态 → 仍 pending
        // 再问一次（问题原文引用）+ 明确同意 → 核销
        cp.git_destructive_mark_asked("需要执行 `git checkout -- scripts/eval_runner.mjs`，请确认");
        let n = cp.git_destructive_resolve_reply("可以，删吧");
        assert_eq!(n, 1);
        assert!(cp.is_git_destructive_confirmed(fp));
        assert!(cp.git_destructive_pending.is_empty());

        // 4. 拒绝回复 → asked 从 pending 移除（死单），不产生确认
        let fp2 = "git reset --hard HEAD~1";
        cp.block_git_destructive(fp2);
        cp.git_destructive_mark_asked(&format!("确认执行 `{}`？", fp2));
        let n = cp.git_destructive_resolve_reply("不行，别动那个文件");
        assert_eq!(n, 0);
        assert!(!cp.is_git_destructive_confirmed(fp2));
        assert!(cp.git_destructive_pending.is_empty());
    }

    #[test]
    fn test_plan_review_batch_barrier_incomplete_blocks_consumed() {
        use crate::agent::child_reviewer::{ReviewScorecard, ReviewerDeliveryData};

        let ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-barrier".to_string(), &ctx);
        cp.plan_mode = PlanExecutionMode::CrossReview;
        let (_base, db) = mk_db("barrier");
        // 台账批次：两名在途审查员
        let batch_ids = plan_batch_fixture(&mut cp, &db, "batch-1", 2);

        assert!(!cp.plan_review_batch_complete(&db));
        assert_eq!(cp.plan_review_pending_count(&db), 2);
        assert!(!cp.plan_reviewer_consumed);

        // 交付 1：安全审查交付
        let del1 = ReviewerDeliveryData {
            findings: Vec::new(),
            confidence: "high".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: None,
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: Some(ReviewScorecard {
                correctness: 90,
                risk: 95,
                style: 85,
                test_coverage: Some(80),
                null_reason: None,
                synthesized: false,
            }),
            extra: std::collections::HashMap::new(),
        };
        // 交付 1：台账消费 + reducer 记账
        ledger_consume(&db, &batch_ids[0], "succeeded");
        cp.record_reviewer_delivery_for_run(&db, Some(&batch_ids[0]), &del1);

        // 齐套屏障拦截：未全部到齐，不得放行 consumed
        assert!(!cp.plan_review_batch_complete(&db));
        assert_eq!(cp.plan_review_pending_count(&db), 1);
        assert!(!cp.plan_reviewer_consumed);

        // 交付 2：架构审查交付
        let del2 = ReviewerDeliveryData {
            findings: Vec::new(),
            confidence: "high".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: None,
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: Some(ReviewScorecard {
                correctness: 80,
                risk: 85,
                style: 95,
                test_coverage: Some(90),
                null_reason: None,
                synthesized: false,
            }),
            extra: std::collections::HashMap::new(),
        };
        ledger_consume(&db, &batch_ids[1], "succeeded");
        cp.record_reviewer_delivery_for_run(&db, Some(&batch_ids[1]), &del2);

        // 全部齐套：屏障解除，consumed 置为 true
        assert!(cp.plan_review_batch_complete(&db));
        assert_eq!(cp.plan_review_pending_count(&db), 0);
        assert!(cp.plan_reviewer_consumed);

        // 验证现场真聚合打分：（90+80)/2=85, (95+85)/2=90, (85+95)/2=90, (80+90)/2=85
        let sc = cp.last_review_scorecard.expect("scorecard should exist");
        assert_eq!(sc.correctness, 85);
        assert_eq!(sc.risk, 90);
        assert_eq!(sc.style, 90);
        assert_eq!(sc.test_coverage, Some(85));
        let _ = std::fs::remove_dir_all(_base);
    }

    #[test]
    fn test_plan_review_batch_idempotency_ignores_replays() {
        use crate::agent::child_reviewer::{ReviewFinding, ReviewScorecard, ReviewerDeliveryData};

        let ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-idempotent".to_string(), &ctx);
        let (_base, db) = mk_db("idem");
        let batch_ids = plan_batch_fixture(&mut cp, &db, "b-idem", 1);
        let rev1 = batch_ids[0].clone();

        let del = ReviewerDeliveryData {
            findings: vec![ReviewFinding {
                id: "F1".to_string(),
                severity: "blocking".to_string(),
                finality: "regardless".to_string(),
                location: "src/main.rs:10".to_string(),
                current: "foo".to_string(),
                why: "fatal bug".to_string(),
                suggestion: "fix bug".to_string(),
                evidence_strength: "confirmed".to_string(),
            }],
            confidence: "high".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: None,
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: Some(ReviewScorecard {
                correctness: 50,
                risk: 40,
                style: 60,
                test_coverage: None,
                null_reason: None,
                synthesized: false,
            }),
            extra: std::collections::HashMap::new(),
        };

        cp.record_reviewer_delivery_for_run(&db, Some(&rev1), &del);
        assert_eq!(cp.plan_review_deliveries.len(), 1);
        assert_eq!(cp.last_review_blocking_count, 1);
        assert_eq!(cp.review_objection_added, 1);

        // 重复投递同一 run_id：幂等保护直接忽略，不重复累加
        cp.record_reviewer_delivery_for_run(&db, Some(&rev1), &del);
        assert_eq!(cp.plan_review_deliveries.len(), 1);
        assert_eq!(cp.last_review_blocking_count, 1);
        assert_eq!(cp.review_objection_added, 1);
        let _ = std::fs::remove_dir_all(_base);
    }

    #[test]
    fn test_plan_review_batch_live_aggregation_unions_blockings_and_respects_withdrawn() {
        use crate::agent::child_reviewer::{ReviewFinding, ReviewerDeliveryData};

        let ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-union".to_string(), &ctx);
        let (_base, db) = mk_db("union");
        let batch_ids = plan_batch_fixture(&mut cp, &db, "b-union", 2);

        // 专家 1 提出 SEC-1 (blocking)
        let del1 = ReviewerDeliveryData {
            findings: vec![ReviewFinding {
                id: "SEC-1".to_string(),
                severity: "blocking".to_string(),
                finality: "regardless".to_string(),
                location: "sec.rs".to_string(),
                current: "a".to_string(),
                why: "xss".to_string(),
                suggestion: "escape".to_string(),
                evidence_strength: "confirmed".to_string(),
            }],
            confidence: "high".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: None,
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: None,
            extra: std::collections::HashMap::new(),
        };
        cp.record_reviewer_delivery_for_run(&db, Some(&batch_ids[0]), &del1);

        // 专家 2 提出 ARCH-1 (blocking), ARCH-2 (blocking 但已撤回), ARCH-3 (advisory 非 blocking)
        let del2 = ReviewerDeliveryData {
            findings: vec![
                ReviewFinding {
                    id: "ARCH-1".to_string(),
                    severity: "blocking".to_string(),
                    finality: "regardless".to_string(),
                    location: "arch.rs".to_string(),
                    current: "b".to_string(),
                    why: "perf bottleneck".to_string(),
                    suggestion: "index".to_string(),
                    evidence_strength: "confirmed".to_string(),
                },
                ReviewFinding {
                    id: "ARCH-2".to_string(),
                    severity: "blocking".to_string(),
                    finality: "regardless".to_string(),
                    location: "arch.rs".to_string(),
                    current: "c".to_string(),
                    why: "lock contention".to_string(),
                    suggestion: "rwlock".to_string(),
                    evidence_strength: "confirmed".to_string(),
                },
                ReviewFinding {
                    id: "ARCH-3".to_string(),
                    severity: "advisory".to_string(),
                    finality: "regardless".to_string(),
                    location: "arch.rs".to_string(),
                    current: "d".to_string(),
                    why: "naming style".to_string(),
                    suggestion: "rename".to_string(),
                    evidence_strength: "confirmed".to_string(),
                },
            ],
            confidence: "high".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: None,
            withdrawn_findings: vec!["ARCH-2".to_string()], // ARCH-2 已被撤回
            objection_verdicts: Vec::new(),
            scorecard: None,
            extra: std::collections::HashMap::new(),
        };
        cp.record_reviewer_delivery_for_run(&db, Some(&batch_ids[1]), &del2);

        // 现场真聚合检查：
        // blocking 并集应当包含 SEC-1 和 ARCH-1，不包含已撤回的 ARCH-2，也不包含 advisory 的 ARCH-3
        let blockings = cp.aggregate_unwithdrawn_blockings();
        assert_eq!(blockings.len(), 2);
        let ids: Vec<&str> = blockings.iter().map(|b| b.id.as_str()).collect();
        assert!(ids.contains(&"SEC-1"));
        assert!(ids.contains(&"ARCH-1"));
        assert_eq!(cp.last_review_blocking_count, 2);
        let _ = std::fs::remove_dir_all(_base);
    }

    /// Tech Lead 定稿权：approved + revised_* 时 submitted_plan 被审查者版本覆盖，
    /// plan_finalized_by 留痕，修订的 target_files 同步为批准范围。
    #[test]
    fn tech_lead_revised_plan_overrides_submission_on_approved() {
        use crate::agent::child_reviewer::{ReviewScorecard, ReviewerDeliveryData};
        use crate::agent::types::PlanSubmission;

        let ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-revised".to_string(), &ctx);
        let (_base, db) = mk_db("revised");
        let batch_ids = plan_batch_fixture(&mut cp, &db, "b-rev", 1);
        cp.submitted_plan = Some(PlanSubmission {
            plan_summary: "原始方案：直接改 workspace_dir".to_string(),
            root_cause: "原始根因".to_string(),
            target_files: vec!["src/a.rs".to_string()],
            evidence_refs: vec!["src/a.rs".to_string()],
            assumptions: Vec::new(),
            open_questions: Vec::new(),
            intended_verification: "cargo test".to_string(),
            risk_level: "medium".to_string(),
            revision_reason: String::new(),
            ..Default::default()
        });

        let mut extra = std::collections::HashMap::new();
        extra.insert("decision".to_string(), serde_json::json!("approved"));
        extra.insert(
            "revised_plan_summary".to_string(),
            serde_json::json!("修订方案：新增会话 tmp 缓冲目录而非复用 workspace_dir"),
        );
        extra.insert(
            "revised_target_files".to_string(),
            serde_json::json!(["src/b.rs", "src/c.rs"]),
        );
        let del = ReviewerDeliveryData {
            findings: Vec::new(),
            confidence: "confirmed".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: None,
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: Some(ReviewScorecard {
                correctness: 90,
                risk: 90,
                style: 90,
                test_coverage: Some(80),
                null_reason: None,
                synthesized: false,
            }),
            extra,
        };
        cp.record_reviewer_delivery_for_run(&db, Some(&batch_ids[0]), &del);

        let plan = cp.submitted_plan.as_ref().expect("submitted_plan 必须存在");
        assert_eq!(
            plan.plan_summary,
            "修订方案：新增会话 tmp 缓冲目录而非复用 workspace_dir"
        );
        assert_eq!(plan.root_cause, "原始根因", "未修订字段保持提交方版本");
        assert_eq!(plan.target_files, vec!["src/b.rs", "src/c.rs"]);
        assert_eq!(cp.plan_finalized_by.as_deref(), Some("tech_lead"));
        assert_eq!(
            cp.approved_target_files.as_deref(),
            Some(&["src/b.rs".to_string(), "src/c.rs".to_string()][..]),
            "修订的 target_files 必须同步为批准范围"
        );
    }

    /// approved 未携带 revised_* 时 submitted_plan 原样保留、无定稿标记。
    #[test]
    fn approved_without_revision_keeps_submission() {
        use crate::agent::child_reviewer::{ReviewScorecard, ReviewerDeliveryData};
        use crate::agent::types::PlanSubmission;

        let ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-norev".to_string(), &ctx);
        let (_base, db) = mk_db("norev");
        let batch_ids = plan_batch_fixture(&mut cp, &db, "b-norev", 1);
        cp.submitted_plan = Some(PlanSubmission {
            plan_summary: "原始方案".to_string(),
            ..Default::default()
        });

        let mut extra = std::collections::HashMap::new();
        extra.insert("decision".to_string(), serde_json::json!("approved"));
        let del = ReviewerDeliveryData {
            findings: Vec::new(),
            confidence: "confirmed".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: None,
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: Some(ReviewScorecard {
                correctness: 90,
                risk: 90,
                style: 90,
                test_coverage: Some(80),
                null_reason: None,
                synthesized: false,
            }),
            extra,
        };
        cp.record_reviewer_delivery_for_run(&db, Some(&batch_ids[0]), &del);

        assert_eq!(
            cp.submitted_plan.as_ref().unwrap().plan_summary,
            "原始方案"
        );
        assert!(cp.plan_finalized_by.is_none());
        let _ = std::fs::remove_dir_all(_base);
    }

    /// rebuttal 携带 revised_* 不生效——修订只在 approved 定稿语义下应用。
    #[test]
    fn rebuttal_revision_is_ignored() {
        use crate::agent::child_reviewer::{ReviewScorecard, ReviewerDeliveryData};
        use crate::agent::types::PlanSubmission;

        let ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-rebuttal".to_string(), &ctx);
        let (_base, db) = mk_db("rebuttal");
        let batch_ids = plan_batch_fixture(&mut cp, &db, "b-reb", 1);
        cp.submitted_plan = Some(PlanSubmission {
            plan_summary: "原始方案".to_string(),
            ..Default::default()
        });

        let mut extra = std::collections::HashMap::new();
        extra.insert("decision".to_string(), serde_json::json!("rebuttal"));
        extra.insert(
            "revised_plan_summary".to_string(),
            serde_json::json!("不该被应用的修订"),
        );
        let del = ReviewerDeliveryData {
            findings: Vec::new(),
            confidence: "confirmed".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: None,
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: Some(ReviewScorecard {
                correctness: 70,
                risk: 80,
                style: 90,
                test_coverage: Some(80),
                null_reason: None,
                synthesized: false,
            }),
            extra,
        };
        cp.record_reviewer_delivery_for_run(&db, Some(&batch_ids[0]), &del);

        assert_eq!(
            cp.submitted_plan.as_ref().unwrap().plan_summary,
            "原始方案",
            "rebuttal 下的修订字段不得生效"
        );
        assert!(cp.plan_finalized_by.is_none());
        let _ = std::fs::remove_dir_all(_base);
    }

    /// ── 名单退役版语义验证：成员/在途/齐套全部由台账推导 ──
    /// 旧版死锁形态（名单挂着已死成员永远等不到交付）结构性消失：
    /// 终态+已消费的行天然出列，failover 备胎盖章自动入批，
    /// "忘了换名"的漂移面不复存在。
    #[test]
    fn plan_batch_membership_derives_from_ledger() {
        let ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("run-ledger".to_string(), &ctx);
        let (_base, db) = mk_db("plan-ledger");

        // 批次 batch-x：1 死（终态已消费）+ 1 回执在信箱（终态未消费）+ 1 在跑
        ledger_run(&db, &cp.run_id, "batch-x", "plan_review", "timed_out", true);
        ledger_run(&db, &cp.run_id, "batch-x", "plan_review", "succeeded", false);
        ledger_run(&db, &cp.run_id, "batch-x", "plan_review", "running", false);
        cp.set_plan_review_batch("batch-x", 3);

        // 待决=2（终态未消费+在途），已消费的死者天然不算待决
        assert_eq!(cp.plan_review_pending_count(&db), 2);
        assert_eq!(cp.plan_review_member_count(&db), 3);
        assert!(!cp.plan_review_batch_complete(&db));
        cp.refresh_consumed_flags(&db);
        assert!(!cp.plan_reviewer_consumed, "仍有真实待办成员，不得放行");

        // failover 备胎：同 batch_id 盖章 → 自动成为批次待决成员，无需登记
        let _spare = ledger_run(&db, &cp.run_id, "batch-x", "plan_review", "running", false);
        assert_eq!(cp.plan_review_pending_count(&db), 3, "备胎自动入批待决");

        // 其余成员全部消费后，备胎也交付 → 齐套
        db.0.lock().unwrap().execute(
            "UPDATE agent_child_runs SET consumed_at='x' WHERE batch_id='batch-x'", [],
        ).unwrap();
        assert!(cp.plan_review_batch_complete(&db));
        cp.refresh_consumed_flags(&db);
        assert!(cp.plan_reviewer_consumed, "全员消费后屏障放行");

        // 对质续跑入批（新行同 batch、purpose=rebuttal、未消费）
        // → consumed 自动回落 false——旧名单时代对质不在名单里，
        //   标志恒 true 会掩盖在途工作
        ledger_run(&db, &cp.run_id, "batch-x", "rebuttal", "running", false);
        cp.refresh_consumed_flags(&db);
        assert!(!cp.plan_reviewer_consumed, "对质续跑入批后批次重新待决");

        // 迟到回执属旧批次 → 不碰当前批状态（批次归属校验）
        cp.set_plan_review_batch("batch-y", 1);
        let old_member = ledger_run(&db, &cp.run_id, "batch-x", "plan_review", "running", false);
        cp.record_reviewer_failure_for_run(&db, &old_member);
        assert!(!cp.plan_reviewer_consumed, "旧批次回执不得放行当前批");
        let _ = std::fs::remove_dir_all(_base);
    }

    #[test]
    fn stage_review_in_flight_derives_from_ledger() {
        // 阶段审查同款：在途 = 批次行 consumed_at IS NULL。
        // 孤儿 id 无法存在——没有名单可挂，台账里没有的行根本不会被算作成员。
        let ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("run-stage-ledger".to_string(), &ctx);
        let (_base, db) = mk_db("stage-ledger");

        // 3 位阶段审查专家：1 死已消费 + 1 待取件 + 1 在跑
        ledger_run(&db, &cp.run_id, "sb", "stage_review", "invalid_output", true);
        ledger_run(&db, &cp.run_id, "sb", "stage_review", "succeeded", false);
        let running = ledger_run(&db, &cp.run_id, "sb", "stage_review", "running", false);
        cp.stage_review_batch_id = Some("sb".to_string());

        assert!(cp.stage_review_in_flight(&db), "2 名未消费成员在途");
        assert_eq!(cp.stage_review_in_flight_remaining(&db), 2);

        // 全部消费后自动出在途——无需任何名单清理动作
        db.0.lock().unwrap().execute(
            "UPDATE agent_child_runs SET consumed_at='x' WHERE batch_id='sb'", [],
        ).unwrap();
        assert!(!cp.stage_review_in_flight(&db));

        // 对质续跑（purpose=rebuttal + 同批次）→ 重新计入在途
        ledger_run(&db, &cp.run_id, "sb", "rebuttal", "running", false);
        assert!(cp.stage_review_in_flight(&db), "对质在途 = 阶段仍开放");
        assert!(cp.is_rebuttal_run(&db, &running) == false);
        let _ = std::fs::remove_dir_all(_base);
    }

    /// ── 回执消费原子提交：checkpoint + applied_at 同事务，同生共死 ──

    #[test]
    fn consume_commit_marks_applied_and_writes_verdict_atomically() {
        let (_base, db) = mk_db("consume-commit");
        let ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("run-cc".to_string(), &ctx);
        // 两条已投递回执：一条正常、一条契约违规裁决
        let ok_id = ledger_run(&db, "run-cc", "b1", "plan_review", "succeeded", true);
        let bad_id = ledger_run(&db, "run-cc", "b1", "plan_review", "succeeded", true);

        save_checkpoint_and_consume(
            &db,
            &mut cp,
            &ctx,
            &[
                AppliedReceipt { run_id: ok_id.clone(), verdict: None },
                AppliedReceipt {
                    run_id: bad_id.clone(),
                    verdict: Some(("invalid_output".to_string(), "contract_violation:missing".to_string())),
                },
            ],
        )
        .unwrap();

        let conn = db.0.lock().unwrap();
        let ok_applied: Option<String> = conn
            .query_row("SELECT applied_at FROM agent_child_runs WHERE id=?1", rusqlite::params![ok_id], |r| r.get(0))
            .unwrap();
        assert!(ok_applied.is_some(), "正常回执 applied_at 必须落账");
        let (bad_status, bad_err, bad_applied): (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT status, error_kind, applied_at FROM agent_child_runs WHERE id=?1",
                rusqlite::params![bad_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(bad_status, "invalid_output", "契约违规裁决须回写台账 status");
        assert_eq!(bad_err.as_deref(), Some("contract_violation:missing"));
        assert!(bad_applied.is_some());
        // checkpoint 本体也落盘了
        let cp_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM agent_checkpoints WHERE run_id='run-cc'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cp_rows, 1);
        drop(conn);
        let _ = std::fs::remove_dir_all(_base);
    }

    #[test]
    fn consume_commit_failure_leaves_receipts_unapplied() {
        // 原子性反向证明：checkpoint 写入失败 → applied_at 绝不能单独落账，
        // 否则"效果未持久化但回执已销毁"的丢件窗口就回来了。
        let (_base, db) = mk_db("consume-fail");
        let ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("run-cf".to_string(), &ctx);
        let rid = ledger_run(&db, "run-cf", "b1", "plan_review", "succeeded", true);

        // 人为制造写入失败：目标表不存在 → INSERT 必错
        db.0.lock().unwrap().execute("DROP TABLE agent_checkpoints", []).unwrap();
        let res = save_checkpoint_and_consume(
            &db,
            &mut cp,
            &ctx,
            &[AppliedReceipt { run_id: rid.clone(), verdict: None }],
        );
        assert!(res.is_err(), "checkpoint 写入失败必须返回错误");

        let applied: Option<String> = db
            .0
            .lock()
            .unwrap()
            .query_row("SELECT applied_at FROM agent_child_runs WHERE id=?1", rusqlite::params![rid], |r| r.get(0))
            .unwrap();
        assert!(applied.is_none(), "checkpoint 未落账时 applied_at 不得先行标记");
        let _ = std::fs::remove_dir_all(_base);
    }

    #[test]
    fn consume_commit_conflict_retries_and_marks_land_once() {
        // 乐观锁冲突路径：库中 version 被顶高 → 首次 UPDATE 不命中 →
        // 合并重试 → 成功提交时 applied_at 才随之落账（不重复、不抢先）。
        let (_base, db) = mk_db("consume-conflict");
        let ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("run-cr".to_string(), &ctx);
        let rid = ledger_run(&db, "run-cr", "b1", "plan_review", "succeeded", true);
        save_checkpoint(&db, &mut cp).unwrap();
        // 模拟另一保存者并发推进 version
        db.0.lock().unwrap().execute(
            "UPDATE agent_checkpoints SET version = 5 WHERE run_id = 'run-cr'", [],
        ).unwrap();

        save_checkpoint_and_consume(
            &db,
            &mut cp,
            &ctx,
            &[AppliedReceipt { run_id: rid.clone(), verdict: None }],
        )
        .unwrap();

        let conn = db.0.lock().unwrap();
        let applied: Option<String> = conn
            .query_row("SELECT applied_at FROM agent_child_runs WHERE id=?1", rusqlite::params![rid], |r| r.get(0))
            .unwrap();
        assert!(applied.is_some(), "冲突重试成功后标记随最终提交落账");
        let version: i64 = conn
            .query_row("SELECT version FROM agent_checkpoints WHERE run_id='run-cr'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 6, "应合并到最新 version 后 +1");
        drop(conn);
        let _ = std::fs::remove_dir_all(_base);
    }
