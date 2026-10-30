use super::*;
use crate::agent::child_reviewer::retirement_tests::MockReviewer;

fn passed() -> serde_json::Value {
    json!({"verdict":"passed","score":85,"summary":"独立核对完成","blocking_issues":[],"nit_issues":[]})
}

#[test]
fn individually_cancelled_member_neither_approves_nor_erases_other_members_blockers() {
    let cancelled = ReviewResult::cancelled("人工停止，本任务排除", "slow", "hash");
    let mut approved = ReviewResult::skipped_empty();
    approved.model_id = "fast".into();
    let merged = aggregate_committee_results(vec![("slow".into(), cancelled.clone()), ("fast".into(), approved.clone())]);
    assert!(merged.is_passed());
    assert!(merged.summary.contains("有效结论 1 位"));
    assert!(merged.summary.contains("通道失败 0 位"));
    assert!(merged.summary.contains("人工停止或本任务排除 1 位"));
    approved.verdict = "rejected".into();
    approved.blocking_issues.push(BlockingIssue { file:"src/a.rs".into(), issue:"真实权限缺口".into(), requirement:"修复".into() });
    let rejected = aggregate_committee_results(vec![("slow".into(),cancelled.clone()),("fast".into(),approved)]);
    assert!(!rejected.is_passed());
    assert_eq!(rejected.blocking_issues.len(), 1);
    assert!(!aggregate_committee_results(vec![("slow".into(),cancelled)]).is_passed(), "全取消不能当批准");
}

async fn review(
    mock: &MockReviewer,
    committee: bool,
    rx: &mut watch::Receiver<u64>,
) -> ReviewResult {
    review_task(mock, committee, rx, true, false).await.0
}

async fn review_task(
    mock: &MockReviewer,
    committee: bool,
    rx: &mut watch::Receiver<u64>,
    writes: bool,
    mechanical_only: bool,
) -> (ReviewResult, Vec<serde_json::Value>) {
    let mut context = SandwichContext::new("sys".into(), "完成当前任务".into());
    let baseline = GitBaseline::capture(&mock.root);
    if writes {
        std::fs::write(mock.root.join("report.md"), "本轮生成的待审查报告").unwrap();
        context.record_side_effect(1, "write_file".into(), "report.md".into());
    }
    let mut routing = ModelRoutingConfig::default();
    routing.stage_review_committee = committee;
    routing.role_models.insert("reviewer".into(), "mock".into());
    let mut triage = crate::agent::triage::TriageVerdict::fallback("fixture");
    triage.final_review_mode = if mechanical_only { "mechanical_only" } else { "artifact_quality" }.into();
    triage.final_review_roles = vec!["review_0".into()];
    let app = tauri::test::mock_app();
    let session_id = format!("committee-session-{}", uuid::Uuid::new_v4());
    use tauri::Listener;
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    for channel in [format!("agent-progress-{session_id}"), format!("agent-log-{session_id}")] {
        let out = events.clone();
        app.listen(channel, move |event| {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(event.payload()) {
                out.lock().unwrap().push(value);
            }
        });
    }
    let result = super::super::evaluate_finish_diff(
        app.handle(),
        &mock.db,
        &context,
        &routing,
        1,
        &mock.root,
        &baseline,
        chrono::Utc::now(),
        &session_id,
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
        &[],
    )
    .await;
    let recorded = events.lock().unwrap().clone();
    (result, recorded)
}

#[tokio::test]
async fn committee_choice_does_not_trigger_reviews_or_member_progress_for_exempt_tasks() {
    let mock = MockReviewer::new(vec![passed()]).await;
    crate::agent::review_team::tests::seed_cards(&mock.db, 3);
    let (_tx, mut rx) = watch::channel(0);
    for committee in [false, true] {
        // 包含已有工作区改动时的只读新问答；mechanical_only 则即使有本轮写入仍走既有机械核验。
        for (writes, mechanical) in [(false, false), (false, true), (true, true)] {
            let (result, events) = review_task(&mock, committee, &mut rx, writes, mechanical).await;
            assert!(result.is_passed(), "{result:?}");
            assert!(result.summary.contains("已跳过"));
            assert_eq!(mock.requests.lock().unwrap().len(), 0, "人数开关不能新增审查请求");
            assert!(!events.iter().any(|v| v["type"] == "finish_review_progress"), "免审不能生成成员面板");
            assert!(!events.iter().any(|v| v.as_str().is_some_and(|s| s.contains("多人终审本轮选择"))));
        }
    }
}

#[tokio::test]
async fn required_committee_reviews_have_three_independent_contexts() {
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
    // 单人夹具不复用三路齐套屏障；相同任务选择单人时只调用一位。
    let single_mock = MockReviewer::new(vec![passed()]).await;
    crate::agent::review_team::tests::seed_cards(&single_mock.db, 3);
    let result = review(&single_mock, false, &mut rx).await;
    assert!(result.is_passed());
    assert_eq!(single_mock.requests.lock().unwrap().len(), 1);
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
