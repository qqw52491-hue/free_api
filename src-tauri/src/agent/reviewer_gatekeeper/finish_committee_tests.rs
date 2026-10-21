use super::*;
use crate::agent::child_reviewer::retirement_tests::MockReviewer;

fn passed() -> serde_json::Value {
    json!({"verdict":"passed","score":85,"summary":"独立核对完成","blocking_issues":[],"nit_issues":[]})
}

async fn review(
    mock: &MockReviewer,
    committee: bool,
    rx: &mut watch::Receiver<u64>,
) -> ReviewResult {
    let context = SandwichContext::new("sys".into(), "只读分析，不修改项目".into());
    let mut routing = ModelRoutingConfig::default();
    routing.stage_review_committee = committee;
    routing.role_models.insert("reviewer".into(), "mock".into());
    let mut triage = crate::agent::triage::TriageVerdict::fallback("fixture");
    triage.final_review_mode = "mechanical_only".into();
    triage.final_review_roles = vec!["review_0".into()];
    let app = tauri::test::mock_app();
    super::super::evaluate_finish_diff(
        app.handle(),
        &mock.db,
        &context,
        &routing,
        1,
        &mock.root,
        &GitBaseline::capture(&mock.root),
        chrono::Utc::now(),
        "committee-session",
        rx,
        None,
        None,
        &[],
        None,
        None,
        &[],
        None,
        Some(&triage),
        None,
        Some("这是需要核对的完整分析报告"),
    )
    .await
}

#[tokio::test]
async fn explicit_committee_reviews_read_only_mechanical_task_with_three_independent_contexts() {
    let mock = MockReviewer::concurrent(vec![passed()], 3).await;
    crate::agent::review_team::tests::seed_cards(&mock.db, 3);
    let (_tx, mut rx) = watch::channel(0);
    let result = review(&mock, true, &mut rx).await;
    assert!(result.is_passed(), "{result:?}");
    assert!(result.summary.contains("有效结论 3 位"));
    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    for i in 0..3 {
        assert!(requests.iter().any(|r| r["messages"]
            .to_string()
            .contains(&format!("REVIEW_MEMBER_{i}"))));
    }
    drop(requests);
    // 默认模式仍尊重原 mechanical_only，不因候选角色多就增加调用。
    let result = review(&mock, false, &mut rx).await;
    assert!(result.is_passed());
    assert_eq!(mock.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn committee_preserves_one_members_real_blocker_without_score_voting() {
    let mut blocked = passed();
    blocked["score"] = json!(100);
    blocked["verdict"] = json!("rejected");
    blocked["blocking_issues"] =
        json!([{"file":"report.md","issue":"报告断言与实际输出矛盾","requirement":"纠正结论"}]);
    let mock = MockReviewer::new(vec![passed(), blocked, passed()]).await;
    crate::agent::review_team::tests::seed_cards(&mock.db, 3);
    let (_tx, mut rx) = watch::channel(0);
    let result = review(&mock, true, &mut rx).await;
    assert!(!result.is_passed());
    assert!(!result.is_service_failure());
    assert_eq!(result.blocking_issues.len(), 1);
    assert!(result.blocking_issues[0]
        .issue
        .contains("报告断言与实际输出矛盾"));
    assert_eq!(mock.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn committee_excludes_unavailable_members_without_treating_them_as_approvals() {
    let mock = MockReviewer::new(vec![passed()]).await;
    crate::agent::review_team::tests::seed_cards(&mock.db, 1);
    let (_tx, mut rx) = watch::channel(0);
    let single = review(&mock, true, &mut rx).await;
    assert!(single.is_passed());
    assert!(single.summary.contains("实际为单人"));
    crate::agent::review_team::tests::seed_cards(&mock.db, 3);
    crate::agent::tools::role_manager::upsert_card(
        &mock.db,
        &json!({"name":"review_1","model_id":"missing-model"}),
    )
    .unwrap();
    let partial = review(&mock, true, &mut rx).await;
    assert!(partial.is_passed(), "{partial:?}");
    assert!(!partial.is_service_failure());
    assert!(partial.summary.contains("通道失败 1 位"));
    assert!(partial.summary.contains("有效结论 2 位"));
    assert_eq!(
        mock.requests.lock().unwrap().len(),
        3,
        "一位单审加两位有效模型，无第三方自动换模"
    );
}

#[tokio::test]
async fn committee_one_valid_member_can_pass_but_all_unavailable_cannot() {
    let mock = MockReviewer::new(vec![passed()]).await;
    crate::agent::review_team::tests::seed_cards(&mock.db, 3);
    for i in [1, 2] {
        crate::agent::tools::role_manager::upsert_card(&mock.db,
            &json!({"name":format!("review_{i}"),"model_id":"missing-model"})).unwrap();
    }
    let (_tx, mut rx) = watch::channel(0);
    let out = review(&mock, true, &mut rx).await;
    assert!(out.is_passed(), "{out:?}");
    assert_eq!(out.score, 85);
    assert!(out.summary.contains("有效结论 1 位"));
    assert!(out.summary.contains("实际为单人"));
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
    crate::agent::tools::role_manager::upsert_card(&mock.db,
        &json!({"name":"review_0","model_id":"missing-model"})).unwrap();
    let out = review(&mock, true, &mut rx).await;
    assert!(!out.is_passed());
    assert!(out.is_service_failure());
    assert!(out.summary.contains("有效结论 0 位"));
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
}

#[test]
fn committee_skipped_and_unavailable_are_not_effective_reviews() {
    // skipped_empty 是既有“无修改直通批准”，不是 skipped 枚举。
    let mut skipped = ReviewResult::skipped_empty();
    skipped.verdict = "skipped".into();
    let out = aggregate_committee_results(vec![
        ("A".into(), skipped),
        ("B".into(), ReviewResult::failed_open("timeout", "m", "hash")),
    ]);
    assert!(!out.is_passed());
    assert!(out.is_service_failure());
    assert_eq!(out.score, 0);
}

#[test]
fn committee_partial_pass_is_overridden_by_explicit_stop() {
    let pass = super::super::parse_reviewer_json(&passed().to_string(), "m", "hash", 1).unwrap();
    let out = settle_committee_results(vec![
        ("A".into(), pass),
        ("B".into(), ReviewResult::failed_open("cancelled", "m", "hash")),
    ], true);
    assert!(!out.is_passed());
    assert!(out.is_service_failure());
    assert!(out.summary.contains("已停止"));
}

#[tokio::test]
async fn committee_stop_cannot_turn_into_a_pass() {
    let (tx, mut rx) = watch::channel(0);
    let mock = MockReviewer::with_request_hook(vec![passed()], move || {
        tx.send_replace(1);
    })
    .await;
    crate::agent::review_team::tests::seed_cards(&mock.db, 3);
    let result = review(&mock, true, &mut rx).await;
    assert!(!result.is_passed());
}

#[test]
fn unit_committee_aggregation_preserves_blocker_and_service_failure() {
    let pass = super::super::parse_reviewer_json(&passed().to_string(), "m", "hash", 1).unwrap();
    let mut block = pass.clone();
    block.verdict = "rejected".into();
    block.blocking_issues.push(BlockingIssue {
        file: "a.rs".into(),
        issue: "真实阻塞".into(),
        requirement: "修复".into(),
    });
    let mut out = aggregate_committee_results(vec![
        ("A".into(), pass.clone()),
        ("B".into(), block.clone()),
    ]);
    assert!(!out.is_passed());
    assert!(!out.is_service_failure());
    assert_eq!(out.blocking_issues.len(), 1);
    assert!(out.blocking_issues[0].issue.contains("B"));
    out = aggregate_committee_results(vec![
        ("B".into(), block),
        (
            "C".into(),
            ReviewResult::failed_open("timeout", "m", "hash"),
        ),
    ]);
    assert!(!out.is_service_failure());
    assert_eq!(out.verdict, "rejected");
    assert_eq!(out.blocking_issues.len(), 1);
    assert!(out.summary.contains("通道失败 1 位"));
    out = aggregate_committee_results(vec![("A".into(), pass)]);
    assert!(out.is_passed());
    assert!(out.summary.contains("实际为单人"));
}
