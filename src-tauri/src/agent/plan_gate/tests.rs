    use super::*;
    use crate::agent::checkpoint::PlanPhase;
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
                contract_schema TEXT, execution_config_json TEXT, execution_checkpoint_json TEXT, execution_version INTEGER NOT NULL DEFAULT 0,
                applied_at TEXT,
                started_at TEXT
            );",
        )
        .unwrap();
        DbState(Arc::new(Mutex::new(conn)))
    }

    #[test]
    fn auto_mode_waits_for_a_formally_dispatched_panel_without_enabling_stage_mode() {
        let db=setup_test_db();
        let context=SandwichContext::new("sys".into(),"goal".into());
        let mut cp=AgentCheckpoint::new("session".into(),&context);
        cp.set_execution_mode(PlanExecutionMode::Auto);
        cp.submitted_plan=Some(serde_json::from_value(serde_json::json!({"plan_summary":"正式提交待多人审查的方案"})).unwrap());
        cp.plan_phase=PlanPhase::Reviewing;
        cp.plan_reviewer_retry_count=1;
        cp.set_plan_review_batch("batch",2);
        seed_review(&db,"a","run","batch","running",false);
        seed_review(&db,"b","run","batch","running",false);
        let result=check_pre_write_gatekeeper(&mut cp,&context,&db,"run",5,false).unwrap();
        assert_eq!(result.route,"plan_gatekeeper:need_wait_reviewer_jobs");
        assert_eq!(cp.plan_mode,PlanExecutionMode::Auto);
        assert!(!cp.plan_phase.is_approved());
        assert!(cp.plan_gate_projection().plan_required);
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
            "INSERT INTO agent_child_runs (id, parent_run_id, child_run_id, status, kind, purpose, batch_id, task_id, attempt_id, consumed_at, applied_at, started_at)
             VALUES (?1, ?2, ?1, ?3, 'review', 'plan_review', ?4, ?1, 1, ?5, ?5, 't')",
            rusqlite::params![
                id, parent, status, batch,
                if consumed { Some("2024-01-01T00:00:00Z".to_string()) } else { None }
            ],
        ).unwrap();
    }

    /// 交付消费（生产时序：poll 先标终态+已消费，reducer 再记账）
    fn mark_succeeded(db: &DbState, id: &str) {
        db.0.lock().unwrap().execute(
            "UPDATE agent_child_runs SET status='succeeded', consumed_at='2024-01-01', applied_at='2024-01-01' WHERE id=?1",
            rusqlite::params![id],
        ).unwrap();
    }

    fn mark_failed(db: &DbState, id: &str, kind: &str) {
        db.0.lock().unwrap().execute(
            "UPDATE agent_child_runs SET status='timed_out', error_kind=?2, consumed_at='2024-01-01', applied_at='2024-01-01' WHERE id=?1",
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
        cp.submitted_plan = Some(valid_submission());

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
    fn one_valid_plan_review_survives_two_unavailable_members_without_losing_its_blockers() {
        for blocked in [false, true] {
            let db = setup_test_db();
            let context = SandwichContext::new("sys".into(), "goal".into());
            let mut cp = AgentCheckpoint::new("partial-panel".into(), &context);
            cp.plan_mode = PlanExecutionMode::CrossReview;
            cp.plan_reviewer_retry_count = 1;
            cp.submitted_plan = Some(valid_submission());
            for id in ["a", "b", "c"] {
                seed_review(&db, id, &cp.run_id, "partial", "running", false);
            }
            cp.set_plan_review_batch("partial", 3);
            let findings = if blocked { serde_json::json!([{
                "id":"F1", "severity":"blocking", "finality":"regardless",
                "location":"src/auth.rs:42", "current":"missing guard",
                "why":"unauthorized access", "suggestion":"add guard", "evidence_strength":"confirmed"
            }]) } else { serde_json::json!([]) };
            let delivery: ReviewerDeliveryData = serde_json::from_value(serde_json::json!({
                "findings":findings, "confidence":"confirmed", "evidence_refs":[], "missing_evidence":[],
                "decision":"approved", "routing":"certain"
            })).unwrap();
            mark_succeeded(&db, "a");
            cp.record_reviewer_delivery_for_run(&db, Some("a"), &delivery);
            // 尚未收束的成员不能因一位批准而被静默放弃。
            assert_eq!(check_pre_write_gatekeeper(&mut cp, &context, &db, "partial-panel", 1, false).unwrap().route,
                "plan_gatekeeper:need_wait_reviewer_jobs");
            for id in ["b", "c"] {
                mark_failed(&db, id, "timed_out");
                cp.record_reviewer_failure_for_run(&db, id);
            }
            assert!(cp.plan_review_batch_complete(&db));
            let result = check_pre_write_gatekeeper(&mut cp, &context, &db, "partial-panel", 1, false);
            if blocked {
                assert_eq!(result.unwrap().route, "plan_gatekeeper:blocking_findings_open");
            } else {
                assert!(result.is_none());
                assert_eq!(cp.plan_phase, PlanPhase::Approved);
            }
        }
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
    fn test_tech_lead_approved_without_scorecard_passes_gate() {
        let db = setup_test_db();
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-tech-lead-ok".to_string(), &context);
        cp.plan_mode = PlanExecutionMode::CrossReview;
        cp.plan_reviewer_retry_count = 1;
        cp.submitted_plan = Some(valid_submission());

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
            scorecard: None, // 无评分也可形成有效批准
            extra,
        };
        mark_succeeded(&db, "rev-tl");
        cp.record_reviewer_delivery_for_run(&db, Some("rev-tl"), &del);

        assert!(cp.last_review_scorecard.is_none(), "批准不应伪造评分");
        let res = check_pre_write_gatekeeper(&mut cp, &context, &db, "test-tech-lead-ok", 1, false);
        assert!(res.is_none(), "Tech Lead 裁决 approved 且无 blocking 时应放行写入");
        assert_eq!(cp.plan_phase, PlanPhase::Approved);
    }

    #[test]
    fn resumed_approval_supersedes_its_own_rebuttal_but_not_another_reviewers() {
        let db = setup_test_db();
        let context = SandwichContext::new("sys".into(), "goal".into());
        let mut cp = AgentCheckpoint::new("review-lineage".into(), &context);
        cp.plan_mode = PlanExecutionMode::CrossReview;
        cp.plan_reviewer_retry_count = 1;
        cp.submitted_plan = Some(valid_submission());
        for (id, task, attempt, decision) in [
            ("rev-old", "task-a", 1, "rebuttal"),
            ("rev-new", "task-a", 2, "approved"),
            ("rev-other", "task-b", 1, "rebuttal"),
        ] {
            seed_review(&db, id, &cp.run_id, "lineage-batch", "succeeded", true);
            db.0.lock().unwrap().execute("UPDATE agent_child_runs SET task_id=?2, attempt_id=?3 WHERE id=?1",
                rusqlite::params![id, task, attempt]).unwrap();
            let delivery = serde_json::from_value(serde_json::json!({
                "findings":[],"confidence":"supported","evidence_refs":[],"missing_evidence":[],"decision":decision
            })).unwrap();
            cp.plan_review_deliveries.insert(id.into(), delivery);
        }
        cp.plan_reviewer_consumed = true;
        let blocked = check_pre_write_gatekeeper(&mut cp, &context, &db, "review-lineage", 3, false).unwrap();
        assert_eq!(blocked.route, "plan_gatekeeper:review_revisions_required");
        cp.plan_review_deliveries.remove("rev-other");
        assert!(check_pre_write_gatekeeper(&mut cp, &context, &db, "review-lineage", 4, false).is_none());
        assert!(cp.plan_phase.is_approved());
        assert_eq!(cp.plan_review_deliveries["rev-old"].get_extra_str("decision"), Some("rebuttal"), "原始裁决保留");
    }

    /// 旁路通道防线：investigate 直连 Tech Lead 也能把 plan_phase 推进 Reviewing
    /// 并拿到 approved 裁决，但没有 submitted_plan 落账的批准没有契约锚点——
    /// 必须拦截并要求结构化提交（workspace_leak 实测绕过路径）。
    #[test]
    fn test_approval_requires_submitted_plan_on_record() {
        let db = setup_test_db();
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-no-plan".to_string(), &context);
        cp.plan_mode = PlanExecutionMode::CrossReview;
        cp.plan_reviewer_retry_count = 1;
        // 不种 submitted_plan：模拟 investigate 直连派发拿到批准
        seed_review(&db, "rev-tl", &cp.run_id, "b1", "running", false);
        cp.set_plan_review_batch("b1", 1);

        let mut extra = std::collections::HashMap::new();
        extra.insert("decision".to_string(), serde_json::json!("approved"));
        let del = ReviewerDeliveryData {
            findings: Vec::new(),
            confidence: "confirmed".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: Some("certain".to_string()),
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: Some(ReviewScorecard::new(90, 80, 85, None)),
            extra,
        };
        mark_succeeded(&db, "rev-tl");
        cp.record_reviewer_delivery_for_run(&db, Some("rev-tl"), &del);

        let res = check_pre_write_gatekeeper(&mut cp, &context, &db, "test-no-plan", 1, false);
        let rej = res.expect("无 submitted_plan 在案时批准不得放行");
        assert_eq!(rej.route, "plan_gatekeeper:no_submitted_plan");
        assert!(rej.stderr.contains("submit_plan"), "应指引结构化提交: {}", rej.stderr);
        assert!(!cp.plan_phase.is_approved(), "阶段不得推进到 Approved");

        // 补上结构化提交后，同一批准结论即可放行
        cp.submitted_plan = Some(valid_submission());
        let res2 = check_pre_write_gatekeeper(&mut cp, &context, &db, "test-no-plan", 2, false);
        assert!(res2.is_none(), "方案落账后同批准结论应放行");
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
        cp.submitted_plan = Some(valid_submission());

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
            revision_reason: String::new(),
            ..Default::default()
        }
    }

    #[test]
    fn test_submit_plan_rejection_phase_matrix() {
        // 交叉审查 + Exploring + 未派审 → 放行
        let mut cp =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 0);
        assert!(submit_plan_rejection(&cp, &valid_submission()).is_none());

        // Auto 模式 → 放行（submit_plan 兼任「报方案拿判定」通道，
        // FastPass 直通 / Escalate 由 handle_submit_plan 内 auto_policy_verdict 分流）
        let auto = exploration_checkpoint(PlanExecutionMode::Auto, PlanPhase::Exploring, 0);
        assert!(submit_plan_rejection(&auto, &valid_submission()).is_none());

        // 人工批准模式同样提交正式方案，由宿主衔接人工确认。
        let approval =
            exploration_checkpoint(PlanExecutionMode::PlanApproval, PlanPhase::Exploring, 0);
        assert!(submit_plan_rejection(&approval, &valid_submission()).is_none());

        // 已放行 → 不适用
        let approved =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Approved, 0);
        assert!(submit_plan_rejection(&approved, &valid_submission()).unwrap().contains("已获放行"));

        // 升级挂起 → 拒绝并指引联合会审
        cp.plan_escalation_required = Some(vec!["architect".to_string()]);
        let rej = submit_plan_rejection(&cp, &valid_submission()).unwrap();
        assert!(rej.contains("联合会审"));
        assert!(rej.contains("architect"));
        cp.plan_escalation_required = None;

        // 审查轮次未闭合（已预扣未消费）→ 拒绝重复提交
        cp.plan_reviewer_retry_count = 1;
        cp.plan_reviewer_consumed = false;
        assert!(submit_plan_rejection(&cp, &valid_submission()).unwrap().contains("在途"));

        // 上一轮已消费 → 允许新一轮提交
        cp.plan_reviewer_consumed = true;
        assert!(submit_plan_rejection(&cp, &valid_submission()).is_none());

        // 预算耗尽 → 拒绝
        cp.plan_reviewer_retry_count = crate::agent::checkpoint::max_plan_reviewer_rounds();
        assert!(submit_plan_rejection(&cp, &valid_submission()).unwrap().contains("上限"));
    }

    #[test]
    fn test_validate_plan_submission_structure() {
        // 合法提交
        let (files, refs) = validate_plan_submission(&valid_submission(), true).unwrap();
        assert_eq!(files, vec!["src/agent/checkpoint.rs"]);
        assert_eq!(refs, vec!["src/agent/checkpoint.rs:922"]);

        // 摘要长度不作为质量判据；仅拒绝空白
        let mut s = valid_submission();
        s.plan_summary = "修 bug".to_string();
        assert!(validate_plan_submission(&s, true).is_ok());
        s.plan_summary = "  ".into();
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
        assert!(validate_plan_submission(&s, true).is_ok());
        s.evidence_refs = vec!["x".repeat(crate::agent::child_reviewer::MAX_REVIEW_REFERENCE_CHARS + 1)];
        assert!(validate_plan_submission(&s, true).unwrap_err().contains("引用总量"));
    }

    #[test]
    fn test_validate_plan_submission_evidence_optional_under_triage() {
        // triage 判决 evidence_policy=none/source_refs → 0 条证据也放行（产物类方案无"已读代码"可引）
        let mut s = valid_submission();
        s.evidence_refs = vec![];
        assert!(validate_plan_submission(&s, false).is_ok());
        // 可选证据允许多条引用，但仍受共享总量预算约束。
        s.evidence_refs = (0..7).map(|i| format!("src/f{i}.rs")).collect();
        assert!(validate_plan_submission(&s, false).is_ok());
        s.evidence_refs = vec!["x".repeat(crate::agent::child_reviewer::MAX_REVIEW_REFERENCE_CHARS + 1)];
        assert!(validate_plan_submission(&s, false).unwrap_err().contains("引用总量"));
    }

    #[test]
    fn test_sensitive_looking_paths_are_not_blocked() {
        // 路径里含 login/auth/cache/sql 等字样不再触发预检拦截
        for f in ["src/views/Login.vue", "src/auth/token_store.rs", "tmp/mock_audit.sql"] {
            let mut s = valid_submission();
            s.target_files = vec![f.to_string()];
            assert!(validate_plan_submission(&s, true).is_ok(), "{f}");
        }
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
        assert!(submit_plan_rejection(&cp, &valid_submission()).unwrap().contains("在途"));
    }

    #[test]
    fn test_post_approval_plan_revision_reopens_review() {
        // 已批准 + 无理由重提 → 拒绝（防误重复提交）
        let approved =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Approved, 1);
        assert!(submit_plan_rejection(&approved, &valid_submission())
            .unwrap()
            .contains("已获放行"));

        // 已批准 + 附 revision_reason → 放行进入修订轮（前轮审查已消费闭合）
        let mut rev = valid_submission();
        rev.revision_reason =
            "path_guard 白名单在 workspace_root==session_tmp 下会误标 tmp 身份".to_string();
        let mut cp =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Approved, 1);
        cp.plan_reviewer_consumed = true;
        assert!(submit_plan_rejection(&cp, &rev).is_none());

        // 修订轮每次重新给一套审查次数：上一轮用尽也不拒绝带理由的修订提交
        cp.plan_reviewer_retry_count = crate::agent::checkpoint::max_plan_reviewer_rounds();
        assert!(submit_plan_rejection(&cp, &rev).is_none());

        // 非修订轮（审查中、未批准）轮次耗尽仍拒绝（逃生口走 ask_human）
        let mut reviewing =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Reviewing, 1);
        reviewing.plan_reviewer_consumed = true;
        reviewing.plan_reviewer_retry_count = crate::agent::checkpoint::max_plan_reviewer_rounds();
        assert!(submit_plan_rejection(&reviewing, &valid_submission())
            .unwrap()
            .contains("上限"));
    }

    #[test]
    fn test_commit_plan_submission_supersedes_approval() {
        // 修订提交的一致落账：阶段回退 Reviewing、旧批准痕迹作废、预算正常预扣
        let mut cp =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Approved, 1);
        cp.plan_reviewer_consumed = true;
        cp.approved_target_files = Some(vec!["src/old.rs".to_string()]);
        cp.approved_plan_summary = Some("用户批准了旧方案".to_string());
        let db = setup_test_db();
        let snapshot = crate::agent::child_reviewer::PlanReviewerDispatchSnapshot::new(
            "model-x",
            "pro",
            "high",
            "persona",
            serde_json::json!({"role": "tech_lead"}),
        )
        .with_batch_id("b-rev");
        seed_review(&db, "rev-r1", &cp.run_id, "b-rev", "running", false);

        let mut rev = valid_submission();
        rev.revision_reason = "执行中发现原方案未覆盖的共享判定顺序问题".to_string();
        cp.commit_plan_submission(rev, "b-rev", &snapshot, 20);

        assert_eq!(cp.plan_phase, PlanPhase::Reviewing);
        assert!(cp.approved_target_files.is_none());
        assert!(cp.approved_plan_summary.is_none());
        // 修订提交先清零再预扣一轮：每次修订重新一套次数
        assert_eq!(cp.plan_reviewer_retry_count, 1);
        assert!(!cp.plan_reviewer_consumed);
        assert_eq!(cp.submitted_plan_step, Some(20));
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
        assert!(!bare.is_bare_retriage_request());

        // 附带工具入口 → 非 bare（宿主会以协议误用拒绝混发）
        let mut mixed = crate::agent::types::AgentInstruction::default();
        mixed.tool = Some("search".to_string());
        mixed.submit_plan = Some(valid_submission());
        assert!(mixed.validate_executable_shape().is_ok());
        assert!(!mixed.is_bare_plan_submission());
    }

    fn technical_plan_verdict() -> crate::agent::triage::TriageVerdict {
        let mut verdict = crate::agent::triage::TriageVerdict::fallback("test");
        verdict.fallback = None;
        verdict.route = "technical_plan".into();
        verdict
    }

    fn approved_delivery(decision: &str) -> ReviewerDeliveryData {
        let mut extra = std::collections::HashMap::new();
        extra.insert("decision".to_string(), serde_json::json!(decision));
        ReviewerDeliveryData {
            findings: Vec::new(),
            confidence: "confirmed".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: Some("certain".to_string()),
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: None,
            extra,
        }
    }

    #[test]
    fn delegated_writes_wait_for_required_plan_then_are_unrestricted() {
        let db = setup_test_db();
        let context = SandwichContext::new("sys".into(), "goal".into());
        let mut parent = AgentCheckpoint::new("parent".into(), &context);
        parent.set_execution_mode(PlanExecutionMode::Auto);
        parent.triage_verdict = Some(technical_plan_verdict());
        assert!(ancestor_plan_pending(&parent, &db).is_some(), "需要方案但未提交时暂停委派写入");

        // 按配置无需审查而直接保存方案：与上级自身写入一致，之后不再限制。
        parent.submitted_plan = Some(valid_submission());
        parent.plan_phase = PlanPhase::Approved;
        assert!(ancestor_plan_pending(&parent, &db).is_none());
    }

    #[test]
    fn delegated_writes_are_not_limited_when_plan_is_not_required_or_disabled() {
        let db = setup_test_db();
        let context = SandwichContext::new("sys".into(), "goal".into());
        let mut parent = AgentCheckpoint::new("parent".into(), &context);
        parent.set_execution_mode(PlanExecutionMode::Auto);
        assert!(ancestor_plan_pending(&parent, &db).is_none(), "未分流不额外限制");

        let mut direct = technical_plan_verdict();
        direct.route = "direct".into();
        parent.triage_verdict = Some(direct);
        assert!(ancestor_plan_pending(&parent, &db).is_none(), "简单任务不额外限制");

        parent.triage_verdict = Some(technical_plan_verdict());
        parent.stages.plan = Some(false);
        assert!(ancestor_plan_pending(&parent, &db).is_none(), "方案环节关闭时沿用原权限");

        let mut failed = technical_plan_verdict();
        failed.fallback = Some("分流失败".into());
        parent.stages.plan = None;
        parent.triage_verdict = Some(failed);
        assert!(ancestor_plan_pending(&parent, &db).is_none(), "分流失败不是需要方案的判决");
    }

    #[test]
    fn delegated_writes_follow_human_plan_approval() {
        let db = setup_test_db();
        let context = SandwichContext::new("sys".into(), "goal".into());
        let mut parent = AgentCheckpoint::new("parent".into(), &context);
        parent.set_execution_mode(PlanExecutionMode::PlanApproval);
        parent.stages.plan = Some(false);
        assert_eq!(ancestor_plan_pending(&parent, &db), Some("方案尚待人工批准"), "人工批准模式不因环节开关放行");

        parent.plan_phase = PlanPhase::Approved;
        parent.approved_plan_summary = Some("用户批准".into());
        assert!(ancestor_plan_pending(&parent, &db).is_none());
    }

    #[test]
    fn delegated_writes_wait_for_review_and_pause_again_on_rebuttal() {
        let db = setup_test_db();
        let context = SandwichContext::new("sys".into(), "goal".into());
        let mut parent = AgentCheckpoint::new("parent-review".into(), &context);
        parent.set_execution_mode(PlanExecutionMode::Auto);
        parent.triage_verdict = Some(technical_plan_verdict());
        parent.submitted_plan = Some(valid_submission());
        parent.plan_phase = PlanPhase::Reviewing;
        parent.plan_reviewer_retry_count = 1;
        seed_review(&db, "rev-a", &parent.run_id, "b-a", "running", false);
        parent.set_plan_review_batch("b-a", 1);
        assert_eq!(ancestor_plan_pending(&parent, &db), Some("方案审查尚未通过"), "审查进行中暂停写入");

        mark_succeeded(&db, "rev-a");
        parent.record_reviewer_delivery_for_run(&db, Some("rev-a"), &approved_delivery("approved"));
        assert!(ancestor_plan_pending(&parent, &db).is_none(), "审查通过即放行，不必等上级自己再写一次");
        assert!(!parent.plan_phase.is_approved(), "只读判定不改上级状态");

        let mut rejected = AgentCheckpoint::new("parent-rebuttal".into(), &context);
        rejected.set_execution_mode(PlanExecutionMode::Auto);
        rejected.submitted_plan = Some(valid_submission());
        rejected.plan_phase = PlanPhase::Reviewing;
        rejected.plan_reviewer_retry_count = 1;
        seed_review(&db, "rev-b", &rejected.run_id, "b-b", "running", false);
        rejected.set_plan_review_batch("b-b", 1);
        mark_succeeded(&db, "rev-b");
        rejected.record_reviewer_delivery_for_run(&db, Some("rev-b"), &approved_delivery("rebuttal"));
        assert_eq!(ancestor_plan_pending(&rejected, &db), Some("方案审查尚未通过"), "打回后暂停写入");
    }

    #[test]
    fn delegated_write_interception_walks_chain_and_reads_live_parent_state() {
        let dir = std::env::temp_dir().join(format!("delegated-plan-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = DbState(Arc::new(Mutex::new(crate::db::init_db(&dir.join("t.db")).unwrap())));
        let context = SandwichContext::new("sys".into(), "goal".into());
        let root = format!("root-{}", uuid::Uuid::new_v4());
        let mut cp = AgentCheckpoint::new(root.clone(), &context);
        cp.set_execution_mode(PlanExecutionMode::Auto);
        cp.triage_verdict = Some(technical_plan_verdict());
        crate::agent::checkpoint::save_checkpoint(&db, &mut cp).unwrap();

        let child = format!("task-{}", uuid::Uuid::new_v4());
        let grandchild = format!("task-{}", uuid::Uuid::new_v4());
        let binding = |parent: &str, child: &str| crate::agent::child_task::TaskBinding {
            parent_session: parent.into(), parent_run: format!("run-{parent}"),
            child_session: child.into(), child_run: format!("run-{child}"),
        };
        let g1 = crate::agent::child_task::TaskGuard::enter(binding(&root, &child));
        let g2 = crate::agent::child_task::TaskGuard::enter(binding(&child, &grandchild));

        assert!(delegated_write_interception(&db, &root).is_none(), "根任务仍走自己的原有门禁");
        for session in [&child, &grandchild] {
            let blocked = delegated_write_interception(&db, session).expect("上级方案未通过时暂停");
            assert_eq!(blocked.route, "plan_gatekeeper:parent_plan_pending");
            assert!(blocked.stderr.contains("只读调查"));
        }

        // 已在运行的子任务下次写入即读到新状态
        cp.submitted_plan = Some(valid_submission());
        cp.plan_phase = PlanPhase::Approved;
        crate::agent::checkpoint::save_checkpoint(&db, &mut cp).unwrap();
        assert!(delegated_write_interception(&db, &child).is_none());
        assert!(delegated_write_interception(&db, &grandchild).is_none());

        drop(g2);
        drop(g1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delegated_plan_status_prompt_tracks_ancestor_lifecycle_without_polluting_history() {
        let dir = std::env::temp_dir().join(format!("delegated-prompt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = DbState(Arc::new(Mutex::new(crate::db::init_db(&dir.join("t.db")).unwrap())));

        let root = format!("root-{}", uuid::Uuid::new_v4());
        let child = format!("task-{}", uuid::Uuid::new_v4());
        let grandchild = format!("task-{}", uuid::Uuid::new_v4());
        let binding = |parent: &str, child: &str| crate::agent::child_task::TaskBinding {
            parent_session: parent.into(),
            parent_run: format!("run-{parent}"),
            child_session: child.into(),
            child_run: format!("run-{child}"),
        };
        let g1 = crate::agent::child_task::TaskGuard::enter(binding(&root, &child));
        let g2 = crate::agent::child_task::TaskGuard::enter(binding(&child, &grandchild));

        let mut root_ctx = SandwichContext::new("sys".into(), "root goal".into());
        let mut root_cp = AgentCheckpoint::new(root.clone(), &root_ctx);
        root_cp.set_execution_mode(PlanExecutionMode::Auto);

        let mut child_ctx = SandwichContext::new("sys".into(), "child goal".into());
        let mut child_cp = AgentCheckpoint::new(child.clone(), &child_ctx);
        child_cp.stages.plan = Some(false);

        let mut grand_ctx = SandwichContext::new("sys".into(), "grandchild goal".into());
        let mut grand_cp = AgentCheckpoint::new(grandchild.clone(), &grand_ctx);
        grand_cp.stages.plan = Some(false);

        let render_assembled = |cp: &AgentCheckpoint, ctx: &mut SandwichContext, session: &str| {
            sync_plan_gate_projection(cp, ctx, &db, session);
            serde_json::to_string(&ctx.assemble_messages()).unwrap()
        };

        // 1. 上级 checkpoint 尚未落库（读取缺失 fail-open）：不虚称需等待方案，也不报错
        let rendered_missing = render_assembled(&grand_cp, &mut grand_ctx, &grandchild);
        assert!(!rendered_missing.contains("【上级方案状态】"));

        // 2. 根任务需要方案但尚未提交：子任务与孙任务均看到动态提示，根任务自身不显示子任务提示
        root_cp.triage_verdict = Some(technical_plan_verdict());
        crate::agent::checkpoint::save_checkpoint(&db, &mut root_cp).unwrap();
        crate::agent::checkpoint::save_checkpoint(&db, &mut child_cp).unwrap();

        let root_rendered = render_assembled(&root_cp, &mut root_ctx, &root);
        assert!(!root_rendered.contains("【上级方案状态】"), "根任务不应获得子任务提示");

        for (cp, ctx, session) in [
            (&child_cp, &mut child_ctx, child.as_str()),
            (&grand_cp, &mut grand_ctx, grandchild.as_str()),
        ] {
            let rendered = render_assembled(cp, ctx, session);
            assert!(rendered.contains("【上级方案状态】"), "{session} 应看到上级方案未通过提示");
            assert!(rendered.contains("已被判定需要方案，尚未提交"));
            assert!(rendered.contains("继续读取、搜索和分析"));
            assert!(rendered.contains("【方案门禁状态】"), "{session} 自身方案状态不应被遮住");
            assert!(rendered.contains("实际写入受上级方案门禁限制"));
            assert!(!rendered.contains("写入将自动放行"), "不应同时声称写入自动放行");
            assert!(!rendered.contains("🟢 门禁已放行"), "不应同时声称无条件放行");
            assert!(ctx.turns_history.is_empty(), "动态提示不得写入持久历史");
        }

        // 2.1 若子任务自身也开启了方案审查（cross_review），应同时看到【上级方案状态】与自身的 submit_plan 指引
        grand_cp.set_execution_mode(PlanExecutionMode::CrossReview);
        let grand_cross_review = render_assembled(&grand_cp, &mut grand_ctx, &grandchild);
        assert!(grand_cross_review.contains("【上级方案状态】"));
        assert!(grand_cross_review.contains("【方案门禁状态】🔍 交叉审查 (cross_review)"));
        assert!(grand_cross_review.contains("submit_plan"), "子任务自身的方案审查指引不得被上级提示遮住");
        grand_cp.set_execution_mode(PlanExecutionMode::Auto);

        // 3. 根任务方案通过并保存后：下一次组装立即移除【上级方案状态】，且历史中无残留旧提示
        root_cp.submitted_plan = Some(valid_submission());
        root_cp.plan_phase = PlanPhase::Approved;
        crate::agent::checkpoint::save_checkpoint(&db, &mut root_cp).unwrap();

        let grand_after_approve = render_assembled(&grand_cp, &mut grand_ctx, &grandchild);
        assert!(!grand_after_approve.contains("【上级方案状态】"), "通过后提示应立即消失");
        assert!(grand_after_approve.contains("【方案门禁状态】"));
        assert!(grand_ctx.turns_history.is_empty());

        // 4. 中间上级（子任务 B）开启方案审查且被打回（rebuttal）：孙任务 C 再次反映待通过状态
        child_cp.stages.plan = Some(true);
        child_cp.submitted_plan = Some(valid_submission());
        child_cp.plan_phase = PlanPhase::Reviewing;
        child_cp.plan_reviewer_retry_count = 1;
        let rev_id = crate::agent::child_reviewer::begin_child_run_extended(
            &db,
            "run-child",
            &child_cp.run_id,
            &dir,
            "mock",
            0,
            "review",
            Some(3),
            None,
            None,
            &crate::agent::child_reviewer::ChildRunIdentity {
                batch_id: Some("b-child".into()),
                ..crate::agent::child_reviewer::ChildRunIdentity::fresh("plan_review")
            },
        )
        .unwrap();
        child_cp.set_plan_review_batch("b-child", 1);
        mark_succeeded(&db, &rev_id);
        child_cp.record_reviewer_delivery_for_run(
            &db,
            Some(&rev_id),
            &approved_delivery("rebuttal"),
        );
        crate::agent::checkpoint::save_checkpoint(&db, &mut child_cp).unwrap();

        let grand_after_rebuttal = render_assembled(&grand_cp, &mut grand_ctx, &grandchild);
        assert!(grand_after_rebuttal.contains("【上级方案状态】"), "中间上级打回后孙任务应再次看到提示");
        assert!(grand_after_rebuttal.contains("方案审查尚未通过"));

        // 5. 子任务 B 方案关闭且清除活跃审查后：不再额外提示等待方案
        child_cp.stages.plan = Some(false);
        child_cp.submitted_plan = None;
        child_cp.plan_review_batch_id = None;
        child_cp.plan_review_expected_count = 0;
        crate::agent::checkpoint::save_checkpoint(&db, &mut child_cp).unwrap();

        let grand_after_disable = render_assembled(&grand_cp, &mut grand_ctx, &grandchild);
        assert!(!grand_after_disable.contains("【上级方案状态】"));

        drop(g2);
        drop(g1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delegation_exemption_only_covers_direct_writer_dispatch() {
        let parse = |value| serde_json::from_value::<crate::agent::types::AgentInstruction>(value).unwrap();
        for raw in [
            serde_json::json!({"tool":"child_writer", "params":{"task":"调查"}}),
            serde_json::json!({"tool":"core", "action":"child_writer", "dispatches":[{"task":"A"},{"task":"B"}]}),
        ] { assert!(is_delegation_dispatch_only(&parse(raw))); }
        for raw in [
            serde_json::json!({"tool":"child_writer", "action":"apply", "params":{"child_run_id":"run"}}),
            serde_json::json!({"tool":"child_writer", "params":{"action":"apply_diff"}}),
            serde_json::json!({"tool":"plugin/write_file", "action":"child_writer"}),
            serde_json::json!({"commands":[{"tool":"plugin/write_file", "action":"child_writer"}]}),
            serde_json::json!({"parallel":[{"tool":"child_writer"}]}),
            serde_json::json!({"plan_tree":{"parallel":[{"tool":"child_writer"}]}}),
            serde_json::json!({"tool":"child_writer", "command":{"commands":[{"tool":"local_file", "action":"write"}]}}),
        ] { assert!(!is_delegation_dispatch_only(&parse(raw.clone())), "{raw}"); }
    }

    #[test]
    fn delegated_gate_blocks_real_pipeline_writes_then_allows_after_approval() {
        let dir = std::env::temp_dir().join(format!("delegated-pipeline-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = DbState(Arc::new(Mutex::new(crate::db::init_db(&dir.join("test.db")).unwrap())));
        let context = SandwichContext::new("sys".into(), "goal".into());
        let root = format!("root-{}", uuid::Uuid::new_v4());
        let child = format!("task-{}", uuid::Uuid::new_v4());
        let mut cp = AgentCheckpoint::new(root.clone(), &context);
        cp.set_execution_mode(PlanExecutionMode::Auto);
        cp.triage_verdict = Some(technical_plan_verdict());
        crate::agent::checkpoint::save_checkpoint(&db, &mut cp).unwrap();
        let guard = crate::agent::child_task::TaskGuard::enter(crate::agent::child_task::TaskBinding {
            parent_session: root.clone(), parent_run: format!("run-{root}"),
            child_session: child.clone(), child_run: format!("run-{child}"),
        });
        let parse = |value| serde_json::from_value::<crate::agent::types::AgentInstruction>(value).unwrap();
        let inherited = parse(serde_json::json!({"tool":"local_file", "command":{"commands":[
            {"action":"write", "params":{"path":"out.txt", "content":"done"}}
        ]}}));
        let conflicting = parse(serde_json::json!({"commands":[
            {"tool":"plugin/write_file", "action":"child_writer", "params":{"path":"out.txt", "content":"bad"}}
        ]}));
        // 计划树无 tool 时以 action 为工具，实际动作来自参数。
        let tree = parse(serde_json::json!({"plan_tree":[
            {"action":"local_file", "params":{"action":"write", "path":"tree.txt", "content":"done"}}
        ]}));
        let parallel = parse(serde_json::json!({"parallel":[
            {"action":"local_file", "params":{"action":"write", "path":"parallel.txt", "content":"done"}}
        ]}));
        for inst in [&inherited, &conflicting, &tree, &parallel] {
            assert!(crate::agent::dispatcher::is_write_or_side_effect_tool(inst));
            assert_eq!(delegated_instruction_interception(&db, &child, inst).unwrap().route,
                "plan_gatekeeper:parent_plan_pending");
        }
        assert!(!dir.join("out.txt").exists());
        let readonly_tree = parse(serde_json::json!({"plan_tree":[
            {"action":"local_file", "params":{"action":"list", "path":"."}}
        ]}));
        assert!(!crate::agent::dispatcher::is_write_or_side_effect_tool(&readonly_tree));
        assert!(delegated_instruction_interception(&db, &child, &readonly_tree).is_none());
        let readonly = parse(serde_json::json!({"tool":"local_file", "params":{"action":"list", "path":"."}}));
        assert!(delegated_instruction_interception(&db, &child, &readonly).is_none());
        let dispatch = parse(serde_json::json!({"tool":"child_writer", "params":{"task":"investigate"}}));
        assert!(delegated_instruction_interception(&db, &child, &dispatch).is_none());

        cp.submitted_plan = Some(valid_submission());
        cp.plan_phase = PlanPhase::Approved;
        crate::agent::checkpoint::save_checkpoint(&db, &mut cp).unwrap();
        assert!(delegated_instruction_interception(&db, &child, &inherited).is_none());
        let result = crate::agent::dispatcher::run_agent_step(&db, &child, &inherited,
            &crate::agent::mcp::PluginRegistry::new(), Some(&dir));
        assert!(result.success, "{}", result.stderr);
        assert_eq!(std::fs::read_to_string(dir.join("out.txt")).unwrap(), "done");
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn complex_readonly_discussion_uses_existing_batch_gate_and_switches_win() {
        let context = SandwichContext::new("sys".into(), "跨模块测试".into());
        let mut cp = AgentCheckpoint::new("owner".into(), &context);
        let db = setup_test_db();
        let mut verdict = crate::agent::triage::TriageVerdict::fallback("offline");
        cp.triage_verdict = Some(verdict.clone());
        assert!(!plan_discussion_required(&cp), "fallback is not a complex verdict");
        verdict.fallback = None;
        cp.triage_verdict = Some(verdict.clone());
        assert!(plan_discussion_required(&cp));
        assert!(configured_plan_missing(&cp));
        let gate = cp.plan_gate_projection();
        assert!(crate::agent::context::render::ContextRenderer::render_plan_gate(Some(&gate)).contains("submit_plan"));
        let blocked = check_pre_write_gatekeeper(&mut cp, &context, &db, "parent", 10, false).unwrap();
        assert_eq!(blocked.route, "plan_gatekeeper:configured_plan_required");
        // A read-only plan is valid; saving it alone is not review approval.
        let plan = serde_json::from_value(serde_json::json!({"plan_summary":"范围、方法与验收", "target_files":[],"evidence_refs":["src/a.rs:1"]})).unwrap();
        assert!(validate_plan_submission(&plan, true).unwrap().0.is_empty());
        cp.submitted_plan = Some(plan);
        assert!(!configured_plan_missing(&cp));
        assert!(check_pre_write_gatekeeper(&mut cp, &context, &db, "parent", 11, false).is_some());
        cp.plan_phase = PlanPhase::Approved;
        assert!(check_pre_write_gatekeeper(&mut cp, &context, &db, "parent", 12, false).is_none());
        cp.stages.plan_review = Some(false);
        assert!(!plan_discussion_required(&cp));
        cp.stages.plan_review = None;
        cp.stages.plan = Some(false);
        assert!(!plan_discussion_required(&cp));
        cp.stages = Default::default();
        verdict.route = "direct".into();cp.triage_verdict=Some(verdict);
        assert!(!plan_discussion_required(&cp));
        assert!(!configured_plan_missing(&cp));
    }

    #[test]
    fn audit_turning_off_review_does_not_turn_off_required_plan() {
        let c=SandwichContext::new("sys".into(),"复杂测试".into());let mut cp=AgentCheckpoint::new("owner".into(),&c);
        let mut v=crate::agent::triage::TriageVerdict::fallback("fixture");v.fallback=None;cp.triage_verdict=Some(v);
        cp.stages.plan_review=Some(false);
        assert!(cp.plan_gate_projection().plan_required);
        assert!(configured_plan_missing(&cp));
        assert!(!plan_discussion_required(&cp));
        cp.stages.plan=Some(false);assert!(!configured_plan_missing(&cp));
    }
