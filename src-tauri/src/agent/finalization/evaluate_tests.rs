use super::*;
use crate::agent::change_verification::ChangeVerification;
use crate::agent::reviewer_gatekeeper::BlockingIssue;

fn context() -> SandwichContext {
    SandwichContext::new("system".into(), "goal".into())
}
fn checkpoint(c: &SandwichContext) -> AgentCheckpoint {
    AgentCheckpoint::new("session".into(), c)
}
fn review(blocking: bool) -> ReviewResult {
    let mut r = ReviewResult::skipped_empty();
    r.model_id = "reviewer".into();
    r.diff_hash = "same-material".into();
    if blocking {
        r.blocking_issues.push(BlockingIssue {
            file: "src/a.rs".into(),
            issue: "真实缺陷".into(),
            requirement: "修复后重新验证".into(),
        });
    }
    r
}
#[test]
fn reviewed_flag_and_retry_count_never_erase_remaining_facts() {
    let mut c = context();
    for step in 1..=39 {
        c.record_side_effect(step, "local_shell".into(), "write or unknown shell".into());
    }
    let mut cp = checkpoint(&c);
    cp.finish_reviewed = true;
    cp.finish_challenge_count = 100;
    assert!(
        change_verification_blocker(c.side_effects.len(), c.change_verification.as_ref()).is_some()
    );
    assert_eq!(
        verification_receipt(true, c.change_verification.as_ref(), &[]),
        ReceiptVerification::Pending
    );
    for retries in [0, 1, 2, 3, 100] {
        assert!(!review_acceptance(&review(true), retries, false).is_accepted());
    }
}
#[test]
fn human_arbitration_is_explicit_and_never_converts_service_failure_to_pass() {
    assert!(!review_acceptance(&review(true), 2, true).is_accepted());
    assert!(matches!(
        review_acceptance(&review(true), 3, true),
        ReceiptReview::DeliveredWithWarnings { .. }
    ));
    assert!(review_acceptance(&review(false), 0, false).is_accepted());
    let failed = ReviewResult::failed_open("401 / timeout", "model", "hash");
    assert!(!review_acceptance(&failed, 100, true).is_accepted());
}
#[test]
fn verifier_failure_is_not_overridden_by_an_older_verified_claim() {
    let verified = ChangeVerification {
        status: "verified".into(),
        evidence_refs: vec!["step_40".into()],
        waived_reason: None,
    };
    let failures =
        vec!["宿主命令 mvn test 在 /tmp/ws 退出 1：[ERROR] No tests matching pattern".into()];
    assert!(matches!(
        verification_receipt(true, Some(&verified), &failures),
        ReceiptVerification::Failed { .. }
    ));
    assert!(matches!(
        verification_receipt(true, Some(&verified), &[]),
        ReceiptVerification::Verified { .. }
    ));
    assert_eq!(
        verification_receipt(false, None, &[]),
        ReceiptVerification::NotRequired
    );
}
#[test]
fn acceptance_version_ignores_display_bookkeeping_but_tracks_new_writes_and_claim_corrections() {
    use crate::agent::target_block::TargetBlock;
    let mut c = context();
    c.block_manager
        .set_blocks(vec![TargetBlock::new("b1", "repair")]);
    let cp = checkpoint(&c);
    let fp = acceptance_fingerprint(&c, &cp, Some("交付"));
    c.current_observation = "new output / artifact registration".into();
    c.add_host_feedback("display-only");
    c.active_target_block_mut().unwrap().on_step_finish();
    assert_eq!(fp, acceptance_fingerprint(&c, &cp, Some("交付")));
    c.record_side_effect(40, "local_file".into(), "source changed".into());
    assert_ne!(fp, acceptance_fingerprint(&c, &cp, Some("交付")));
    let written = acceptance_fingerprint(&c, &cp, Some("交付"));
    c.change_verification = Some(ChangeVerification {
        status: "waived".into(),
        evidence_refs: vec![],
        waived_reason: Some("无测试工具，由终审核对".into()),
    });
    assert_ne!(written, acceptance_fingerprint(&c, &cp, Some("交付")));
}
#[test]
fn acceptance_version_tracks_stage_defects_scope_and_summary() {
    use crate::agent::target_block::TargetBlock;
    let mut c = context();
    c.block_manager
        .set_blocks(vec![TargetBlock::new("b1", "repair")]);
    let mut cp = checkpoint(&c);
    let fp = acceptance_fingerprint(&c, &cp, Some("summary"));
    assert_ne!(
        fp,
        acceptance_fingerprint(&c, &cp, Some("different result"))
    );
    cp.stage_review_blockers_open = 1;
    assert_ne!(fp, acceptance_fingerprint(&c, &cp, Some("summary")));
    cp.stage_review_blockers_open = 0;
    c.active_target_block_mut()
        .unwrap()
        .scope
        .push("another.rs".into());
    assert_ne!(fp, acceptance_fingerprint(&c, &cp, Some("summary")));
}
#[test]
fn successful_review_does_not_consume_a_stop_signal_as_completion() {
    let (tx, rx) = watch::channel(0);
    let generation = *rx.borrow();
    assert!(!interrupted_since(&rx, generation));
    let accepted = review_acceptance(&review(false), 0, false);
    assert!(accepted.is_accepted());
    tx.send(1).unwrap();
    assert!(interrupted_since(&rx, generation));
    tx.send(2).unwrap();
    assert!(interrupted_since(&rx, generation));
}
#[test]
fn pending_or_failed_verification_never_produces_a_ready_receipt() {
    let mut receipt:FinalizationReceipt=serde_json::from_value(json!({
        "receipt_id":"test","diff_hash":"h","scope_revision":0,"contract_fingerprint":"",
        "step_id":42,"verification":{"status":"pending"},"review":{"status":"passed","score":84,"model_id":"m","summary":"ok"},
        "blockers":[],"risk_flags":[],"created_at":"now"
    })).unwrap();
    assert!(!receipt.is_ready());
    receipt.verification = ReceiptVerification::Failed {
        details: "exit 1".into(),
    };
    assert!(!receipt.is_ready());
    receipt.verification = ReceiptVerification::Verified {
        evidence: "host exit 0".into(),
    };
    assert!(receipt.is_ready());
    receipt.review = ReceiptReview::Pending;
    assert!(!receipt.is_ready());
}
#[test]
fn inferred_verification_is_scoped_to_the_task_repository_and_subproject() {
    use crate::agent::change_verification::infer_adaptive_verification_command_in;
    let dir = std::env::temp_dir().join(format!("finish-scope-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(dir.join("module/src")).unwrap();
    std::fs::write(
        dir.join("module/Cargo.toml"),
        "[package]\nname='module'\nversion='0.1.0'\n",
    )
    .unwrap();
    let cmd = infer_adaptive_verification_command_in(&dir, &["module/src/main.rs".into()]).unwrap();
    assert!(cmd.contains("/module' && cargo check"), "{cmd}");
    assert!(infer_adaptive_verification_command_in(&dir, &["../outside/a.rs".into()]).is_none());
    assert!(infer_adaptive_verification_command_in(&dir, &["/outside/a.rs".into()]).is_none());
    assert!(infer_adaptive_verification_command_in(&dir, &["README.md".into()]).is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn frozen_suite_success_saves_real_receipt_without_model_recopy_and_failure_does_not() {
    let mut c = context();
    c.record_side_effect(39, "local_file".into(), "write".into());
    let mut result = crate::agent::host_verify::ObligationVerifyResult {
        obligation_id: "package".into(),
        command: "unzip -l package.zip | grep lic".into(),
        working_dir: "/tmp/ws".into(),
        exit_code: "1".into(),
        output_tail: "not found".into(),
        passed: false,
    };
    assert!(!save_successful_obligation_receipt(&mut c, 42, &[]));
    assert!(c.change_verification.is_none());
    assert!(!save_successful_obligation_receipt(
        &mut c,
        42,
        std::slice::from_ref(&result)
    ));
    assert!(c.change_verification.is_none());
    result.passed = true;
    result.exit_code = "0".into();
    assert!(save_successful_obligation_receipt(&mut c, 43, &[result]));
    assert!(c.change_verification.as_ref().unwrap().is_resolved());
    c.record_side_effect(44, "local_file".into(), "new write".into());
    assert!(c.change_verification.is_none());
}

#[tokio::test]
async fn completed_report_todos_do_not_infer_a_requirement_to_write_code() {
    use crate::agent::child_reviewer::{retirement_tests::MockReviewer, ReviewScorecard};
    use crate::agent::types::TodoItem;
    let mock = MockReviewer::new(vec![json!({})]).await;
    for description in [
        "检查修复方案，不修改项目",
        "解释实现原理",
        "review fix proposal",
        "compare refactor options",
    ] {
        let mut c = context();
        c.todo_list.push(TodoItem {
            id: 1,
            status: "done".into(),
            description: description.into(),
            evidence_output_id: None,
        });
        let mut cp = checkpoint(&c);
        cp.plan_phase = crate::agent::checkpoint::PlanPhase::Reviewing;
        cp.last_review_scorecard = Some(ReviewScorecard::new(80, 48, 85, Some(75)));
        let baseline = GitBaseline::capture(&mock.root);
        let (blockers, _, _) = collect_checks(
            &mock.db,
            &c,
            &cp,
            "report-run",
            "report-session",
            1,
            &mock.root,
            &baseline,
            Utc::now(),
            false,
            false,
        )
        .await;
        assert!(blockers.is_empty(), "{description}: {blockers:?}");
        cp.stage_review_blockers_open = 1;
        let (blockers, _, _) = collect_checks(
            &mock.db,
            &c,
            &cp,
            "report-run",
            "report-session",
            1,
            &mock.root,
            &baseline,
            Utc::now(),
            false,
            false,
        )
        .await;
        assert!(
            blockers.iter().any(|b| b.contains("阶段审查仍有 1 条")),
            "真实阶段缺陷仍应阻塞: {blockers:?}"
        );
    }
}
