    use super::*;
    use crate::agent::child_reviewer::{ReviewFinding, ReviewScorecard, ReviewerDeliveryData};
    use crate::agent::context::SandwichContext;
    use std::sync::{Arc, Mutex};

    fn setup_test_db() -> DbState {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE agent_child_runs (
                id TEXT PRIMARY KEY,
                parent_run_id TEXT NOT NULL,
                child_run_id TEXT NOT NULL,
                status TEXT NOT NULL,
                kind TEXT,
                result_json TEXT,
                consumed_at TEXT,
                created_at TEXT,
                updated_at TEXT,
                finished_at TEXT,
                error_kind TEXT,
                task_id TEXT,
                attempt_id INTEGER,
                purpose TEXT,
                batch_id TEXT,
                contract_id TEXT,
                contract_schema TEXT,
                started_at TEXT
            );",
        )
        .unwrap();
        DbState(Arc::new(Mutex::new(conn)))
    }

    /// 名单退役版夹具：往台账插一条带 batch/purpose 盖章的审查行。
    /// consumed=true 模拟"回执已被 poll 消费"（reducer 跑在消费之后）。
    fn seed_review(
        db: &DbState,
        id: &str,
        parent: &str,
        batch: &str,
        status: &str,
        consumed: bool,
    ) {
        db.0.lock().unwrap().execute(
            "INSERT INTO agent_child_runs (id, parent_run_id, child_run_id, status, kind, purpose, batch_id, task_id, attempt_id, consumed_at, started_at)
             VALUES (?1, ?2, ?1, ?3, 'review', 'plan_review', ?4, ?1, 1, ?5, 't')",
            rusqlite::params![
                id, parent, status, batch,
                if consumed { Some("2024-01-01T00:00:00Z".to_string()) } else { None }
            ],
        ).unwrap();
    }

    /// 交付消费（生产时序：poll 先标终态+已消费，reducer 再记账）
    fn mark_succeeded(db: &DbState, id: &str) {
        db.0.lock().unwrap().execute(
            "UPDATE agent_child_runs SET status='succeeded', consumed_at='2024-01-01' WHERE id=?1",
            rusqlite::params![id],
        ).unwrap();
    }

    fn mark_failed(db: &DbState, id: &str, kind: &str) {
        db.0.lock().unwrap().execute(
            "UPDATE agent_child_runs SET status='timed_out', error_kind=?2, consumed_at='2024-01-01' WHERE id=?1",
            rusqlite::params![id, kind],
        ).unwrap();
    }

    #[test]
    fn test_gate_blocks_on_incomplete_batch_barrier() {
        let db = setup_test_db();
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-run".to_string(), &context);
        cp.plan_mode = PlanExecutionMode::CrossReview;
        cp.plan_reviewer_retry_count = 1;

        let batch = vec!["rev-1".to_string(), "rev-2".to_string()];
        for id in &batch {
            seed_review(&db, id, &cp.run_id, "b1", "running", false);
        }
        cp.set_plan_review_batch("b1", batch.len());

        // 仅交付 1 个，未齐套
        let del = ReviewerDeliveryData {
            findings: Vec::new(),
            confidence: "high".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: None,
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: Some(ReviewScorecard {
                correctness: 90,
                risk: 90,
                style: 90,
                test_coverage: None,
                null_reason: None,
                synthesized: false,
            }),
            extra: std::collections::HashMap::new(),
        };
        mark_succeeded(&db, "rev-1");
        cp.record_reviewer_delivery_for_run(&db, Some("rev-1"), &del);

        let res = check_pre_write_gatekeeper(&mut cp, &context, &db, "test-run", 1, false);
        assert!(res.is_some());
        let dispatch = res.unwrap();
        assert_eq!(dispatch.route, "plan_gatekeeper:need_wait_reviewer_jobs");
        assert!(dispatch.stderr.contains("齐套屏障保护"));
        assert!(dispatch.stderr.contains("本批次派发了 2 个审查任务，当前仅回传 1 个"));
    }

    #[test]
    fn test_gate_blocks_on_union_blocking_and_passes_when_clean() {
        let db = setup_test_db();
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-run".to_string(), &context);
        cp.plan_mode = PlanExecutionMode::CrossReview;
        cp.plan_reviewer_retry_count = 1;

        let batch = vec!["rev-1".to_string(), "rev-2".to_string()];
        for id in &batch {
            seed_review(&db, id, &cp.run_id, "b1", "running", false);
        }
        cp.set_plan_review_batch("b1", batch.len());

        // 审查员 1 发现 blocking 缺陷
        let del1 = ReviewerDeliveryData {
            findings: vec![ReviewFinding {
                id: "SEC-BLOCK-1".to_string(),
                severity: "blocking".to_string(),
                finality: "regardless".to_string(),
                location: "src/auth.rs:42".to_string(),
                current: "unauthenticated".to_string(),
                why: "missing auth check".to_string(),
                suggestion: "add auth guard".to_string(),
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
                risk: 30,
                style: 80,
                test_coverage: None,
                null_reason: None,
                synthesized: false,
            }),
            extra: std::collections::HashMap::new(),
        };
        mark_succeeded(&db, "rev-1");
        cp.record_reviewer_delivery_for_run(&db, Some("rev-1"), &del1);

        // 审查员 2 交付无 blocking
        let del2 = ReviewerDeliveryData {
            findings: Vec::new(),
            confidence: "high".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: None,
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: Some(ReviewScorecard {
                correctness: 90,
                risk: 90,
                style: 90,
                test_coverage: None,
                null_reason: None,
                synthesized: false,
            }),
            extra: std::collections::HashMap::new(),
        };
        mark_succeeded(&db, "rev-2");
        cp.record_reviewer_delivery_for_run(&db, Some("rev-2"), &del2);

        // 齐套且存在 blocking 缺陷：必须拦截并提供精准归因
        let res = check_pre_write_gatekeeper(&mut cp, &context, &db, "test-run", 1, false);
        assert!(res.is_some());
        let dispatch = res.unwrap();
        assert_eq!(dispatch.route, "plan_gatekeeper:blocking_findings_open");
        assert!(dispatch.stderr.contains("SEC-BLOCK-1"));
        assert!(dispatch.stderr.contains("src/auth.rs:42"));
        assert!(dispatch.stderr.contains("add auth guard"));

        // 模拟下一轮：缺陷被解决并撤回后，无 blocking 且评分达标
        // 新一轮 = 新批次锚点（旧成员行已消费出列，不污染本轮口径）
        seed_review(&db, "rev-3", &cp.run_id, "b2", "running", false);
        seed_review(&db, "rev-4", &cp.run_id, "b2", "running", false);
        cp.set_plan_review_batch("b2", 2);
        let mut del1_fixed = del1.clone();
        del1_fixed.withdrawn_findings = vec!["SEC-BLOCK-1".to_string()];
        del1_fixed.scorecard = Some(ReviewScorecard {
            correctness: 95,
            risk: 90,
            style: 85,
            test_coverage: None,
            null_reason: None,
            synthesized: false,
        });
        mark_succeeded(&db, "rev-3");
        cp.record_reviewer_delivery_for_run(&db, Some("rev-3"), &del1_fixed);
        mark_succeeded(&db, "rev-4");
        cp.record_reviewer_delivery_for_run(&db, Some("rev-4"), &del2);

        let res_clean = check_pre_write_gatekeeper(&mut cp, &context, &db, "test-run", 1, false);
        assert!(res_clean.is_none(), "全部齐套且无 blocking 时应放行写入");
        assert_eq!(cp.plan_phase, PlanPhase::Approved);
    }

    #[test]
    fn test_gate_handles_all_reviewers_failed_without_deadlock() {
        let db = setup_test_db();
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-run-fail".to_string(), &context);
        cp.plan_mode = PlanExecutionMode::CrossReview;
        cp.plan_reviewer_retry_count = 1;

        let batch = vec!["rev-1".to_string(), "rev-2".to_string()];
        for id in &batch {
            seed_review(&db, id, &cp.run_id, "b1", "running", false);
        }
        cp.set_plan_review_batch("b1", batch.len());

        // 两个任务均异常失败
        mark_failed(&db, "rev-1", "timed_out");
        cp.record_reviewer_failure_for_run(&db, "rev-1");
        mark_failed(&db, "rev-2", "corrupted_json_output");
        cp.record_reviewer_failure_for_run(&db, "rev-2");

        // 批次已收口，绝不能卡在 need_wait_reviewer_jobs！
        assert!(cp.plan_review_batch_complete(&db));
        assert_eq!(cp.plan_review_pending_count(&db), 0);

        let res = check_pre_write_gatekeeper(&mut cp, &context, &db, "test-run-fail", 1, false);
        assert!(res.is_some());
        let dispatch = res.unwrap();
        assert_eq!(dispatch.route, "plan_gatekeeper:reviewer_all_failed");
        assert!(dispatch.stderr.contains("rev-1"));
        assert!(dispatch.stderr.contains("timed_out"));
        assert!(dispatch.stderr.contains("rev-2"));
        assert!(dispatch.stderr.contains("corrupted_json_output"));
    }

    #[test]
    fn test_failover_replaces_expected_reviewer_in_batch() {
        // 名单退役版：备胎靠 batch_id 盖章自动入批，无需任何"换名"动作
        let db = setup_test_db();
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-failover".to_string(), &context);
        seed_review(&db, "rev-old", &cp.run_id, "b1", "running", false);
        cp.set_plan_review_batch("b1", 1);

        assert!(cp.run_belongs_to_plan_batch(&db, "rev-old"));

        // 老成员终态已消费 + 备胎行盖同 batch_id → 台账推导下自动成为唯一待决
        mark_failed(&db, "rev-old", "timed_out");
        seed_review(&db, "rev-new", &cp.run_id, "b1", "running", false);
        assert_eq!(cp.plan_review_pending_count(&db), 1, "唯一待决 = 备胎");
        assert!(cp.run_belongs_to_plan_batch(&db, "rev-new"));
        assert!(!cp.plan_review_batch_complete(&db));

        // 新任务交付后完成
        let del = ReviewerDeliveryData {
            findings: Vec::new(),
            confidence: "high".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: None,
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: None,
            extra: Default::default(),
        };
        mark_succeeded(&db, "rev-new");
        cp.record_reviewer_delivery_for_run(&db, Some("rev-new"), &del);
        assert!(cp.plan_review_batch_complete(&db));
    }

    #[test]
    fn test_tech_lead_approved_synthesizes_scorecard_and_passes_gate() {
        let db = setup_test_db();
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-tech-lead-ok".to_string(), &context);
        cp.plan_mode = PlanExecutionMode::CrossReview;
        cp.plan_reviewer_retry_count = 1;

        seed_review(&db, "rev-tl", &cp.run_id, "b1", "running", false);
        cp.set_plan_review_batch("b1", 1);

        let mut extra = std::collections::HashMap::new();
        extra.insert("decision".to_string(), serde_json::json!("approved"));
        extra.insert("file_coupling".to_string(), serde_json::json!("isolated"));
        extra.insert("analysis".to_string(), serde_json::json!("方案改动局部且清晰，通过"));

        let del = ReviewerDeliveryData {
            findings: Vec::new(),
            confidence: "confirmed".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: Some("certain".to_string()),
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: None, // 无显式 scorecard，由契约自动合成合格评分卡
            extra,
        };
        mark_succeeded(&db, "rev-tl");
        cp.record_reviewer_delivery_for_run(&db, Some("rev-tl"), &del);

        assert!(cp.last_review_scorecard.is_some(), "approved 应自动合成基线合格评分卡");
        assert!(
            cp.last_review_scorecard.as_ref().unwrap().synthesized,
            "合成的基线评分卡必须携带 synthesized 标记，与非审查员真实评分区分"
        );
        let res = check_pre_write_gatekeeper(&mut cp, &context, &db, "test-tech-lead-ok", 1, false);
        assert!(res.is_none(), "Tech Lead 裁决 approved 且无 blocking 时应放行写入");
        assert_eq!(cp.plan_phase, PlanPhase::Approved);
    }

    /// FREE_API_EVAL_REVIEW_PANEL 是进程级 env，凡读写它的测试共用一把锁防并发竞态
    static PANEL_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_tech_lead_escalate_blocks_gate_until_multi_expert_council_passes() {
        let _env_guard = PANEL_ENV_LOCK.lock().unwrap();
        let db = setup_test_db();
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-tech-lead-esc".to_string(), &context);
        cp.plan_mode = PlanExecutionMode::CrossReview;
        cp.plan_reviewer_retry_count = 1;

        // 1. Tech Lead 1v1 评审判定 escalate
        seed_review(&db, "rev-tl", &cp.run_id, "b1", "running", false);
        cp.set_plan_review_batch("b1", 1);

        let mut extra = std::collections::HashMap::new();
        extra.insert("decision".to_string(), serde_json::json!("escalate"));
        extra.insert("file_coupling".to_string(), serde_json::json!("coupled"));
        extra.insert("analysis".to_string(), serde_json::json!("涉及核心鉴权架构改造，需联合会审"));
        extra.insert("escalate_roles".to_string(), serde_json::json!(["architect", "security_specialist"]));

        let del = ReviewerDeliveryData {
            findings: Vec::new(),
            confidence: "confirmed".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: Some("certain".to_string()),
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: None,
            extra,
        };
        mark_succeeded(&db, "rev-tl");
        cp.record_reviewer_delivery_for_run(&db, Some("rev-tl"), &del);

        assert!(cp.plan_escalation_required.is_some());
        let res = check_pre_write_gatekeeper(&mut cp, &context, &db, "test-tech-lead-esc", 1, false);
        assert!(res.is_some());
        let dispatch = res.unwrap();
        assert_eq!(dispatch.route, "plan_gatekeeper:escalation_required");
        assert!(dispatch.stderr.contains("Tech Lead 方案评审判定必须升级多专家联合会审"));
        assert!(dispatch.stderr.contains("architect, security_specialist"));

        // 2. 主脑响应升级要求，并发派发多专家联合会审
        let council_batch = vec!["council-arch".to_string(), "council-sec".to_string()];
        for id in &council_batch {
            seed_review(&db, id, &cp.run_id, "b1", "running", false);
        }
        cp.set_plan_review_batch("b1", council_batch.len());
        assert!(cp.plan_escalation_required.is_none(), "注册多任务联合会审后升级挂起状态已清除");

        let council_del = ReviewerDeliveryData {
            findings: Vec::new(),
            confidence: "confirmed".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: Some("certain".to_string()),
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: Some(ReviewScorecard::new(90, 90, 85, None)),
            extra: Default::default(),
        };
        mark_succeeded(&db, "council-arch");
        cp.record_reviewer_delivery_for_run(&db, Some("council-arch"), &council_del);
        mark_succeeded(&db, "council-sec");
        cp.record_reviewer_delivery_for_run(&db, Some("council-sec"), &council_del);

        // 联合会审齐套且无 blocking：放行写入！
        let res_council = check_pre_write_gatekeeper(&mut cp, &context, &db, "test-tech-lead-esc", 2, false);
        assert!(res_council.is_none(), "联合会审齐套通过后放行写入");
        assert_eq!(cp.plan_phase, PlanPhase::Approved);
    }

    #[test]
    fn test_panel_off_downgrades_escalate_to_rebuttal() {
        let _env_guard = PANEL_ENV_LOCK.lock().unwrap();
        std::env::set_var("FREE_API_EVAL_REVIEW_PANEL", "off");
        let db = setup_test_db();
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-panel-off".to_string(), &context);
        cp.plan_mode = PlanExecutionMode::CrossReview;
        cp.plan_reviewer_retry_count = 1;
        seed_review(&db, "rev-tl", &cp.run_id, "b1", "running", false);
        cp.set_plan_review_batch("b1", 1);

        let mut extra = std::collections::HashMap::new();
        extra.insert("decision".to_string(), serde_json::json!("escalate"));
        extra.insert("file_coupling".to_string(), serde_json::json!("coupled"));
        extra.insert("analysis".to_string(), serde_json::json!("跨模块强关联，建议会审"));
        extra.insert(
            "escalate_roles".to_string(),
            serde_json::json!(["architect"]),
        );
        let del = ReviewerDeliveryData {
            findings: Vec::new(),
            confidence: "confirmed".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: Some("certain".to_string()),
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: Some(ReviewScorecard::new(70, 70, 70, None)),
            extra,
        };
        mark_succeeded(&db, "rev-tl");
        cp.record_reviewer_delivery_for_run(&db, Some("rev-tl"), &del);
        std::env::remove_var("FREE_API_EVAL_REVIEW_PANEL");

        // 面板禁用：不挂起升级要求，1v1 通道（submit_plan）不因 escalate 被封死
        assert!(cp.plan_escalation_required.is_none());
        // 降级后的 decision 以 rebuttal 入库，原始裁决留痕 original_decision
        let last = cp
            .plan_review_deliveries
            .values()
            .next()
            .expect("应有交付记录");
        assert_eq!(last.get_extra_str("decision"), Some("rebuttal"));
        assert_eq!(last.get_extra_str("original_decision"), Some("escalate"));
        assert!(last.get_extra_str("panel_note").is_some());
        // 有评分卡且无 blocking 时门禁可放行（1v1 闭环不被升级要求卡死）
        let _ = db; // db 仅为签名所需
    }

    fn exploration_checkpoint(
        mode: PlanExecutionMode,
        phase: PlanPhase,
        retry_count: u32,
    ) -> AgentCheckpoint {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-liveness".to_string(), &context);
        cp.plan_mode = mode;
        cp.plan_phase = phase;
        cp.plan_reviewer_retry_count = retry_count;
        cp
    }

    #[test]
    fn test_exploration_gate_fires_only_after_budget() {
        let cp = exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 0);
        assert!(check_cross_review_exploration_gate(&cp, 19, "read_file", "", 20).is_none());
        let res = check_cross_review_exploration_gate(&cp, 20, "grep_search", "", 20);
        assert!(res.is_some());
        let dispatch = res.unwrap();
        assert_eq!(
            dispatch.route,
            "plan_gatekeeper:exploration_budget_exhausted"
        );
        assert!(!dispatch.success);
        assert!(dispatch.stderr.contains("20"));
        assert!(dispatch.stderr.contains("child_reviewer"));
    }

    #[test]
    fn test_exploration_gate_allows_reviewer_dispatch_and_human_escalation() {
        let cp = exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 0);
        assert!(check_cross_review_exploration_gate(&cp, 20, "child_reviewer", "", 20).is_none());
        assert!(check_cross_review_exploration_gate(&cp, 20, "", "child_reviewer", 20).is_none());
        assert!(check_cross_review_exploration_gate(&cp, 20, "ask_human", "", 20).is_none());
        assert!(check_cross_review_exploration_gate(&cp, 20, "", "ask", 20).is_none());
    }

    #[test]
    fn test_exploration_gate_ignores_non_exploring_states() {
        let auto = exploration_checkpoint(PlanExecutionMode::Auto, PlanPhase::Exploring, 0);
        assert!(check_cross_review_exploration_gate(&auto, 100, "read_file", "", 20).is_none());

        let retried =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 1);
        assert!(check_cross_review_exploration_gate(&retried, 100, "read_file", "", 20).is_none());

        let approved =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Approved, 0);
        assert!(check_cross_review_exploration_gate(&approved, 100, "read_file", "", 20).is_none());
    }

    // ---------- 探索延期裁决（软提醒 + 结构化延长 + 绝对上限） ----------

    fn extension_cfg() -> ExplorationBudgetConfig {
        ExplorationBudgetConfig {
            soft_steps: 10,
            base_steps: 20,
            per_extension_steps: 8,
            max_extensions: 2,
            absolute_steps: 40,
        }
    }

    fn valid_extension_request() -> crate::agent::types::ExplorationExtensionRequest {
        crate::agent::types::ExplorationExtensionRequest {
            unresolved_question: "尚未确认 checkpoint 与 wait_jobs 的失败交付边界".to_string(),
            next_queries: vec!["定位 record_reviewer_failure_for_run 的调用点".to_string()],
            extra_steps: None,
            reason: String::new(),
        }
    }

    #[test]
    fn test_extension_grant_raises_effective_limit() {
        let cfg = extension_cfg();
        let mut cp =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 0);
        let verdict =
            adjudicate_exploration_extension_with(&mut cp, &valid_extension_request(), cfg);
        assert!(verdict.granted);
        assert_eq!(verdict.granted_steps, 8);
        assert_eq!(verdict.route, "plan_liveness:extension_granted");
        assert_eq!(cp.plan_exploration_extensions_used, 1);
        assert_eq!(cp.plan_exploration_granted_steps, 8);
        // 有效上限从 20 抬升到 28：第 25 步的搜索在授予后被放行
        assert!(check_cross_review_exploration_gate(&cp, 25, "search", "", 28).is_none());
    }

    #[test]
    fn test_extension_denied_without_unresolved_question() {
        let cfg = extension_cfg();
        let mut cp =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 0);
        let mut req = valid_extension_request();
        req.unresolved_question = "   ".to_string();
        let verdict = adjudicate_exploration_extension_with(&mut cp, &req, cfg);
        assert!(!verdict.granted);
        assert_eq!(verdict.route, "plan_liveness:extension_denied");
        // 结构不合法的拒绝不消耗延期次数
        assert_eq!(cp.plan_exploration_extensions_used, 0);
        assert_eq!(cp.plan_exploration_granted_steps, 0);
    }

    #[test]
    fn test_extension_denied_without_concrete_queries() {
        let cfg = extension_cfg();
        let mut cp =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 0);
        let mut req = valid_extension_request();
        req.next_queries = vec!["  ".to_string()];
        let verdict = adjudicate_exploration_extension_with(&mut cp, &req, cfg);
        assert!(!verdict.granted);
        assert_eq!(cp.plan_exploration_extensions_used, 0);

        req.next_queries = (0..7).map(|i| format!("query {}", i)).collect();
        let verdict = adjudicate_exploration_extension_with(&mut cp, &req, cfg);
        assert!(!verdict.granted, "超过 6 条取证项同样拒绝");
        assert_eq!(cp.plan_exploration_extensions_used, 0);
    }

    #[test]
    fn test_extension_count_capped_by_max_extensions() {
        let cfg = extension_cfg();
        let mut cp =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 0);
        let req = valid_extension_request();
        assert!(adjudicate_exploration_extension_with(&mut cp, &req, cfg).granted);
        assert!(adjudicate_exploration_extension_with(&mut cp, &req, cfg).granted);
        let third = adjudicate_exploration_extension_with(&mut cp, &req, cfg);
        assert!(!third.granted, "第三次申请必须被次数上限拒绝");
        assert_eq!(cp.plan_exploration_extensions_used, 2);
        assert_eq!(cp.plan_exploration_granted_steps, 16);
    }

    #[test]
    fn test_extension_never_exceeds_absolute_cap() {
        let cfg = extension_cfg(); // base 20 + abs 40 → 最多再授 20 步
        let mut cp =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 0);
        let mut req = valid_extension_request();
        req.extra_steps = Some(8);
        adjudicate_exploration_extension_with(&mut cp, &req, cfg); // +8 → 28
        adjudicate_exploration_extension_with(&mut cp, &req, cfg); // +8 → 36
        // 次数已用完；把次数上限抬高后，余量只剩 40-36=4 步
        let loose_cfg = ExplorationBudgetConfig { max_extensions: 9, ..cfg };
        let verdict = adjudicate_exploration_extension_with(&mut cp, &req, loose_cfg);
        assert!(verdict.granted);
        assert_eq!(verdict.granted_steps, 4, "授予量必须被绝对上限截断");
        assert_eq!(cfg.effective_limit(cp.plan_exploration_granted_steps), 40);
    }

    #[test]
    fn test_extension_not_applicable_outside_exploring() {
        let cfg = extension_cfg();
        let req = valid_extension_request();
        let mut auto = exploration_checkpoint(PlanExecutionMode::Auto, PlanPhase::Exploring, 0);
        let v = adjudicate_exploration_extension_with(&mut auto, &req, cfg);
        assert!(!v.granted);
        assert_eq!(v.route, "plan_liveness:extension_not_applicable");

        let mut dispatched =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Reviewing, 1);
        let v = adjudicate_exploration_extension_with(&mut dispatched, &req, cfg);
        assert!(!v.granted);

        let mut approved =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Approved, 0);
        let v = adjudicate_exploration_extension_with(&mut approved, &req, cfg);
        assert!(!v.granted);
        // 不适用裁决不得污染延期账本
        assert_eq!(auto.plan_exploration_extensions_used, 0);
        assert_eq!(dispatched.plan_exploration_granted_steps, 0);
        assert_eq!(approved.plan_exploration_extensions_used, 0);
    }

    #[test]
    fn test_extension_request_makes_bare_instruction_executable() {
        let bare = crate::agent::types::AgentInstruction {
            request_exploration_extension: Some(valid_extension_request()),
            ..Default::default()
        };
        assert!(bare.validate_executable_shape().is_ok());
        assert!(bare.is_bare_exploration_extension_request());

        let mut attached = crate::agent::types::AgentInstruction::default();
        attached.tool = Some("search".to_string());
        attached.request_exploration_extension = Some(valid_extension_request());
        assert!(attached.validate_executable_shape().is_ok());
        assert!(!attached.is_bare_exploration_extension_request());
    }

    // ---------- submit_plan：适用性预检 / 结构校验 / 一致提交 ----------

    fn valid_submission() -> crate::agent::types::PlanSubmission {
        crate::agent::types::PlanSubmission {
            plan_summary: "修复 checkpoint 保存时的竞态：把批次注册并入同一次 UPDATE".to_string(),
            target_files: vec!["src/agent/checkpoint.rs".to_string()],
            root_cause: "spawn 与批次注册分离导致半截状态".to_string(),
            evidence_refs: vec!["src/agent/checkpoint.rs:922".to_string()],
            assumptions: vec![],
            open_questions: vec![],
            intended_verification: "cargo test --lib plan_gate".to_string(),
            risk_level: "medium".to_string(),
        }
    }

    #[test]
    fn test_submit_plan_rejection_phase_matrix() {
        // 交叉审查 + Exploring + 未派审 → 放行
        let mut cp =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 0);
        assert!(submit_plan_rejection(&cp).is_none());

        // Auto 模式 → 不适用
        let auto = exploration_checkpoint(PlanExecutionMode::Auto, PlanPhase::Exploring, 0);
        assert!(submit_plan_rejection(&auto).unwrap().contains("不适用"));

        // PlanApproval 模式 → 不适用（应走 ask_human）
        let approval =
            exploration_checkpoint(PlanExecutionMode::PlanApproval, PlanPhase::Exploring, 0);
        assert!(submit_plan_rejection(&approval).unwrap().contains("不适用"));

        // 已放行 → 不适用
        let approved =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Approved, 0);
        assert!(submit_plan_rejection(&approved).unwrap().contains("已获放行"));

        // 升级挂起 → 拒绝并指引联合会审
        cp.plan_escalation_required = Some(vec!["architect".to_string()]);
        let rej = submit_plan_rejection(&cp).unwrap();
        assert!(rej.contains("联合会审"));
        assert!(rej.contains("architect"));
        cp.plan_escalation_required = None;

        // 审查轮次未闭合（已预扣未消费）→ 拒绝重复提交
        cp.plan_reviewer_retry_count = 1;
        cp.plan_reviewer_consumed = false;
        assert!(submit_plan_rejection(&cp).unwrap().contains("在途"));

        // 上一轮已消费 → 允许新一轮提交
        cp.plan_reviewer_consumed = true;
        assert!(submit_plan_rejection(&cp).is_none());

        // 预算耗尽 → 拒绝
        cp.plan_reviewer_retry_count = crate::agent::checkpoint::max_plan_reviewer_rounds();
        assert!(submit_plan_rejection(&cp).unwrap().contains("上限"));
    }

    #[test]
    fn test_validate_plan_submission_structure() {
        // 合法提交
        let (files, refs) = validate_plan_submission(&valid_submission(), true).unwrap();
        assert_eq!(files, vec!["src/agent/checkpoint.rs"]);
        assert_eq!(refs, vec!["src/agent/checkpoint.rs:922"]);

        // 摘要过短
        let mut s = valid_submission();
        s.plan_summary = "修 bug".to_string();
        assert!(validate_plan_submission(&s, true).unwrap_err().contains("plan_summary"));

        // 目标文件可空（纯分析/只读交付），但上限仍生效
        let mut s = valid_submission();
        s.target_files = vec![];
        assert!(validate_plan_submission(&s, true).is_ok());
        s.target_files = (0..51).map(|i| format!("f{i}.rs")).collect();
        assert!(validate_plan_submission(&s, true).unwrap_err().contains("target_files"));

        // 无证据 / 证据超上限
        let mut s = valid_submission();
        s.evidence_refs = vec!["  ".to_string()];
        assert!(validate_plan_submission(&s, true).unwrap_err().contains("evidence_refs"));
        s.evidence_refs = (0..7).map(|i| format!("src/f{i}.rs")).collect();
        assert!(validate_plan_submission(&s, true).unwrap_err().contains("evidence_refs"));
    }

    #[test]
    fn test_validate_plan_submission_evidence_optional_under_triage() {
        // triage 判决 evidence_policy=none/source_refs → 0 条证据也放行（产物类方案无"已读代码"可引）
        let mut s = valid_submission();
        s.evidence_refs = vec![];
        assert!(validate_plan_submission(&s, false).is_ok());
        // 上限仍然生效：超过 6 条照样拒绝
        s.evidence_refs = (0..7).map(|i| format!("src/f{i}.rs")).collect();
        assert!(validate_plan_submission(&s, false).unwrap_err().contains("evidence_refs"));
    }

    #[test]
    fn test_plan_risk_preflight_blocks_silent_sensitive_plans() {
        // 命中敏感模块但摘要无防御陈述 → 预检拦截并指路
        let mut s = valid_submission();
        s.target_files = vec!["src/auth/token_store.rs".to_string()];
        let err = validate_plan_submission(&s, true).unwrap_err();
        assert!(err.contains("预检拦截"));
        assert!(err.contains("敏感凭据"));

        // 摘要显式陈述防御设计 → 放行
        let mut s = valid_submission();
        s.target_files = vec!["src/auth/token_store.rs".to_string()];
        s.plan_summary.push_str("；日志对 token 做脱敏处理，不落盘明文");
        assert!(validate_plan_submission(&s, true).is_ok());

        // 缓存模块缺有界陈述 → 拦截
        let mut s = valid_submission();
        s.target_files = vec!["src/cache/memo_store.rs".to_string()];
        let err = validate_plan_submission(&s, true).unwrap_err();
        assert!(err.contains("缓存/内存结构"));

        // 显式豁免陈述（逃生口：声明风险不适用即放行）→ 放行
        let mut s = valid_submission();
        s.target_files = vec!["src/cache/memo_store.rs".to_string()];
        s.plan_summary.push_str("；本次为只读访问，不涉及容量变更");
        assert!(validate_plan_submission(&s, true).is_ok());

        // 普通文件不触发预检 → 放行
        assert!(validate_plan_submission(&valid_submission(), true).is_ok());
    }

    #[test]
    fn test_commit_plan_submission_is_consistent() {
        let mut cp =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 0);
        let db = setup_test_db();
        let snapshot = crate::agent::child_reviewer::PlanReviewerDispatchSnapshot::new(
            "model-x",
            "pro",
            "high",
            "persona",
            serde_json::json!({"role": "tech_lead"}),
        )
        .with_batch_id("b-42");
        // 台账夹具：派发落库的子 run 行盖批次章（spawn 时写入）
        seed_review(&db, "rev-42", &cp.run_id, "b-42", "running", false);
        cp.commit_plan_submission(valid_submission(), "b-42", &snapshot, 9);

        // 不变式：方案落账、阶段推进、预算预扣、批次注册、派发快照——同生同灭
        assert!(cp.submitted_plan.is_some());
        assert_eq!(cp.submitted_plan_step, Some(9));
        assert_eq!(cp.plan_phase, PlanPhase::Reviewing);
        assert_eq!(cp.plan_reviewer_retry_count, 1);
        assert!(!cp.plan_reviewer_consumed);
        assert!(cp.run_belongs_to_plan_batch(&db, "rev-42"));
        assert_eq!(cp.plan_review_member_count(&db), 1);
        assert!(cp.plan_reviewer_last_dispatch.is_some());
        // 提交后门禁应把后续提交视为「轮次未闭合」
        assert!(submit_plan_rejection(&cp).unwrap().contains("在途"));
    }

    #[test]
    fn test_submit_plan_instruction_shapes() {
        // 纯提交可执行且识别为 bare
        let bare = crate::agent::types::AgentInstruction {
            submit_plan: Some(valid_submission()),
            ..Default::default()
        };
        assert!(bare.validate_executable_shape().is_ok());
        assert!(bare.is_bare_plan_submission());
        assert!(!bare.is_bare_exploration_extension_request());

        // 附带工具入口 → 非 bare（宿主会以协议误用拒绝混发）
        let mut mixed = crate::agent::types::AgentInstruction::default();
        mixed.tool = Some("search".to_string());
        mixed.submit_plan = Some(valid_submission());
        assert!(mixed.validate_executable_shape().is_ok());
        assert!(!mixed.is_bare_plan_submission());
    }
