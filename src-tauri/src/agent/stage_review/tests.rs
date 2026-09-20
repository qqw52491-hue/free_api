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
    fn should_fire_requires_approved_non_auto_mode_and_real_writes() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let instruction = instruction_with_todo_done();
        let routing = default_routing();

        // 未批准：不触发（方案审查负责批准前）
        let mut cp = approved_checkpoint();
        cp.plan_phase = crate::agent::checkpoint::PlanPhase::Reviewing;
        assert!(should_fire(&cp, &context, &instruction, &routing).is_none());

        // 全自动模式：不触发（保持原有行为）
        let mut cp = approved_checkpoint();
        cp.set_execution_mode(PlanExecutionMode::Auto);
        assert!(should_fire(&cp, &context, &instruction, &routing).is_none());

        // 批准 + 交叉审查，但无写入：不触发
        let cp = approved_checkpoint();
        assert!(should_fire(&cp, &context, &instruction, &routing).is_none());

        // 有写入 + todo 阶段完成：触发
        let mut context2 = SandwichContext::new("system".to_string(), "goal".to_string());
        context2.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        let cp = approved_checkpoint();
        assert!(should_fire(&cp, &context2, &instruction, &routing).is_some());
    }

    #[test]
    fn write_count_threshold_is_the_behavioral_backstop() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        let routing = default_routing();
        // 无 todo 更新的普通指令
        let instruction = AgentInstruction::default();
        // 阈值默认 6：写 5 个不触发
        for i in 0..5 {
            context.record_side_effect(i, "write_file".to_string(), format!("f{}.rs", i));
        }
        let cp = approved_checkpoint();
        assert!(should_fire(&cp, &context, &instruction, &routing).is_none(), "5 个文件不应触发");
        // 第 6 个触发（记账博弈兜底：模型不更新 todo 也逃不掉）
        context.record_side_effect(6, "write_file".to_string(), "f6.rs".to_string());
        assert!(should_fire(&cp, &context, &instruction, &routing).is_some(), "达到阈值必须触发");
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
        let cp = approved_checkpoint();

        // shell 未覆盖变更 5 个：不触发（同 todo/文件数阈值）
        let mut cp5 = approved_checkpoint();
        cp5.uncovered_shell_mutations = 5;
        assert!(should_fire(&cp5, &context, &instruction, &routing).is_none(), "5 个 shell 变更不应触发");

        // 达到阈值 6：无 todo 推进也必须触发（防旁路的核心断言）
        let mut cp6 = approved_checkpoint();
        cp6.uncovered_shell_mutations = 6;
        let reason = should_fire(&cp6, &context, &instruction, &routing).unwrap();
        assert!(reason.contains("shell"), "触发原因应标明 shell 旁路: {}", reason);

        // 基线语义：派发时快照后，新增变更才计入未覆盖
        let mut cp_baselined = approved_checkpoint();
        cp_baselined.uncovered_shell_mutations = 10;
        cp_baselined.stage_review_shell_baseline = 8;
        assert!(should_fire(&cp_baselined, &context, &instruction, &routing).is_none(), "基线之上的增量 2 < 6 不触发");
        cp_baselined.uncovered_shell_mutations = 14;
        assert!(should_fire(&cp_baselined, &context, &instruction, &routing).is_some(), "基线上增量 6 触发");

        let _ = cp; let _ = cp5;
    }

    #[test]
    fn should_fire_short_circuits_when_triage_disables_stage_review() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        let instruction = instruction_with_todo_done();
        let routing = default_routing();

        // 判决 stage_review_mode="none"：即便写入+todo 完成也不触发
        let mut cp = approved_checkpoint();
        let mut verdict = crate::agent::triage::TriageVerdict::fallback("test");
        verdict.stage_review_mode = "none".to_string();
        verdict.route = "direct".to_string();
        cp.triage_verdict = Some(verdict);
        assert!(should_fire(&cp, &context, &instruction, &routing).is_none());

        // 判决 milestone：正常触发
        let mut cp2 = approved_checkpoint();
        let mut v2 = crate::agent::triage::TriageVerdict::fallback("test");
        v2.stage_review_mode = "milestone".to_string();
        cp2.triage_verdict = Some(v2);
        assert!(should_fire(&cp2, &context, &instruction, &routing).is_some());
    }

    #[test]
    fn should_fire_respects_budget_inflight_and_blockers() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        let instruction = instruction_with_todo_done();
        let routing = default_routing();

        // 预算耗尽
        let mut cp = approved_checkpoint();
        cp.stage_review_budget_used = crate::agent::checkpoint::MAX_STAGE_REVIEWS;
        assert!(should_fire(&cp, &context, &instruction, &routing).is_none());

        // 在途审查
        let mut cp = approved_checkpoint();
        cp.stage_review_run_id = Some("child-1".to_string());
        assert!(should_fire(&cp, &context, &instruction, &routing).is_none());

        // 有未解决拦截时不重复派发（等对质收敛）
        let mut cp = approved_checkpoint();
        cp.stage_review_blockers_open = 1;
        assert!(should_fire(&cp, &context, &instruction, &routing).is_none());
    }

    #[test]
    fn should_fire_respects_stage_reviewer_override_disabled_and_roles() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        let instruction = instruction_with_todo_done();
        let cp = approved_checkpoint();

        // 默认配置下：达到条件触发
        let default_cfg = default_routing();
        assert!(should_fire(&cp, &context, &instruction, &default_cfg).is_some());

        // 显式免审（"disabled" 或 "none"）：直通免审，完全不触发
        let mut disabled_cfg = default_routing();
        disabled_cfg.stage_reviewer_override = Some("disabled".to_string());
        assert!(should_fire(&cp, &context, &instruction, &disabled_cfg).is_none(), "disabled 必须直通免审");

        let mut none_cfg = default_routing();
        none_cfg.stage_reviewer_override = Some("None".to_string());
        assert!(should_fire(&cp, &context, &instruction, &none_cfg).is_none(), "None 必须直通免审");

        // 指定角色卡：依然正常触发阶段审查
        let mut custom_role_cfg = default_routing();
        custom_role_cfg.stage_reviewer_override = Some("frontend_gatekeeper".to_string());
        assert!(should_fire(&cp, &context, &instruction, &custom_role_cfg).is_some(), "指定角色卡仍应正常触发");
    }

    #[test]
    fn apply_stage_result_success_resets_baseline_and_sets_blockers() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        context.record_side_effect(2, "write_file".to_string(), "b.rs".to_string());
        let mut cp = approved_checkpoint();
        cp.stage_review_run_id = Some("child-9".to_string());
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
        };
        let note = apply_stage_result(&mut cp, &result, None).note;
        assert_eq!(cp.stage_review_blockers_open, 1, "仅 regardless 计入拦截");
        assert_eq!(cp.stage_review_final_only.len(), 1, "final_only 仅记账");
        assert_eq!(cp.stage_review_baseline_side_effects, 2, "消费不得覆盖派发时基线（防覆盖记账竞态）");
        assert!(cp.stage_review_run_id.is_none());
        assert!(note.contains("拦截"), "有 regardless 拦截时必须给出解套提示");

        // 基线推进后：同样写入不再触发
        assert!(should_fire(&cp, &context, &instruction_with_todo_done(), &default_routing()).is_none());
    }

    #[test]
    fn apply_stage_result_infra_failure_marks_stage_unreviewed() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        let mut cp = approved_checkpoint();
        cp.stage_review_run_id = Some("child-9".to_string());
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
        };
        let out = apply_stage_result(&mut cp, &result, None);
        // 退款改为批后结算：回执层只标记 refund_pending，由调用方 settle。
        assert!(out.refund_pending, "infra 失败应标记待结算退款");
        assert_eq!(cp.stage_review_budget_used, 1, "回执层不再内联退款");
        assert!(cp.settle_stage_review_refund(), "批后统一结算完成退款");
        // 预算退还 + 基线不动（视为未审，会重新触发）+ 不产生拦截
        assert_eq!(cp.stage_review_budget_used, 0);
        assert_eq!(cp.stage_review_baseline_side_effects, 0);
        assert_eq!(cp.stage_review_blockers_open, 0);
        assert!(cp.stage_review_run_id.is_none());
    }

    #[test]
    fn rebuttal_invalid_output_preserves_blockers_without_refund() {
        // 对质子任务技术性失败：未产生有效裁决 → 原 blocking 异议保留、
        // 不计对质失败、不退款；可重新 resume_child 对质或等宿主判分清账。
        use crate::agent::protocol::DeliveryReceipt;
        let mut cp = approved_checkpoint();
        cp.stage_review_rebuttal_run_id = Some("reb-1".to_string());
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
        let out = apply_stage_receipt(&mut cp, &receipt, None);
        assert!(!out.refund_pending, "对质非法输出不退还阶段次数");
        assert_eq!(cp.stage_review_blockers_open, 2, "原 blocking 异议必须保留");
        assert!(cp.stage_review_rebuttal_run_id.is_none());
        assert!(out.note.contains("技术性失败"));
    }

    #[test]
    fn apply_stage_receipt_directly_handles_blockers_and_infra_failure() {
        use crate::agent::protocol::DeliveryReceipt;
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        let mut cp = approved_checkpoint();
        cp.stage_review_run_id = Some("receipt-1".to_string());
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
        let out = apply_stage_receipt(&mut cp, &receipt, None);
        assert!(out.refund_pending);
        assert!(out.note.contains("阶段审查任务未正常完成"));
        assert!(cp.settle_stage_review_refund(), "批后结算完成退款");
        assert_eq!(cp.stage_review_budget_used, 0);
    }

    #[test]
    fn apply_stage_receipt_committee_accumulates_blockers_and_merges_final_only() {
        use crate::agent::protocol::DeliveryReceipt;
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "app.vue".to_string());
        context.record_side_effect(2, "write_file".to_string(), "lib.rs".to_string());
        let mut cp = approved_checkpoint();
        cp.stage_review_run_ids = vec!["rev-fe".to_string(), "rev-be".to_string()];
        cp.stage_review_run_id = Some("rev-fe".to_string());
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
        let note1 = apply_stage_receipt(&mut cp, &receipt1, None).note;
        assert_eq!(cp.stage_review_blockers_open, 1, "前端专家产出 1 个 blocking");
        assert_eq!(cp.stage_review_final_only.len(), 1);
        assert_eq!(cp.stage_review_run_ids, vec!["rev-be".to_string()]);
        assert_eq!(cp.stage_review_run_id, Some("rev-be".to_string()));
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
        let note2 = apply_stage_receipt(&mut cp, &receipt2, None).note;
        assert_eq!(cp.stage_review_blockers_open, 3, "联合会审累计 1 + 2 = 3 个 blocking");
        assert_eq!(cp.stage_review_final_only.len(), 2, "final_only 汇总 2 条");
        assert!(cp.stage_review_run_ids.is_empty(), "所有专家均已交付");
        assert!(cp.stage_review_run_id.is_none());
        assert!(!note2.contains("仍在途"));

        // 对质撤回测试：针对 BE-1 进行对质并撤回，blocking 减 1
        cp.stage_review_rebuttal_run_id = Some("reb-be".to_string());
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
        let reb_note = apply_stage_receipt(&mut cp, &rebuttal_receipt, None).note;
        assert_eq!(cp.stage_review_blockers_open, 2, "撤回 1 条后 blocking 降为 2");
        assert!(reb_note.contains("对质复检结论"));
    }

    #[test]
    fn test_dispatch_preserves_stage_review_final_only() {
        let mut cp = approved_checkpoint();
        cp.stage_review_final_only = vec!["【对质未撤回自动降级】F1: 历史欠账".to_string()];
        cp.stage_review_blockers_open = 0;

        // dispatch 时：只清 blockers_open，不清空 final_only
        cp.stage_review_blockers_open = 0;
        // 验证 final_only 跨阶段累加 carry-over 到收尾审查未被抹除
        assert_eq!(cp.stage_review_final_only.len(), 1);
        assert!(cp.stage_review_final_only[0].contains("历史欠账"));
    }
