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
