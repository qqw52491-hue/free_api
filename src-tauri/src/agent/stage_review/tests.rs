    use super::*;
    use crate::agent::checkpoint::PlanExecutionMode;
    use crate::agent::child_reviewer::{ChildReviewResult, ReviewFinding};
    use crate::agent::types::TodoItem;

    fn approved_checkpoint() -> AgentCheckpoint {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test_run".to_string(), &context);
        cp.set_execution_mode(PlanExecutionMode::CrossReview);
        cp.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
        cp
    }

    fn instruction_with_todo_done() -> AgentInstruction {
        AgentInstruction {
            todo_update: vec![TodoItem {
                id: 1,
                status: "done".to_string(),
                description: "阶段一".to_string(),
                evidence_output_id: None,
            }],
            ..Default::default()
        }
    }

    fn finding(id: &str, severity: &str, finality: &str) -> ReviewFinding {
        ReviewFinding {
            id: id.to_string(),
            severity: severity.to_string(),
            finality: finality.to_string(),
            location: String::new(),
            current: String::new(),
            why: "问题".to_string(),
            suggestion: String::new(),
            evidence_strength: "confirmed".to_string(),
        }
    }

    use crate::agent::router::ModelRoutingConfig;

    fn default_routing() -> ModelRoutingConfig {
        ModelRoutingConfig::default()
    }

    #[test]
    fn committee_choice_does_not_add_stage_review_triggers() {
        let mut context=SandwichContext::new("sys".into(),"goal".into());
        context.record_side_effect(1,"edit_file".into(),"src/a.rs".into());
        let mut cp=approved_checkpoint();
        let (root,db)=mk_db("people-trigger");
        let instruction=instruction_with_todo_done();
        let one=default_routing();let mut many=one.clone();many.stage_review_committee=true;
        assert_eq!(should_fire(&cp,&db,&context,&instruction,&one),should_fire(&cp,&db,&context,&instruction,&many));
        cp.set_execution_mode(PlanExecutionMode::Auto);
        assert!(should_fire(&cp,&db,&context,&instruction,&many).is_none());
        cp.set_execution_mode(PlanExecutionMode::CrossReview);
        let mut triage=crate::agent::triage::TriageVerdict::fallback("fixture");
        triage.stage_review_mode="none".into();cp.triage_verdict=Some(triage);
        assert!(should_fire(&cp,&db,&context,&instruction,&many).is_none());
        let _=std::fs::remove_dir_all(root);
    }

    /// ── 名单退役版台账夹具：阶段成员身份由 agent_child_runs 的
    /// batch_id+purpose+consumed_at 承担 ──
    fn mk_db(tag: &str) -> (std::path::PathBuf, crate::db::DbState) {
        let base = std::env::temp_dir().join(format!("stage-{}-{}", tag, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let state = crate::db::DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("t.db")).unwrap(),
        )));
        (base, state)
    }

    /// 插一条阶段批次成员行（consumed=true = 回执已被 poll 消费的生产时序）
    fn seed_stage_run(db: &crate::db::DbState, parent_run_id: &str, batch: &str, id: &str, purpose: &str, status: &str, consumed: bool) {
        db.0.lock().unwrap().execute(
            "INSERT INTO agent_child_runs (id, parent_run_id, parent_session_id, workspace_root, model_id, status, started_at, kind, task_id, attempt_id, purpose, batch_id, consumed_at)
             VALUES (?1, ?2, 'sess', '/tmp', 'm', ?3, 't', 'review', ?1, 1, ?4, ?5, ?6)",
            rusqlite::params![
                id, parent_run_id, status, purpose, batch,
                if consumed { Some("2024-01-01T00:00:00Z".to_string()) } else { None }
            ],
        ).unwrap();
    }

    /// 模拟"回执已被消费"（poll 先于 reducer 落 consumed_at）
    fn mark_consumed(db: &crate::db::DbState, id: &str, status: &str) {
        db.0.lock().unwrap().execute(
            "UPDATE agent_child_runs SET status=?1, consumed_at='2024-01-01T00:00:00Z' WHERE id=?2",
            rusqlite::params![status, id],
        ).unwrap();
    }

    #[test]
    fn should_fire_requires_approved_non_auto_mode_and_real_writes() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let instruction = instruction_with_todo_done();
        let routing = default_routing();
        let (_db_base, _db) = mk_db("t");

        // 未批准：不触发（方案审查负责批准前）
        let mut cp = approved_checkpoint();
        let (_db_base, db) = mk_db("t");
        cp.plan_phase = crate::agent::checkpoint::PlanPhase::Reviewing;
        assert!(should_fire(&cp, &db, &context, &instruction, &routing).is_none());

        // 全自动模式：不触发（保持原有行为）
        let mut cp = approved_checkpoint();
        let (_db_base, db) = mk_db("t");
        cp.set_execution_mode(PlanExecutionMode::Auto);
        assert!(should_fire(&cp, &db, &context, &instruction, &routing).is_none());

        // 批准 + 交叉审查，但无写入：不触发
        let cp = approved_checkpoint();
        let (_db_base, db) = mk_db("t");
        assert!(should_fire(&cp, &db, &context, &instruction, &routing).is_none());

        // 有写入 + todo 阶段完成：触发
        let mut context2 = SandwichContext::new("system".to_string(), "goal".to_string());
        context2.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        let cp = approved_checkpoint();
        let (_db_base, db) = mk_db("t");
        assert!(should_fire(&cp, &db, &context2, &instruction, &routing).is_some());
    }

    #[test]
    fn write_count_threshold_is_the_behavioral_backstop() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        let routing = default_routing();
        let (_db_base, _db) = mk_db("t");
        // 无 todo 更新的普通指令
        let instruction = AgentInstruction::default();
        // 阈值默认 6：写 5 个不触发
        for i in 0..5 {
            context.record_side_effect(i, "write_file".to_string(), format!("f{}.rs", i));
        }
        let cp = approved_checkpoint();
        let (_db_base, db) = mk_db("t");
        assert!(should_fire(&cp, &db, &context, &instruction, &routing).is_none(), "5 个文件不应触发");
        // 第 6 个触发（记账博弈兜底：模型不更新 todo 也逃不掉）
        context.record_side_effect(6, "write_file".to_string(), "f6.rs".to_string());
        assert!(should_fire(&cp, &db, &context, &instruction, &routing).is_some(), "达到阈值必须触发");
    }

    #[test]
    fn stage_evidence_never_exceeds_child_reviewer_budget() {
        let dir = std::env::temp_dir().join(format!("stage-ev-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git").args(args).current_dir(&dir).status().unwrap()
        };
        std::fs::write(dir.join("big.rs"), "fn base() {}\n").unwrap();
        assert!(git(&["init", "-q"]).success());
        assert!(git(&["-c", "user.email=t@t", "-c", "user.name=t", "add", "-A"]).success());
        assert!(git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init"]).success());
        // 已跟踪文件的修改才会进 git diff HEAD
        std::fs::write(dir.join("big.rs"), "x".repeat(40_000)).unwrap();

        let mut context = SandwichContext::new("system".to_string(), "目标".to_string());
        context.user_goal = "很长的目标".repeat(50);
        for i in 0..24 {
            context.todo_list.push(crate::agent::types::TodoItem {
                id: i,
                status: "done".to_string(),
                description: "很长的阶段任务描述".repeat(5),
                evidence_output_id: None,
            });
        }

        let evidence = build_stage_evidence(&context, &dir, None, &[]);
        assert!(
            evidence.chars().count() <= 6_000,
            "证据包超限会被 child_reviewer 解析层拒绝（实测教训）: {}",
            evidence.chars().count()
        );
        assert!(evidence.contains("【任务目标】"), "目标必须保留");
        assert!(evidence.contains("前段截断"), "大 diff 应截断保留尾部");

        // 小 diff 不截断
        std::fs::write(dir.join("big.rs"), "fn small() {}\n").unwrap();
        let evidence2 = build_stage_evidence(&context, &dir, None, &[]);
        assert!(!evidence2.contains("前段截断"));
        assert!(evidence2.chars().count() <= 6_000);

        // 验证 focus_context 注入且总长度仍受 6000 字符限制
        let focus_mat = "【关注领域重点】: 前端组件及可访问性规范".repeat(20);
        let evidence3 = build_stage_evidence(&context, &dir, Some(&focus_mat), &[]);
        assert!(evidence3.contains("【审查专家专项关注材料】"));
        assert!(evidence3.chars().count() <= 6_000);

        // extra_sections（产物清单/轨迹摘要）应进证据包且挤占 diff 配额仍不超限
        let extras = vec!["【会话 tmp 产物清单（本轮新增 1 个）】\n- tmp/x.csv (1 KB)".to_string()];
        let evidence4 = build_stage_evidence(&context, &dir, None, &extras);
        assert!(evidence4.contains("tmp/x.csv"), "产物清单应注入证据包");
        assert!(evidence4.chars().count() <= 6_000);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shell_batch_mutations_trigger_stage_review_independently() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let instruction = AgentInstruction::default();
        let routing = default_routing();
        let (_db_base, _db) = mk_db("t");
        let cp = approved_checkpoint();
        let (_db_base, _db) = mk_db("t");

        // shell 未覆盖变更 5 个：不触发（同 todo/文件数阈值）
        let mut cp5 = approved_checkpoint();
        let (_db_base, db) = mk_db("t");
        cp5.uncovered_shell_mutations = 5;
        assert!(should_fire(&cp5, &db, &context, &instruction, &routing).is_none(), "5 个 shell 变更不应触发");

        // 达到阈值 6：无 todo 推进也必须触发（防旁路的核心断言）
        let mut cp6 = approved_checkpoint();
        let (_db_base, db) = mk_db("t");
        cp6.uncovered_shell_mutations = 6;
        let reason = should_fire(&cp6, &db, &context, &instruction, &routing).unwrap();
        assert!(reason.contains("shell"), "触发原因应标明 shell 旁路: {}", reason);

        // 基线语义：派发时快照后，新增变更才计入未覆盖
        let mut cp_baselined = approved_checkpoint();
        let (_db_base, db) = mk_db("t");
        cp_baselined.uncovered_shell_mutations = 10;
        cp_baselined.stage_review_shell_baseline = 8;
        assert!(should_fire(&cp_baselined, &db, &context, &instruction, &routing).is_none(), "基线之上的增量 2 < 6 不触发");
        cp_baselined.uncovered_shell_mutations = 14;
        assert!(should_fire(&cp_baselined, &db, &context, &instruction, &routing).is_some(), "基线上增量 6 触发");

        let _ = cp; let _ = cp5;
    }

    #[test]
    fn should_fire_short_circuits_when_triage_disables_stage_review() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        let instruction = instruction_with_todo_done();
        let routing = default_routing();
        let (_db_base, _db) = mk_db("t");

        // 判决 stage_review_mode="none"：即便写入+todo 完成也不触发
        let mut cp = approved_checkpoint();
        let (_db_base, db) = mk_db("t");
        let mut verdict = crate::agent::triage::TriageVerdict::fallback("test");
        verdict.stage_review_mode = "none".to_string();
        verdict.route = "direct".to_string();
        cp.triage_verdict = Some(verdict);
        assert!(should_fire(&cp, &db, &context, &instruction, &routing).is_none());

        // 判决 milestone：正常触发
        let mut cp2 = approved_checkpoint();
        let (_db_base, db) = mk_db("t");
        let mut v2 = crate::agent::triage::TriageVerdict::fallback("test");
        v2.stage_review_mode = "milestone".to_string();
        cp2.triage_verdict = Some(v2);
        assert!(should_fire(&cp2, &db, &context, &instruction, &routing).is_some());
    }

    #[test]
    fn should_fire_respects_budget_inflight_and_blockers() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        let instruction = instruction_with_todo_done();
        let routing = default_routing();
        let (_db_base, _db) = mk_db("t");

        // 预算耗尽
        let mut cp = approved_checkpoint();
        let (_db_base, db) = mk_db("t");
        cp.stage_review_budget_used = crate::agent::checkpoint::MAX_STAGE_REVIEWS;
        assert!(should_fire(&cp, &db, &context, &instruction, &routing).is_none());

        // 在途审查：台账批次成员未消费 = 在途
        let mut cp = approved_checkpoint();
        let (_db_base, db) = mk_db("t");
        seed_stage_run(&db, &cp.run_id, "sb", "child-1", "stage_review", "running", false);
        cp.stage_review_batch_id = Some("sb".to_string());
        assert!(should_fire(&cp, &db, &context, &instruction, &routing).is_none());

        // 有未解决拦截时不重复派发（等对质收敛）
        let mut cp = approved_checkpoint();
        let (_db_base, db) = mk_db("t");
        cp.stage_review_blockers_open = 1;
        assert!(should_fire(&cp, &db, &context, &instruction, &routing).is_none());
    }

    #[test]
    fn should_fire_respects_stage_reviewer_override_disabled_and_roles() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        let instruction = instruction_with_todo_done();
        let cp = approved_checkpoint();
        let (_db_base, db) = mk_db("t");

        // 默认配置下：达到条件触发
        let default_cfg = default_routing();
        assert!(should_fire(&cp, &db, &context, &instruction, &default_cfg).is_some());

        // 显式免审（"disabled" 或 "none"）：直通免审，完全不触发
        let mut disabled_cfg = default_routing();
        disabled_cfg.stage_reviewer_override = Some("disabled".to_string());
        assert!(should_fire(&cp, &db, &context, &instruction, &disabled_cfg).is_none(), "disabled 必须直通免审");

        let mut none_cfg = default_routing();
        none_cfg.stage_reviewer_override = Some("None".to_string());
        assert!(should_fire(&cp, &db, &context, &instruction, &none_cfg).is_none(), "None 必须直通免审");

        // 指定角色卡：依然正常触发阶段审查
        let mut custom_role_cfg = default_routing();
        custom_role_cfg.stage_reviewer_override = Some("frontend_gatekeeper".to_string());
        assert!(should_fire(&cp, &db, &context, &instruction, &custom_role_cfg).is_some(), "指定角色卡仍应正常触发");
    }

    #[test]
    fn apply_stage_result_success_resets_baseline_and_sets_blockers() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        context.record_side_effect(2, "write_file".to_string(), "b.rs".to_string());
        let mut cp = approved_checkpoint();
        let (_db_base, db) = mk_db("t");
        // 台账夹具：阶段批次 sb 成员 child-9，回执已消费（poll 先标记的时序）
        seed_stage_run(&db, &cp.run_id, "sb", "child-9", "stage_review", "succeeded", true);
        cp.stage_review_batch_id = Some("sb".to_string());
        // 派发时基线已记录为 2（dispatch 时刻的台账长度）
        cp.stage_review_baseline_side_effects = 2;

        let result = ChildReviewResult {
            child_run_id: "child-9".to_string(),
            status: "succeeded".to_string(),
            findings: vec![
                finding("F1", "blocking", "regardless"),
                finding("F2", "blocking", "final_only"),
                finding("F3", "suggestion", "regardless"),
            ],
            confidence: "confirmed".to_string(),
            evidence_refs: vec![],
            missing_evidence: vec![],
            duration_ms: 100,
            completion_tokens: 10,
            prompt_tokens: 20,
            error_kind: None,
            raw_result_json: None,
            kind: None,
            routing: None,
            withdrawn_findings: vec![],
            objection_verdicts: vec![],
            scorecard: None,
            task_id: None,
            attempt_id: None,
        };
        let note = apply_stage_result(&mut cp, &db, &result, None).note;
        assert_eq!(cp.stage_review_blockers_open, 1, "仅 regardless 计入拦截");
        assert_eq!(cp.stage_review_final_only.len(), 1, "final_only 仅记账");
        assert_eq!(cp.stage_review_baseline_side_effects, 2, "消费不得覆盖派发时基线（防覆盖记账竞态）");
        assert!(!cp.stage_review_in_flight(&db), "成员已消费 → 无在途");
        assert!(note.contains("拦截"), "有 regardless 拦截时必须给出解套提示");

        // 基线推进后：同样写入不再触发
        assert!(should_fire(&cp, &db, &context, &instruction_with_todo_done(), &default_routing()).is_none());
    }

    #[test]
    fn apply_stage_result_infra_failure_marks_stage_unreviewed() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        let mut cp = approved_checkpoint();
        let (_db_base, db) = mk_db("t");
        // 台账夹具：成员 child-9 终态失败且回执已消费
        seed_stage_run(&db, &cp.run_id, "sb", "child-9", "stage_review", "timed_out", true);
        cp.stage_review_batch_id = Some("sb".to_string());
        cp.stage_review_budget_used = 1;

        let result = ChildReviewResult {
            child_run_id: "child-9".to_string(),
            status: "timed_out".to_string(),
            findings: vec![],
            confidence: "unverified".to_string(),
            evidence_refs: vec![],
            missing_evidence: vec![],
            duration_ms: 100,
            completion_tokens: 0,
            prompt_tokens: 0,
            error_kind: Some("timed_out".to_string()),
            raw_result_json: None,
            kind: None,
            routing: None,
            withdrawn_findings: vec![],
            objection_verdicts: vec![],
            scorecard: None,
            task_id: None,
            attempt_id: None,
        };
        let out = apply_stage_result(&mut cp, &db, &result, None);
        // 退款改为批后结算：回执层只标记 refund_pending，由调用方 settle。
        assert!(out.refund_pending, "infra 失败应标记待结算退款");
        assert_eq!(cp.stage_review_budget_used, 1, "回执层不再内联退款");
        assert!(cp.settle_stage_review_refund(&db), "批后统一结算完成退款");
        // 预算退还 + 基线不动（视为未审，会重新触发）+ 不产生拦截
        assert_eq!(cp.stage_review_budget_used, 0);
        assert_eq!(cp.stage_review_baseline_side_effects, 0);
        assert_eq!(cp.stage_review_blockers_open, 0);
        assert!(!cp.stage_review_in_flight(&db));
    }

    #[test]
    fn rebuttal_invalid_output_preserves_blockers_without_refund() {
        // 对质子任务技术性失败：未产生有效裁决 → 原 blocking 异议保留、
        // 不计对质失败、不退款；可重新 resume_child 对质或等宿主判分清账。
        use crate::agent::protocol::DeliveryReceipt;
        let mut cp = approved_checkpoint();
        let (_db_base, db) = mk_db("t");
        // 台账夹具：对质任务 purpose='rebuttal' 盖章，回执已消费
        seed_stage_run(&db, &cp.run_id, "sb", "reb-1", "rebuttal", "invalid_output", true);
        cp.stage_review_batch_id = Some("sb".to_string());
        cp.stage_review_blockers_open = 2;
        cp.stage_review_budget_used = 1;

        let receipt = DeliveryReceipt {
            child_run_id: "reb-1".to_string(),
            task_id: "reb-1".to_string(),
            attempt_id: 1,
            kind: "review".to_string(),
            status: "invalid_output".to_string(),
            duration_ms: 100,
            completion_tokens: 0,
            prompt_tokens: 0,
            error_kind: Some("invalid_output".to_string()),
            payload: None,
            raw_text: Some("{半截非法".to_string()),
        };
        let out = apply_stage_receipt(&mut cp, &db, &receipt, None);
        assert!(!out.refund_pending, "对质非法输出不退还阶段次数");
        assert_eq!(cp.stage_review_blockers_open, 2, "原 blocking 异议必须保留");
        assert!(!cp.stage_review_in_flight(&db), "对质行已消费 → 无在途");
        assert!(out.note.contains("技术性失败"));
    }

    #[test]
    fn apply_stage_receipt_directly_handles_blockers_and_infra_failure() {
        use crate::agent::protocol::DeliveryReceipt;
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        let mut cp = approved_checkpoint();
        let (_db_base, db) = mk_db("t");
        // 台账夹具：成员 receipt-1 终态超时且回执已消费
        seed_stage_run(&db, &cp.run_id, "sb", "receipt-1", "stage_review", "timed_out", true);
        cp.stage_review_batch_id = Some("sb".to_string());
        cp.stage_review_budget_used = 1;

        let receipt = DeliveryReceipt {
            child_run_id: "receipt-1".to_string(),
            task_id: "receipt-1".to_string(),
            attempt_id: 1,
            kind: "review".to_string(),
            status: "timed_out".to_string(),
            duration_ms: 60000,
            completion_tokens: 0,
            prompt_tokens: 0,
            error_kind: Some("timed_out".to_string()),
            payload: None,
            raw_text: None,
        };
        let out = apply_stage_receipt(&mut cp, &db, &receipt, None);
        assert!(out.refund_pending);
        assert!(out.note.contains("阶段审查任务未正常完成"));
        assert!(cp.settle_stage_review_refund(&db), "批后结算完成退款");
        assert_eq!(cp.stage_review_budget_used, 0);
    }

    #[test]
    fn apply_stage_receipt_committee_accumulates_blockers_and_merges_final_only() {
        use crate::agent::protocol::DeliveryReceipt;
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "app.vue".to_string());
        context.record_side_effect(2, "write_file".to_string(), "lib.rs".to_string());
        let mut cp = approved_checkpoint();
        let (_db_base, db) = mk_db("t");
        // 台账夹具：阶段批次 sb 两专家均在途（未消费）
        seed_stage_run(&db, &cp.run_id, "sb", "rev-fe", "stage_review", "running", false);
        seed_stage_run(&db, &cp.run_id, "sb", "rev-be", "stage_review", "running", false);
        cp.stage_review_batch_id = Some("sb".to_string());
        cp.stage_review_budget_used = 1;

        // 专家 1（前端）：发现 1 个 blocking，1 个 final_only
        let receipt1 = DeliveryReceipt {
            child_run_id: "rev-fe".to_string(),
            task_id: "stage_review_1_fe".to_string(),
            attempt_id: 1,
            kind: "review".to_string(),
            status: "succeeded".to_string(),
            duration_ms: 1200,
            completion_tokens: 150,
            prompt_tokens: 450,
            error_kind: None,
            payload: Some(serde_json::json!({
                "confidence": "confirmed",
                "findings": [
                    { "id": "FE-1", "severity": "blocking", "finality": "regardless", "why": "前端状态泄露" },
                    { "id": "FE-2", "severity": "blocking", "finality": "final_only", "why": "缺少可访问性 aria-label" }
                ],
                "withdrawn_findings": []
            })),
            raw_text: None,
        };
        // 回执消费时序：poll 先把 rev-fe 标为已消费，reducer 再处理
        mark_consumed(&db, "rev-fe", "succeeded");
        let note1 = apply_stage_receipt(&mut cp, &db, &receipt1, None).note;
        assert_eq!(cp.stage_review_blockers_open, 1, "前端专家产出 1 个 blocking");
        assert_eq!(cp.stage_review_final_only.len(), 1);
        assert_eq!(cp.stage_review_in_flight_remaining(&db), 1, "仅 rev-be 仍在途");
        assert!(note1.contains("联合会审仍在途，剩余 1 位专家"));

        // 专家 2（后端）：发现 2 个 blocking，1 个 final_only
        let receipt2 = DeliveryReceipt {
            child_run_id: "rev-be".to_string(),
            task_id: "stage_review_1_be".to_string(),
            attempt_id: 1,
            kind: "review".to_string(),
            status: "succeeded".to_string(),
            duration_ms: 1500,
            completion_tokens: 200,
            prompt_tokens: 500,
            error_kind: None,
            payload: Some(serde_json::json!({
                "confidence": "confirmed",
                "findings": [
                    { "id": "BE-1", "severity": "blocking", "finality": "regardless", "why": "SQL 注入隐患" },
                    { "id": "BE-2", "severity": "blocking", "finality": "regardless", "why": "并发死锁隐患" },
                    { "id": "BE-3", "severity": "blocking", "finality": "final_only", "why": "缺少索引日志" }
                ],
                "withdrawn_findings": []
            })),
            raw_text: None,
        };
        mark_consumed(&db, "rev-be", "succeeded");
        let note2 = apply_stage_receipt(&mut cp, &db, &receipt2, None).note;
        assert_eq!(cp.stage_review_blockers_open, 3, "联合会审累计 1 + 2 = 3 个 blocking");
        assert_eq!(cp.stage_review_final_only.len(), 2, "final_only 汇总 2 条");
        assert!(!cp.stage_review_in_flight(&db), "所有专家均已交付");
        assert!(!note2.contains("仍在途"));

        // 对质撤回测试：针对 BE-1 进行对质并撤回，blocking 减 1
        // 对质任务 = 台账行 purpose='rebuttal' + 同批次盖章（resume 落库时写入）
        seed_stage_run(&db, &cp.run_id, "sb", "reb-be", "rebuttal", "running", false);
        db.0.lock().unwrap().execute(
            "UPDATE agent_child_runs SET task_id='rev-be',attempt_id=2 WHERE id='reb-be'", [],
        ).unwrap();
        let rebuttal_receipt = DeliveryReceipt {
            child_run_id: "reb-be".to_string(),
            task_id: "stage_rebuttal_1".to_string(),
            attempt_id: 1,
            kind: "review".to_string(),
            status: "succeeded".to_string(),
            duration_ms: 800,
            completion_tokens: 80,
            prompt_tokens: 300,
            error_kind: None,
            payload: Some(serde_json::json!({
                "confidence": "confirmed",
                "findings": [
                    { "id": "BE-2", "severity": "blocking", "finality": "regardless", "why": "并发死锁隐患" }
                ],
                "withdrawn_findings": ["BE-1"],
                "objection_verdicts": [
                    { "finding_id": "BE-1", "verdict": "withdraw", "reason": "父 Agent 提供的参数化查询证据充分" }
                ]
            })),
            raw_text: None,
        };
        mark_consumed(&db, "reb-be", "succeeded");
        let reb_note = apply_stage_receipt(&mut cp, &db, &rebuttal_receipt, None).note;
        assert_eq!(cp.stage_review_blockers_open, 2, "撤回 1 条后 blocking 降为 2");
        assert!(reb_note.contains("对质复检结论"));
    }

    #[test]
    fn test_dispatch_preserves_stage_review_final_only() {
        let mut cp = approved_checkpoint();
        let (_db_base, _db) = mk_db("t");
        cp.stage_review_final_only = vec!["【对质未撤回自动降级】F1: 历史欠账".to_string()];
        cp.stage_review_blockers_open = 0;

        // dispatch 时：只清 blockers_open，不清空 final_only
        cp.stage_review_blockers_open = 0;
        // 验证 final_only 跨阶段累加 carry-over 到收尾审查未被抹除
        assert_eq!(cp.stage_review_final_only.len(), 1);
        assert!(cp.stage_review_final_only[0].contains("历史欠账"));
    }

    // 用真实台账身份和阶段回执入口覆盖不同执行 ID 的连续对质。
    fn owned_stage_receipt(
        db: &crate::db::DbState, cp: &AgentCheckpoint, id: &str,
        task: &str, attempt: u32, rebuttal: bool, findings: serde_json::Value,
        withdrawn: &[&str],
    ) -> crate::agent::protocol::DeliveryReceipt {
        seed_stage_run(db, &cp.run_id, "owned-stage", id,
            if rebuttal { "rebuttal" } else { "stage_review" }, "succeeded", true);
        let payload = json!({"confidence":"confirmed","findings":findings,
            "withdrawn_findings":withdrawn,"objection_verdicts":[]});
        db.0.lock().unwrap().execute(
            "UPDATE agent_child_runs SET task_id=?1,attempt_id=?2,result_json=?3 WHERE id=?4",
            rusqlite::params![task,attempt,payload.to_string(),id],
        ).unwrap();
        crate::agent::protocol::DeliveryReceipt {
            child_run_id:id.into(), task_id:task.into(), attempt_id:attempt,
            kind:"review".into(), status:"succeeded".into(), duration_ms:1,
            completion_tokens:1,prompt_tokens:1,error_kind:None,
            payload:Some(payload),raw_text:None,
        }
    }
    fn owned_blocking() -> serde_json::Value {
        json!([{"id":"F1","severity":"blocking","finality":"regardless","why":"仍需修复的问题"}])
    }

    #[test]
    fn owned_stage_repeat_withdrawal_cannot_remove_another_members_f1() {
        let (base, db)=mk_db("owned-withdraw");
        let mut cp=approved_checkpoint();
        cp.stage_review_batch_id=Some("owned-stage".into());
        for (id,task) in [("a","task-a"),("b","task-b")] {
            let r=owned_stage_receipt(&db,&cp,id,task,1,false,owned_blocking(),&[]);
            apply_stage_receipt(&mut cp,&db,&r,None);
        }
        assert_eq!(cp.stage_review_blockers_open,2);
        for attempt in [2,3] {
            let r=owned_stage_receipt(&db,&cp,&format!("a-{attempt}"),"task-a",attempt,true,json!([]),&["F1","F99"]);
            apply_stage_receipt(&mut cp,&db,&r,None);
            assert_eq!(cp.stage_review_blockers_open,1,"重复/未知撤回不能扣掉 B 的问题");
        }
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn owned_stage_different_run_ids_share_task_counter_and_preserve_other_members() {
        let (base,db)=mk_db("owned-rounds");
        let mut cp=approved_checkpoint();
        cp.stage_review_batch_id=Some("owned-stage".into());
        for (id,task) in [("a","task-a"),("b","task-b")] {
            let r=owned_stage_receipt(&db,&cp,id,task,1,false,owned_blocking(),&[]);
            apply_stage_receipt(&mut cp,&db,&r,None);
        }
        for (id,task,attempt) in [("a-2","task-a",2),("b-2","task-b",2),("a-3","task-a",3)] {
            let r=owned_stage_receipt(&db,&cp,id,task,attempt,true,owned_blocking(),&[]);
            apply_stage_receipt(&mut cp,&db,&r,None);
        }
        assert_eq!(cp.stage_review_blockers_open,1,"A 连续两次有效对质应只移交 A 的问题");
        assert!(cp.stage_review_final_only.iter().any(|s| s.contains("F1")));
        let r=owned_stage_receipt(&db,&cp,"b-3","task-b",3,true,owned_blocking(),&[]);
        apply_stage_receipt(&mut cp,&db,&r,None);
        assert_eq!(cp.stage_review_blockers_open,0,"B 的独立计数也应保留");
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn owned_stage_checkpoint_restore_and_late_receipts_do_not_repeat_rounds() {
        let (base,db)=mk_db("owned-restore");
        let mut cp=approved_checkpoint();
        cp.stage_review_batch_id=Some("owned-stage".into());
        let initial=owned_stage_receipt(&db,&cp,"initial","task",1,false,owned_blocking(),&[]);
        apply_stage_receipt(&mut cp,&db,&initial,None);
        let second=owned_stage_receipt(&db,&cp,"second","task",2,true,owned_blocking(),&[]);
        apply_stage_receipt(&mut cp,&db,&second,None);
        apply_stage_receipt(&mut cp,&db,&second,None);
        assert_eq!(cp.stage_review_blockers_open,1,"重复回执不算第二轮对质");
        cp=serde_json::from_value(serde_json::to_value(&cp).unwrap()).unwrap();
        let third=owned_stage_receipt(&db,&cp,"third","task",3,true,owned_blocking(),&[]);
        apply_stage_receipt(&mut cp,&db,&third,None);
        assert_eq!(cp.stage_review_blockers_open,0);
        let lines=cp.stage_review_final_only.clone();
        apply_stage_receipt(&mut cp,&db,&initial,None);
        assert_eq!(cp.stage_review_blockers_open,0,"迟到回执不得恢复旧阻塞");
        assert_eq!(cp.stage_review_final_only,lines,"重放不重复追加移交项");
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn owned_stage_old_checkpoint_rehydrates_known_ownership_but_does_not_guess_unknowns() {
        let (base,db)=mk_db("owned-legacy");
        let mut cp=approved_checkpoint();
        cp.stage_review_batch_id=Some("owned-stage".into());
        for (id,task) in [("a","task-a"),("b","task-b")] {
            let r=owned_stage_receipt(&db,&cp,id,task,1,false,owned_blocking(),&[]);
            apply_stage_receipt(&mut cp,&db,&r,None);
        }
        let mut saved=serde_json::to_value(&cp).unwrap();
        saved.as_object_mut().unwrap().remove("stage_review_state");
        cp=serde_json::from_value(saved).unwrap();
        let r=owned_stage_receipt(&db,&cp,"a-withdraw","task-a",2,true,json!([]),&["F1"]);
        apply_stage_receipt(&mut cp,&db,&r,None);
        assert_eq!(cp.stage_review_blockers_open,1,"旧记录中 A/B 的归属可从台账恢复");
        // 只有一个旧计数、没有可核实的旧发现时，撤回不得凭空扣减。
        let mut unknown=approved_checkpoint();
        unknown.stage_review_batch_id=Some("owned-stage".into());
        unknown.stage_review_blockers_open=5;
        let r=owned_stage_receipt(&db,&unknown,"unknown","task-x",2,true,json!([]),&["F1","missing"]);
        let note=apply_stage_receipt(&mut unknown,&db,&r,None).note;
        assert_eq!(unknown.stage_review_blockers_open,5);
        assert!(note.contains("来源尚无法完整还原"));
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn owned_stage_omission_failure_and_explicit_downgrade_have_distinct_effects() {
        let (base,db)=mk_db("owned-updates");
        let mut cp=approved_checkpoint();
        cp.stage_review_batch_id=Some("owned-stage".into());
        let r=owned_stage_receipt(&db,&cp,"a","task-a",1,false,owned_blocking(),&[]);
        apply_stage_receipt(&mut cp,&db,&r,None);
        let omitted=owned_stage_receipt(&db,&cp,"a-2","task-a",2,true,json!([]),&[]);
        apply_stage_receipt(&mut cp,&db,&omitted,None);
        assert_eq!(cp.stage_review_blockers_open,1,"漏写不等于撤回");
        let mut failure=owned_stage_receipt(&db,&cp,"a-3","task-a",3,true,json!([]),&["F1"]);
        failure.status="cancelled".into(); // 无 error_kind 的取消也不构成有效审查
        db.0.lock().unwrap().execute("UPDATE agent_child_runs SET status='cancelled' WHERE id='a-3'", []).unwrap();
        apply_stage_receipt(&mut cp,&db,&failure,None);
        assert_eq!(cp.stage_review_blockers_open,1,"失败载荷不得解除阻塞或累计对质");
        let suggestion=json!([{"id":"F1","severity":"suggestion","finality":"regardless","why":"仅作建议"}]);
        let downgraded=owned_stage_receipt(&db,&cp,"a-4","task-a",4,true,suggestion,&[]);
        apply_stage_receipt(&mut cp,&db,&downgraded,None);
        assert_eq!(cp.stage_review_blockers_open,0);
        assert!(cp.stage_review_final_only.is_empty(),"失败不算一次有效维持");
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn owned_stage_old_batch_and_cleared_receipts_cannot_reopen_blockers() {
        let (base,db)=mk_db("owned-clear");
        let mut cp=approved_checkpoint();
        cp.stage_review_batch_id=Some("owned-stage".into());
        let r=owned_stage_receipt(&db,&cp,"a","task-a",1,false,owned_blocking(),&[]);
        apply_stage_receipt(&mut cp,&db,&r,None);
        cp.clear_stage_review_blockers(); // 人工放行与宿主旧阶段清账共用此入口
        apply_stage_receipt(&mut cp,&db,&r,None);
        assert_eq!(cp.stage_review_blockers_open,0);
        cp.stage_review_batch_id=Some("next-stage".into());
        let note=apply_stage_receipt(&mut cp,&db,&r,None).note;
        assert_eq!(cp.stage_review_blockers_open,0);
        assert!(note.contains("旧阶段批次"));
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn owned_stage_out_of_order_withdrawal_and_new_finding_use_the_same_fact_view() {
        let (base,db)=mk_db("owned-order");
        let mut cp=approved_checkpoint();
        cp.stage_review_batch_id=Some("owned-stage".into());
        let first=owned_stage_receipt(&db,&cp,"a-1","task-a",1,false,owned_blocking(),&[]);
        apply_stage_receipt(&mut cp,&db,&first,None);
        let third=owned_stage_receipt(&db,&cp,"a-3","task-a",3,true,json!([]),&["F1"]);
        apply_stage_receipt(&mut cp,&db,&third,None);
        let second=owned_stage_receipt(&db,&cp,"a-2","task-a",2,true,owned_blocking(),&[]);
        apply_stage_receipt(&mut cp,&db,&second,None);
        assert_eq!(cp.stage_review_blockers_open,0,"晚到旧维持不能推翻较新的撤回");
        assert!(cp.stage_review_final_only.is_empty());

        let new_finding=json!([{"id":"F2","severity":"blocking","finality":"regardless","why":"对质新发现"}]);
        let fourth=owned_stage_receipt(&db,&cp,"a-4","task-a",4,true,new_finding,&[]);
        apply_stage_receipt(&mut cp,&db,&fourth,None);
        assert_eq!(cp.stage_review_blockers_open,1,"对质新增的问题不能静默丢掉");
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn owned_stage_new_finding_does_not_borrow_older_findings_rounds() {
        let (base, db) = mk_db("owned-new-rounds");
        let mut cp = approved_checkpoint();
        cp.stage_review_batch_id = Some("owned-stage".into());
        for attempt in [1, 2] {
            let r = owned_stage_receipt(&db, &cp, &format!("a-{attempt}"), "task-a", attempt,
                attempt > 1, owned_blocking(), &[]);
            apply_stage_receipt(&mut cp, &db, &r, None);
        }
        let both = json!([
            {"id":"F1","severity":"blocking","why":"原有问题"},
            {"id":"F2","severity":"blocking","why":"本轮新发现"}
        ]);
        let r = owned_stage_receipt(&db, &cp, "a-3", "task-a", 3, true, both, &[]);
        apply_stage_receipt(&mut cp, &db, &r, None);
        assert_eq!(cp.stage_review_blockers_open, 1, "F2 首次出现，不能借 F1 的两轮对质移交");
        assert!(!cp.stage_review_final_only.iter().any(|s| s.contains("F2:")));
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn owned_stage_omitted_findings_are_preserved_without_counting_as_maintained() {
        let (base, db) = mk_db("owned-omitted-rounds");
        let mut cp = approved_checkpoint();
        cp.stage_review_batch_id = Some("owned-stage".into());
        let r = owned_stage_receipt(&db, &cp, "a-1", "task-a", 1, false, owned_blocking(), &[]);
        apply_stage_receipt(&mut cp, &db, &r, None);
        for attempt in [2, 3] {
            let r = owned_stage_receipt(&db, &cp, &format!("a-{attempt}"), "task-a", attempt,
                true, json!([]), &[]);
            apply_stage_receipt(&mut cp, &db, &r, None);
        }
        assert_eq!(cp.stage_review_blockers_open, 1, "没有提及不等于两轮明确维持");
        assert!(cp.stage_review_final_only.is_empty());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn owned_stage_explicit_maintain_without_repeating_finding_counts_and_withdrawal_resets_it() {
        let (base, db) = mk_db("owned-explicit-rounds");
        let mut cp = approved_checkpoint();
        cp.stage_review_batch_id = Some("owned-stage".into());
        let r = owned_stage_receipt(&db, &cp, "a-1", "task-a", 1, false, owned_blocking(), &[]);
        apply_stage_receipt(&mut cp, &db, &r, None);
        for attempt in [2, 3] {
            let mut r = owned_stage_receipt(&db, &cp, &format!("a-{attempt}"), "task-a", attempt,
                true, json!([]), &[]);
            r.payload.as_mut().unwrap()["objection_verdicts"] = json!([
                {"finding_id":"F1","verdict":"maintain","reason":"原问题仍然存在"}
            ]);
            apply_stage_receipt(&mut cp, &db, &r, None);
            assert_eq!(cp.stage_review_blockers_open, if attempt == 2 { 1 } else { 0 });
        }
        let r = owned_stage_receipt(&db, &cp, "a-4", "task-a", 4, true, json!([]), &["F1"]);
        apply_stage_receipt(&mut cp, &db, &r, None);
        let r = owned_stage_receipt(&db, &cp, "a-5", "task-a", 5, true, owned_blocking(), &[]);
        apply_stage_receipt(&mut cp, &db, &r, None);
        assert_eq!(cp.stage_review_blockers_open, 1, "撤回后再次明确报告的问题不能继承此前的移交豁免");
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn owned_stage_final_only_same_text_has_separate_owners_and_withdraws_locally() {
        let (base, db) = mk_db("owned-final-withdraw");
        let mut cp = approved_checkpoint();
        cp.stage_review_batch_id = Some("owned-stage".into());
        let final_only = json!([{"id":"F1","severity":"blocking","finality":"final_only","why":"相同文字"}]);
        for task in ["task-a", "task-b"] {
            let r = owned_stage_receipt(&db, &cp, task, task, 1, false, final_only.clone(), &[]);
            apply_stage_receipt(&mut cp, &db, &r, None);
        }
        assert_eq!(cp.stage_review_final_only.len(), 2, "相同 F1/文字不能抹掉独立来源");
        for (id, task, remaining) in [("a-2", "task-a", 1), ("b-2", "task-b", 0)] {
            let r = owned_stage_receipt(&db, &cp, id, task, 2, true, json!([]), &["F1"]);
            apply_stage_receipt(&mut cp, &db, &r, None);
            assert_eq!(cp.stage_review_final_only.len(), remaining, "明确撤回要更新当前移交项");
        }
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn owned_stage_auto_deferred_withdrawal_removes_current_carry_after_restore() {
        let (base, db) = mk_db("owned-deferred-withdraw");
        let mut cp = approved_checkpoint();
        cp.stage_review_batch_id = Some("owned-stage".into());
        for attempt in [1, 2, 3] {
            let r = owned_stage_receipt(&db, &cp, &format!("a-{attempt}"), "task-a", attempt,
                attempt > 1, owned_blocking(), &[]);
            apply_stage_receipt(&mut cp, &db, &r, None);
        }
        assert_eq!(cp.stage_review_final_only.len(), 1);
        cp = serde_json::from_value(serde_json::to_value(&cp).unwrap()).unwrap();
        let r = owned_stage_receipt(&db, &cp, "a-4", "task-a", 4, true, json!([]), &["F1"]);
        apply_stage_receipt(&mut cp, &db, &r, None);
        assert!(cp.stage_review_final_only.is_empty(), "撤回后不能继续把旧欠账作为待处理意见交终审");
        assert_eq!(cp.stage_review_blockers_open, 0);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn owned_stage_carry_survives_stage_clear_and_unbound_legacy_is_disclosed() {
        let (base, db) = mk_db("owned-carry-clear");
        let mut cp = approved_checkpoint();
        cp.stage_review_batch_id = Some("owned-stage".into());
        let f = json!([{"id":"F1","severity":"blocking","finality":"final_only","why":"旧阶段仍需核对"}]);
        let first = owned_stage_receipt(&db, &cp, "a-1", "task-a", 1, false, f, &[]);
        apply_stage_receipt(&mut cp, &db, &first, None);
        let carried = cp.stage_review_final_only.clone();
        cp.clear_stage_review_blockers();
        cp.stage_review_batch_id = Some("next-stage".into());
        let new = owned_stage_receipt(&db, &cp, "b-1", "task-b", 1, false, owned_blocking(), &[]);
        db.0.lock().unwrap().execute("UPDATE agent_child_runs SET batch_id='next-stage' WHERE id='b-1'", []).unwrap();
        apply_stage_receipt(&mut cp, &db, &new, None);
        assert_eq!(cp.stage_review_blockers_open, 1);
        assert_eq!(cp.stage_review_final_only, carried, "跨阶段清阻塞不能丢失仍有效的移交项");
        let mut saved = serde_json::to_value(&cp).unwrap();
        saved["stage_review_state"].as_object_mut().unwrap().remove("final_only_projection");
        cp = serde_json::from_value(saved).unwrap();
        cp.stage_review_final_only.push("F1: 无法确定归属的旧文字".into());
        let note = apply_stage_receipt(&mut cp, &db, &new, None).note;
        assert!(note.contains("历史参考"));
        assert!(cp.stage_review_final_only.iter().any(|s| s.contains("来源未逐项绑定") && s.contains("无法确定归属")));
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn owned_stage_all_receipt_orders_produce_the_same_current_opinions() {
        let (base, db) = mk_db("owned-permutations");
        let mut seed = approved_checkpoint();
        seed.stage_review_batch_id = Some("owned-stage".into());
        let mut receipts = Vec::new();
        for attempt in [1, 2, 3] {
            receipts.push(owned_stage_receipt(&db, &seed, &format!("a-{attempt}"), "task-a", attempt,
                attempt > 1, owned_blocking(), &[]));
        }
        receipts.push(owned_stage_receipt(&db, &seed, "a-4", "task-a", 4, true, json!([]), &["F1"]));
        receipts.push(owned_stage_receipt(&db, &seed, "b-1", "task-b", 1, false,
            json!([{"id":"F1","severity":"blocking","finality":"final_only","why":"B 独立移交"}]), &[]));
        receipts.push(owned_stage_receipt(&db, &seed, "c-1", "task-c", 1, false, owned_blocking(), &[]));
        fn permutations(items: &mut [usize], at: usize, visit: &mut impl FnMut(&[usize])) {
            if at == items.len() { visit(items); return; }
            for i in at..items.len() {
                items.swap(at, i);
                permutations(items, at + 1, visit);
                items.swap(at, i);
            }
        }
        let mut checked = 0;
        permutations(&mut [0, 1, 2, 3, 4, 5], 0, &mut |order| {
            let mut cp = seed.clone();
            for (position, index) in order.iter().enumerate() {
                apply_stage_receipt(&mut cp, &db, &receipts[*index], None);
                if position == 2 {
                    cp = serde_json::from_value(serde_json::to_value(&cp).unwrap()).unwrap();
                }
            }
            assert_eq!(cp.stage_review_blockers_open, 1, "order={order:?}");
            assert_eq!(cp.stage_review_final_only.len(), 1, "order={order:?}");
            assert!(cp.stage_review_final_only[0].contains("task-b"), "order={order:?}");
            let settled = cp.stage_review_final_only.clone();
            for receipt in &receipts {
                apply_stage_receipt(&mut cp, &db, receipt, None);
            }
            assert_eq!(cp.stage_review_final_only, settled, "重放不能复活 A 的旧移交");
            assert_eq!(cp.stage_review_blockers_open, 1);
            checked += 1;
        });
        assert_eq!(checked, 720);
        std::fs::remove_dir_all(base).unwrap();
    }
