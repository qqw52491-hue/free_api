use super::*;
use crate::agent::child_reviewer::retirement_tests::MockReviewer;

fn pass() -> serde_json::Value {
    json!({"findings":[],"confidence":"supported","evidence_refs":[],"missing_evidence":[],
        "root_cause_verdict":"explains","decision":"approved",
        "revised_plan_summary":"不应按到达先后覆盖原方案"})
}

#[tokio::test]
async fn plan_team_spawns_three_one_batch_and_keeps_independent_blocker_and_original_plan() {
    let mut blocked = pass();
    blocked["decision"] = json!("rebuttal");
    blocked["findings"] = json!([{"id":"F1","severity":"blocking","location":"src/a.rs:1",
        "current":"未处理旧调用","why":"调用方仍按旧接口读取","suggestion":"补齐调用方修改","evidence_strength":"supported"}]);
    let mock = MockReviewer::concurrent(vec![pass(), blocked, pass()], 3).await;
    let cards = crate::agent::review_team::tests::seed_cards(&mock.db, 3);
    let mut config = crate::agent::router::ModelRoutingConfig::default();
    config.stage_review_committee = true;
    config.role_models.insert("reviewer".into(), "mock".into());
    let mut context = crate::agent::context::SandwichContext::new("sys".into(), "目标".into());
    let routing =
        crate::agent::router::AgentRouter::route_for_role(&context, &config, 1, "reviewer");
    let primary = PlanReviewerDispatchSnapshot::new(
        "mock",
        "flash",
        "auto",
        "PRIMARY",
        json!({"question":"核查方案是否解决题面问题","plan_review":true,"evidence_refs":[],
            "evidence_excerpt":"合成需求：接口由旧参数改为新参数；提交方案须同步检查真实调用方。",
            "role":"review_0","role_label":"独立专家0"}),
    )
    .with_purpose("plan_review")
    .with_batch_id("panel-batch");
    let snapshots =
        prepare_snapshots(&mock.db, &mock.root, &config, &routing, primary, &cards).await;
    let submission =
        serde_json::from_value(json!({"plan_summary":"原始提交方案，核查新旧接口与真实调用方"}))
            .unwrap();
    let mut cp = AgentCheckpoint::new("panel-session".into(), &context);
    cp.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::CrossReview);
    let (_tx, rx) = watch::channel(0);
    let result = dispatch_plan_team(
        None,
        &mock.db,
        "panel-run",
        "panel-session",
        &mock.root,
        &mut cp,
        &submission,
        1,
        1,
        false,
        true,
        snapshots,
        rx,
    )
    .await;
    assert!(result.success, "{}", result.stderr);
    assert!(result.stdout.contains("实际启动 3 位"));
    assert_eq!(cp.plan_review_expected_count, 3);
    assert_eq!(cp.plan_reviewer_retry_count, 1, "每批只算一轮");
    let ids: Vec<String> = {
        let conn = mock.db.0.lock().unwrap();
        let mut stmt=conn.prepare(
        "SELECT id FROM agent_child_runs WHERE parent_run_id='panel-run' ORDER BY started_at").unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(ids.len(), 3);
    for id in &ids {
        assert_eq!(mock.wait_terminal(id).await.0, "succeeded");
    }
    let (_, rx) = watch::channel(0);
    let env = crate::agent::child_receipt::ReceiptEnv {
        app: None,
        db: &mock.db,
        parent_run_id: "panel-run",
        session_id: "panel-session",
        workspace_dir: &mock.root,
        interrupt_rx: rx,
        reviewer_backup_model: "",
    };
    for (index, id) in ids.iter().enumerate() {
        let receipts = crate::agent::child_reviewer::poll_completed_delivery_receipts(
            &mock.db,
            "panel-run",
            Some(&[id.clone()]),
        )
        .unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(
            receipts[0].payload.as_ref().unwrap()["plan_review_can_finalize"],
            false
        );
        let effects = crate::agent::child_receipt::reduce_child_receipt(
            &env,
            &mut cp,
            &mut context,
            &receipts[0],
        )
        .await;
        assert!(!effects.model_text.contains("已在 1v1 中修订并定稿"));
        mock.db
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE agent_child_runs SET applied_at='now' WHERE id=?1",
                [id],
            )
            .unwrap();
        assert_eq!(cp.plan_reviewer_consumed, index == 2, "不得提前收齐");
    }
    assert_eq!(cp.last_review_blocking_count, 1);
    assert!(cp.plan_revisions_required(&mock.db));
    assert_eq!(
        cp.submitted_plan.as_ref().unwrap().plan_summary,
        submission.plan_summary
    );
    assert_eq!(cp.reviewer_dispatches.len(), 3);
    assert_eq!(mock.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn plan_team_discloses_one_member_and_complete_dispatch_failure_without_approval() {
    let mock = MockReviewer::new(vec![pass()]).await;
    let context = crate::agent::context::SandwichContext::new("sys".into(), "goal".into());
    for model in ["mock", "missing-model"] {
        let mut cp = AgentCheckpoint::new("partial-session".into(), &context);
        let old = cp.plan_phase;
        let submission =
            serde_json::from_value(json!({"plan_summary":"核查已有接口的只读方案"})).unwrap();
        let primary = PlanReviewerDispatchSnapshot::new(
            model,
            "flash",
            "auto",
            "",
            json!({
            "question":"核查方案","plan_review":true,"evidence_refs":[],
            "evidence_excerpt":"合成需求：只读核查已有接口，不修改代码。提交方案：核对接口声明与调用。"}),
        )
        .with_purpose("plan_review")
        .with_batch_id(format!("partial-{model}"));
        let (_tx, rx) = watch::channel(0);
        let result = dispatch_plan_team(
            None,
            &mock.db,
            &format!("partial-{model}"),
            "partial-session",
            &mock.root,
            &mut cp,
            &submission,
            1,
            1,
            false,
            true,
            vec![primary],
            rx,
        )
        .await;
        if model == "mock" {
            assert!(result.success);
            assert!(result.stdout.contains("实际仅启动 1 位"));
            let id = cp.reviewer_dispatches.keys().next().unwrap();
            mock.wait_terminal(id).await;
        } else {
            assert!(!result.success);
            assert_eq!(cp.plan_phase, old);
            assert!(cp.submitted_plan.is_none());
        }
    }
}

#[tokio::test]
async fn unit_plan_team_snapshots_share_batch_and_cannot_overwrite_each_others_plan() {
    let fixture = crate::agent::review_team::tests::TestDb::new();
    let cards = crate::agent::review_team::tests::seed_cards(&fixture.db, 3);
    let context = crate::agent::context::SandwichContext::new("sys".into(), "goal".into());
    let mut config = crate::agent::router::ModelRoutingConfig::default();
    config.stage_review_committee = true;
    config.role_models.insert("reviewer".into(), "mock".into());
    let routing =
        crate::agent::router::AgentRouter::route_for_role(&context, &config, 1, "reviewer");
    let primary = PlanReviewerDispatchSnapshot::new(
        "mock",
        "flash",
        "auto",
        "",
        json!({"question":"核查同一份方案","plan_review":true,"evidence_refs":[]}),
    )
    .with_purpose("plan_review")
    .with_batch_id("unit-panel");
    let snapshots = prepare_snapshots(
        &fixture.db,
        &fixture.root,
        &config,
        &routing,
        primary,
        &cards,
    )
    .await;
    assert_eq!(snapshots.len(), 3);
    for snapshot in &snapshots {
        assert_eq!(snapshot.batch_id.as_deref(), Some("unit-panel"));
        assert_eq!(snapshot.params["plan_review_can_finalize"], false);
        assert_eq!(snapshot.params["max_concurrency"], 3);
    }
    let submission: crate::agent::types::PlanSubmission =
        serde_json::from_value(json!({"plan_summary":"原始方案必须保持一致"})).unwrap();
    let mut cp = AgentCheckpoint::new("unit-session".into(), &context);
    cp.commit_plan_submission(submission.clone(), "unit-panel", &snapshots[0], 1);
    let delivery: crate::agent::child_reviewer::ReviewerDeliveryData =
        serde_json::from_value(json!({
        "findings":[],"confidence":"supported","decision":"approved",
        "root_cause_verdict":"explains","plan_review_can_finalize":false,
        "revised_plan_summary":"这份修订不能覆盖共同提交版本"}))
        .unwrap();
    cp.record_reviewer_delivery_for_run(&fixture.db, None, &delivery);
    assert_eq!(
        cp.submitted_plan.as_ref().unwrap().plan_summary,
        submission.plan_summary
    );
    assert!(cp.plan_finalized_by.is_none());

    // 成员要求升级仍是未批准；稍后到达的其他成员 approved 不能把它冲掉。
    let mut escalation = delivery.clone();
    escalation.extra.insert("decision".into(), json!("escalate"));
    cp.record_reviewer_delivery_for_run(&fixture.db, None, &escalation);
    cp.record_reviewer_delivery_for_run(&fixture.db, None, &delivery);
    assert!(cp.plan_revisions_required(&fixture.db));
}

#[tokio::test]
async fn unit_plan_team_missing_models_leave_no_running_or_approved_orphans() {
    let fixture = crate::agent::review_team::tests::TestDb::new();
    let context = crate::agent::context::SandwichContext::new("sys".into(), "goal".into());
    let mut cp = AgentCheckpoint::new("missing-session".into(), &context);
    let submission = serde_json::from_value(json!({"plan_summary":"尚未开始审查的方案"})).unwrap();
    let snapshots = vec![PlanReviewerDispatchSnapshot::new(
        "missing-model",
        "flash",
        "auto",
        "",
        json!({"question":"核对方案","plan_review":true}),
    )
    .with_batch_id("missing-batch")];
    let (_tx, rx) = watch::channel(0);
    let result = dispatch_plan_team(
        None,
        &fixture.db,
        "missing-run",
        "missing-session",
        &fixture.root,
        &mut cp,
        &submission,
        1,
        1,
        false,
        true,
        snapshots,
        rx,
    )
    .await;
    assert!(!result.success);
    assert!(cp.submitted_plan.is_none());
    assert!(!cp.plan_phase.is_approved());
    let running: i64 = fixture
        .db
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM agent_child_runs WHERE status='running'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(running, 0);
}

#[tokio::test]
async fn human_review_all_dispatches_fail_can_be_disclosed_and_arbitrated() {
    use crate::agent::checkpoint::{load_checkpoint, save_checkpoint, PlanExecutionMode};
    let fixture = crate::agent::review_team::tests::TestDb::new();
    let mut context = crate::agent::context::SandwichContext::new("sys".into(), "goal".into());
    let mut cp = AgentCheckpoint::new("human-failed-dispatch".into(), &context);
    cp.set_execution_mode(PlanExecutionMode::PlanApproval);
    cp.stages.plan_review = Some(true);
    cp.stages.plan = Some(true);
    cp.stages.final_review = Some(false);
    let submission = serde_json::from_value(json!({"plan_summary":"未审方案"})).unwrap();
    let snapshots = vec![
        PlanReviewerDispatchSnapshot::new("missing-model", "flash", "auto", "", json!({"role_label":"专家甲"})).with_batch_id("failed-batch"),
        PlanReviewerDispatchSnapshot::new("other", "flash", "auto", "", json!({"role_label":"专家乙", "host_input_error":"缺少所需输入"})).with_batch_id("failed-batch"),
    ];
    let (_tx, rx) = watch::channel(0);
    let result = dispatch_plan_team(None, &fixture.db, "failed-run", "human-failed-dispatch", &fixture.root,
        &mut cp, &submission, 3, 1, false, true, snapshots, rx).await;
    assert!(!result.success);
    assert_eq!(result.route, "submit_plan:dispatch_failed");
    assert!(result.stderr.contains("方案已保存但未经审查"));
    assert_eq!(cp.submitted_plan.as_ref().unwrap().plan_summary, "未审方案");
    assert!(cp.plan_review_batch_id.is_none());
    assert_eq!(cp.plan_reviewer_retry_count, 0);
    assert!(!cp.plan_phase.is_approved());
    assert!(crate::agent::plan_gate::check_pre_write_gatekeeper(&mut cp, &context, &fixture.db, "failed-run", 4, false).is_some());
    save_checkpoint(&fixture.db, &mut cp).unwrap();
    let mut cp = load_checkpoint(&fixture.db, "human-failed-dispatch").unwrap().unwrap();
    let material = crate::agent::plan_gate::human_plan_review_material(&cp, &fixture.db).unwrap().unwrap();
    for expected in ["未审方案", "专家甲未启动", "专家乙未启动", "缺少所需输入", "没有 Agent 审查结论"] {
        assert!(material.contains(expected), "missing {expected}: {material}");
    }
    assert!(crate::agent::plan_gate::submit_plan_rejection(&cp, &submission).is_none(), "允许重试");
    cp.apply_explicit_plan_decision(&fixture.db, &mut context, false, &material, "打回");
    assert!(!cp.plan_phase.is_approved());
    cp.apply_explicit_plan_decision(&fixture.db, &mut context, true, &material, "接受未经审查，批准");
    assert!(cp.plan_phase.is_approved());
    assert!(cp.plan_unreviewed_release);
    assert!(crate::agent::plan_gate::check_pre_write_gatekeeper(&mut cp, &context, &fixture.db, "failed-run", 5, false).is_none());
    let app = tauri::test::mock_app();
    let (_interrupt_tx, mut interrupt_rx) = watch::channel(0);
    let finish = serde_json::from_value(json!({"tool":"finish", "params":{"summary":"完成"}})).unwrap();
    let evaluated = crate::agent::finalization::evaluate_completion(
        app.handle(), &fixture.db, &mut context, &mut cp, "failed-run", "human-failed-dispatch",
        5, &fixture.root, &crate::agent::reviewer_gatekeeper::GitBaseline::non_git(),
        chrono::Utc::now(), &Default::default(), &mut interrupt_rx, &finish,
        crate::agent::finalization::CompletionMode::Finish, false, false,
    ).await;
    assert!(evaluated.receipt.is_ready(), "{:?}", evaluated.receipt.blockers);
    let mut revision = submission.clone();
    revision.plan_summary = "新版本".into();
    revision.revision_reason = "改变方案".into();
    assert!(record_human_review_dispatch_failure(&mut cp, &revision, 6, vec!["专家仍未启动".into()]));
    assert!(!cp.plan_phase.is_approved());
    assert!(cp.approved_plan_summary.is_none());
    let material = crate::agent::plan_gate::human_plan_review_material(&cp, &fixture.db).unwrap().unwrap();
    assert!(material.contains("新版本") && !material.contains("专家甲未启动"));
    let snapshot = PlanReviewerDispatchSnapshot::new("mock", "flash", "auto", "", json!({})).with_batch_id("retry");
    cp.commit_plan_submission(revision, "retry", &snapshot, 7);
    assert!(cp.plan_review_dispatch_failures.is_empty());
    assert!(crate::agent::plan_gate::human_plan_review_material(&cp, &fixture.db).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn human_review_partial_dispatch_discloses_failure_after_waiting_for_started_member() {
    use crate::agent::checkpoint::{load_checkpoint, save_checkpoint, PlanExecutionMode};
    let fixture = crate::agent::review_team::tests::TestDb::new();
    {
        let conn = fixture.db.0.lock().unwrap();
        conn.execute("INSERT INTO platforms (id,name,base_url,api_key,created_at) VALUES ('local-test','local-test','http://127.0.0.1:9/v1','test-only','now')", []).unwrap();
        conn.execute("INSERT INTO models (id,platform_id,name,display_name,created_at) VALUES ('local-test','local-test','test','test','now')", []).unwrap();
    }
    let context = crate::agent::context::SandwichContext::new("sys".into(), "goal".into());
    let mut cp = AgentCheckpoint::new("partial-human".into(), &context);
    cp.set_execution_mode(PlanExecutionMode::PlanApproval);
    cp.stages.plan_review = Some(true);
    let plan = serde_json::from_value(json!({"plan_summary":"部分启动方案"})).unwrap();
    let good = PlanReviewerDispatchSnapshot::new("local-test", "flash", "auto", "", json!({
        "role_label":"可启动专家", "question":"检查方案", "plan_review":true,
        "evidence_excerpt":"已读取的方案材料"})).with_purpose("plan_review").with_batch_id("partial-human-batch");
    let bad = PlanReviewerDispatchSnapshot::new("missing", "flash", "auto", "", json!({
        "role_label":"缺失专家", "question":"检查方案", "plan_review":true,
        "evidence_excerpt":"已读取的方案材料"})).with_purpose("plan_review").with_batch_id("partial-human-batch");
    let (tx, rx) = watch::channel(0);
    let result = dispatch_plan_team(None, &fixture.db, "partial-human-run", "partial-human", &fixture.root,
        &mut cp, &plan, 1, 1, false, true, vec![bad, good], rx).await;
    assert!(result.success, "{}", result.stderr);
    assert!(result.stdout.contains("缺失专家未启动"));
    assert_eq!(cp.plan_review_expected_count, 1);
    assert_eq!(crate::agent::plan_gate::human_plan_review_material(&cp, &fixture.db).unwrap_err().route,
        "plan_gatekeeper:need_wait_reviewer_jobs");
    let id = cp.reviewer_dispatches.keys().next().unwrap().clone();
    // 停止后台运行；构造已消费的技术失败，验证审批投影，不依赖本机监听或真实模型。
    tx.send(1).unwrap();
    fixture.db.0.lock().unwrap().execute("UPDATE agent_child_runs SET status='failed', error_kind='request_or_stream', consumed_at='now', applied_at='now' WHERE id=?1", [&id]).unwrap();
    cp.record_reviewer_failure_for_run(&fixture.db, &id);
    save_checkpoint(&fixture.db, &mut cp).unwrap();
    let cp = load_checkpoint(&fixture.db, "partial-human").unwrap().unwrap();
    let material = crate::agent::plan_gate::human_plan_review_material(&cp, &fixture.db).unwrap().unwrap();
    assert!(material.contains("缺失专家未启动"), "{material}");
    assert!(material.contains("成功 0 条，失败 1 条"), "{material}");
    assert!(material.contains("request_or_stream"), "{material}");
    assert!(!cp.plan_phase.is_approved());
}

#[tokio::test]
async fn human_review_role_unavailable_preserves_failure_for_user() {
    use crate::agent::checkpoint::PlanExecutionMode;
    let fixture = crate::agent::review_team::tests::TestDb::new();
    let mut context = crate::agent::context::SandwichContext::new("sys".into(), "goal".into());
    let mut cp = AgentCheckpoint::new("disabled-reviewer-session".into(), &context);
    cp.set_execution_mode(PlanExecutionMode::PlanApproval);
    cp.stages.plan_review = Some(true);
    let plan = serde_json::from_value(json!({"plan_summary":"核对接口", "evidence_refs":["src/a.rs"]})).unwrap();
    let mut routing = crate::agent::router::ModelRoutingConfig::default();
    routing.disabled_role_cards = vec!["tech_lead".into()];
    let error = crate::agent::tools::role_manager::resolve_plan_reviewer_card(&fixture.db, &routing.disabled_role_cards).unwrap_err();
    assert!(record_human_review_dispatch_failure(&mut cp, &plan, 1, vec![format!("方案审查角色未启动：{error}")]));
    let material = crate::agent::plan_gate::human_plan_review_material(&cp, &fixture.db).unwrap().unwrap();
    assert!(material.contains("tech_lead") && material.contains("未启动") && material.contains("核对接口"), "{material}");
    assert!(!cp.plan_phase.is_approved());
    cp.resume_failed_task(&mut context, "继续");
    let resumed = crate::agent::plan_gate::human_plan_review_material(&cp, &fixture.db).unwrap().unwrap();
    assert!(resumed.contains("tech_lead"));
    assert!(!cp.plan_phase.is_approved());
}

#[tokio::test]
async fn plan_reviewer_receives_requested_effective_modes_and_explicit_stage_settings() {
    let mock = MockReviewer::new(vec![pass()]).await;
    let c = crate::agent::context::SandwichContext::new("sys".into(), "跨模块测试报告".into());
    let mut cp = AgentCheckpoint::new("owner".into(), &c);
    let mut v = crate::agent::triage::TriageVerdict::fallback("fixture");
    v.fallback = None;
    cp.triage_verdict = Some(v);
    let sub = crate::agent::types::PlanSubmission {
        plan_summary: "验证测试范围".into(),
        final_review_mode: "mechanical_only".into(),
        stage_review_mode: "none".into(),
        ..Default::default()
    };
    let policy = review_policy_inputs(&cp, &sub);
    assert_eq!(policy["requested"]["final_review_mode"], "mechanical_only");
    assert_eq!(policy["effective"]["final_review_mode"], "code_review");
    assert_eq!(policy["effective"]["stage_review_mode"], "milestone");
    let snapshot = PlanReviewerDispatchSnapshot::new(
        "mock",
        "pro",
        "",
        "",
        json!({"question":"审查测试方案与审查配置","parent_task_goal":"完整目标标记".repeat(1300),"parent_workflow_context":crate::agent::review_team::parent_workflow_context(&cp),"evidence_excerpt":format!("宿主配置：{policy}"),"custom_inputs":{"final_review_mode":sub.final_review_mode,"stage_review_mode":sub.stage_review_mode,"review_policy":policy},"plan_review":true}),
    );
    let (_tx, rx) = watch::channel(0);
    let id = snapshot
        .spawn(None, &mock.db, "parent", "owner", &mock.root, rx, None)
        .await
        .unwrap();
    assert_eq!(mock.wait_terminal(&id).await.0, "succeeded");
    let requests = mock.requests.lock().unwrap();
    let messages = requests[0]["messages"].to_string();
    assert!(requests[0]["messages"].as_array().unwrap().iter().any(|m|m["content"].as_str().is_some_and(|s| s.starts_with("【完整父任务目标】") && s.contains(&"完整目标标记".repeat(1300)))));
    assert!(requests[0]["messages"].as_array().unwrap().iter().any(|m|
        m["content"].as_str().is_some_and(|s| s.starts_with("【被审查父任务的流程配置快照】"))));
    assert!(requests[0]["messages"].as_array().unwrap().iter().any(|m|
        m["content"].as_str().is_some_and(|s| s.contains(crate::agent::review_tools::TEST_EVIDENCE_GUIDANCE.trim()))));
    for token in [
        "mechanical_only",
        "code_review",
        "milestone",
        "review_policy",
        "effective",
    ] {
        assert!(
            messages.contains(token),
            "{token} missing from actual reviewer request"
        );
    }
    drop(requests);
    cp.stages.final_review = Some(false);
    cp.stages.stage_review = Some(false);
    let explicit = review_policy_inputs(&cp, &sub);
    assert_eq!(explicit["stages"]["final_review"], false);
    assert_eq!(explicit["stages"]["stage_review"], false);
}
