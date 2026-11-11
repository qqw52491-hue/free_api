use super::*;
use crate::agent::{
    checkpoint::{load_checkpoint, save_checkpoint, PlanPhase},
    context::SandwichContext,
};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[test]
fn human_review_combination_is_opt_in_and_reuses_submission_channel() {
    let c = SandwichContext::new("system".into(), "goal".into());
    let mut cp = AgentCheckpoint::new("session".into(), &c);
    cp.set_execution_mode(PlanExecutionMode::PlanApproval);
    let plan = serde_json::from_value(json!({"plan_summary":"inspect evidence"})).unwrap();
    let db = DbState(Arc::new(Mutex::new(
        crate::db::init_db(&":memory:".into()).unwrap(),
    )));
    for setting in [None, Some(false), Some(true)] {
        cp.stages.plan_review = setting;
        assert!(submit_plan_rejection(&cp, &plan).is_none());
        if setting == Some(true) {
            assert_eq!(
                human_plan_review_material(&cp, &db).unwrap_err().route,
                "plan_gatekeeper:need_reviewer_dispatch"
            );
        } else {
            assert!(human_plan_review_material(&cp, &db).unwrap().is_none());
        }
    }
}

#[tokio::test]
async fn reviewed_human_plan_waits_for_all_members_then_approves_and_reopens_after_revision() {
    use crate::agent::{
        child_reviewer::{
            begin_child_run_extended, ChildRunIdentity, PlanReviewerDispatchSnapshot,
        },
        finalization::*,
    };
    for has_rejection_and_failure in [false, true] {
        let db = DbState(Arc::new(Mutex::new(
            crate::db::init_db(&":memory:".into()).unwrap(),
        )));
        let root = std::env::temp_dir().join(format!("review-human-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut c = SandwichContext::new("system".into(), "inspect evidence".into());
        let mut cp = AgentCheckpoint::new("human-review-session".into(), &c);
        cp.set_execution_mode(PlanExecutionMode::PlanApproval);
        cp.stages.plan = Some(true);
        cp.stages.plan_review = Some(true);
        cp.stages.final_review = Some(false);
        let plan=serde_json::from_value(json!({"plan_summary":"inspect evidence without changing files","target_files":[],"evidence_refs":[]})).unwrap();
        let batch = "human-batch";
        let mut ids = Vec::new();
        for _ in 0..2 {
            let mut identity = ChildRunIdentity::fresh("plan_review");
            identity.batch_id = Some(batch.into());
            ids.push(
                begin_child_run_extended(
                    &db,
                    "run",
                    &cp.run_id,
                    &root,
                    "mock",
                    0,
                    "review",
                    Some(3),
                    None,
                    None,
                    &identity,
                )
                .unwrap(),
            );
        }
        let snapshot =
            PlanReviewerDispatchSnapshot::new("mock", "pro", "high", "persona", json!({}))
                .with_batch_id(batch);
        cp.commit_plan_submission(plan, batch, &snapshot, 1);
        cp.plan_review_expected_count = 2;
        if has_rejection_and_failure {
            cp.plan_review_dispatch_failures = vec!["第三位专家未启动：模型配置不可用".into()];
        }
        assert_eq!(cp.plan_mode, PlanExecutionMode::PlanApproval);
        assert_eq!(
            human_plan_review_material(&cp, &db).unwrap_err().route,
            "plan_gatekeeper:need_wait_reviewer_jobs"
        );
        // Exercise both unanimous approval and rejection plus technical failure.
        // Neither case substitutes for the human's decision.
        let delivery=serde_json::from_value(json!({"findings":if has_rejection_and_failure {vec![json!({"id":"F1","severity":"blocking","why":"need a decision","location":"evidence.txt:1"})]} else {vec![]},"confidence":"supported","evidence_refs":[],"missing_evidence":[],"decision":if has_rejection_and_failure {"rebuttal"} else {"approved"}})).unwrap();
        db.0.lock().unwrap().execute("UPDATE agent_child_runs SET status='succeeded',consumed_at='now',applied_at='now' WHERE id=?1",[&ids[0]]).unwrap();
        cp.record_reviewer_delivery_for_run(&db, Some(&ids[0]), &delivery);
        assert!(human_plan_review_material(&cp, &db).is_err());
        if has_rejection_and_failure {
            db.0.lock().unwrap().execute("UPDATE agent_child_runs SET status='failed',error_kind='request_or_stream',consumed_at='now',applied_at='now' WHERE id=?1",[&ids[1]]).unwrap();
            cp.record_reviewer_failure_for_run(&db, &ids[1]);
        } else {
            db.0.lock().unwrap().execute("UPDATE agent_child_runs SET status='succeeded',consumed_at='now',applied_at='now' WHERE id=?1",[&ids[1]]).unwrap();
            cp.record_reviewer_delivery_for_run(&db, Some(&ids[1]), &delivery);
        }
        save_checkpoint(&db, &mut cp).unwrap();
        let mut cp = load_checkpoint(&db, "human-review-session")
            .unwrap()
            .unwrap();
        let material = human_plan_review_material(&cp, &db).unwrap().unwrap();
        assert!(material.contains("inspect evidence without changing files"));
        if has_rejection_and_failure {
            assert!(material.contains("need a decision"));
            assert!(material.contains("成功 1 条，失败 1 条"));
            assert!(material.contains("第三位专家未启动：模型配置不可用"));
            assert!(material.contains("request_or_stream"));
        } else {
            assert!(material.contains("成功 2 条，失败 0 条"));
        }
        assert!(!cp.plan_phase.is_approved());
        assert!(check_pre_write_gatekeeper(&mut cp, &c, &db, "run", 2, false).is_some());
        let app = tauri::test::mock_app();
        let (_tx, mut rx) = tokio::sync::watch::channel(0);
        let inst = serde_json::from_value(
            json!({"tool":"finish","params":{"summary":"inspection complete"}}),
        )
        .unwrap();
        let result = evaluate_completion(
            app.handle(),
            &db,
            &mut c,
            &mut cp,
            "run",
            "human-review-session",
            2,
            &root,
            &crate::agent::reviewer_gatekeeper::GitBaseline::non_git(),
            chrono::Utc::now(),
            &Default::default(),
            &mut rx,
            &inst,
            CompletionMode::Finish,
            false,
            false,
        )
        .await;
        assert!(!result.receipt.is_ready());
        // Human rejection keeps the plan unapproved; explicit approval may accept
        // the disclosed findings/failure under the existing human waiver protocol.
        cp.apply_explicit_plan_decision(&db, &mut c, false, &material, "请修改");
        assert_eq!(cp.plan_phase, PlanPhase::Reviewing);
        cp.apply_explicit_plan_decision(&db, &mut c, true, &material, "接受上述风险，批准");
        save_checkpoint(&db, &mut cp).unwrap();
        let mut cp = load_checkpoint(&db, "human-review-session")
            .unwrap()
            .unwrap();
        assert!(check_pre_write_gatekeeper(&mut cp, &c, &db, "run", 3, false).is_none());
        let result = evaluate_completion(
            app.handle(),
            &db,
            &mut c,
            &mut cp,
            "run",
            "human-review-session",
            3,
            &root,
            &crate::agent::reviewer_gatekeeper::GitBaseline::non_git(),
            chrono::Utc::now(),
            &Default::default(),
            &mut rx,
            &inst,
            CompletionMode::Finish,
            false,
            false,
        )
        .await;
        assert!(result.receipt.is_ready(), "{:?}", result.receipt.blockers);
        // A new saved revision cannot reuse the old human approval.
        let mut revision = cp.submitted_plan.clone().unwrap();
        revision.revision_reason = "new evidence".into();
        cp.commit_plan_submission(revision, "next-batch", &snapshot, 4);
        assert!(cp.approved_plan_summary.is_none());
        assert!(!cp.plan_phase.is_approved());
        assert!(human_plan_review_material(&cp, &db).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn automatic_approval_reuses_saved_plan_and_waits_for_the_review_batch() {
    use crate::agent::child_reviewer::{begin_child_run_extended, ChildRunIdentity, PlanReviewerDispatchSnapshot};
    let db = DbState(Arc::new(Mutex::new(crate::db::init_db(&":memory:".into()).unwrap())));
    let mut c = SandwichContext::new("system".into(), "goal".into());
    let mut cp = AgentCheckpoint::new("auto-approval".into(), &c);
    cp.set_execution_mode(PlanExecutionMode::PlanApproval);
    cp.stages.plan_review = Some(false);
    cp.triage_verdict = Some(serde_json::from_value(json!({
        "route": "technical_plan", "stage_review_mode": "none", "final_review_mode": "operational_verification",
    })).unwrap());
    let plan = serde_json::from_value(json!({
        "plan_summary": "inspect then change", "target_files": ["src/a.rs"],
        "stage_review_mode": "milestone", "final_review_mode": "mechanical_only",
    })).unwrap();
    assert!(crate::agent::plan_submit::record_plan_without_review(&mut cp, &plan, 1).unwrap().success);
    assert!(!cp.plan_phase.is_approved());
    assert!(cp.approved_plan_summary.is_none());
    assert!(automatic_plan_approval_instruction(&cp, &db).unwrap().is_none());
    cp.auto_plan_approval_pending = true;
    save_checkpoint(&db, &mut cp).unwrap();
    let mut cp = load_checkpoint(&db, "auto-approval").unwrap().unwrap();
    let verdict = cp.triage_verdict.as_ref().unwrap();
    assert_eq!(verdict.stage_review_mode, "milestone");
    assert_eq!(verdict.final_review_mode, "operational_verification");
    let ask = automatic_plan_approval_instruction(&cp, &db).unwrap().unwrap();
    assert_eq!(ask.get_tool(), "ask_human");
    assert!(ask.get_params()["form"]["description"].as_str().unwrap().contains("inspect then change"));
    cp.apply_explicit_plan_decision(&db, &mut c, true, "plan", "approved");
    assert!(cp.plan_phase.is_approved());
    assert!(check_pre_write_gatekeeper(&mut cp, &c, &db, "auto-approval", 2, false).is_none());

    let mut revision = plan.clone();
    revision.plan_summary = "new plan".into();
    revision.revision_reason = "new evidence".into();
    cp.stages.plan_review = Some(true);
    let batch = "auto-review-batch";
    let mut identity = ChildRunIdentity::fresh("plan_review");
    identity.batch_id = Some(batch.into());
    let id = begin_child_run_extended(&db, "auto-approval", "session", std::path::Path::new("/tmp"),
        "mock", 0, "review", Some(2), None, None, &identity).unwrap();
    let snapshot = PlanReviewerDispatchSnapshot::new("mock", "pro", "high", "", json!({})).with_batch_id(batch);
    cp.commit_plan_submission(revision, batch, &snapshot, 3);
    assert!(cp.approved_plan_summary.is_none());
    let wait = automatic_plan_approval_instruction(&cp, &db).unwrap().unwrap();
    assert_eq!(wait.get_tool(), "wait_jobs");
    assert_eq!(wait.get_params()["job_ids"][0], id);
    assert!(is_automatic_plan_wait(&cp, &db, &wait), "host wait may read this plan batch even with restricted tools");
    let other: crate::agent::types::AgentInstruction = serde_json::from_value(json!({"tool":"wait_jobs", "params":{"job_ids":["other-job"]}})).unwrap();
    assert!(!is_automatic_plan_wait(&cp, &db, &other), "other jobs cannot bypass tool permissions");
    let with_action: crate::agent::types::AgentInstruction = serde_json::from_value(json!({"tool":"wait_jobs", "action":"cancel", "params":{"job_ids":[id]}})).unwrap();
    assert!(!is_automatic_plan_wait(&cp, &db, &with_action));
    db.0.lock().unwrap().execute("UPDATE agent_child_runs SET status='failed',error_kind='timeout',consumed_at='now',applied_at='now' WHERE id=?1", [&id]).unwrap();
    let ask = automatic_plan_approval_instruction(&cp, &db).unwrap().unwrap();
    assert_eq!(ask.get_tool(), "ask_human");
    let material = human_plan_review_material(&cp, &db).unwrap().unwrap();
    assert!(material.contains("new plan"));
    assert!(material.contains("timeout"));
    assert!(!cp.plan_phase.is_approved(), "technical failure is not approval");
    cp.plan_reviewer_consumed = true;
    cp.plan_escalation_required = Some(vec!["expert".into()]);
    cp.plan_reviewer_retry_count = crate::agent::checkpoint::max_plan_reviewer_rounds();
    cp.apply_explicit_plan_decision(&db, &mut c, false, "plan", "please revise");
    let revised_again = serde_json::from_value(json!({"plan_summary":"revised again"})).unwrap();
    assert!(submit_plan_rejection(&cp, &revised_again).is_none(), "human rejection must permit a new plan");
    assert!(cp.plan_review_batch_id.is_some(), "old review history is not deleted");
}

#[test]
fn automatic_approval_discloses_total_dispatch_failure_without_faking_review() {
    let db = DbState(Arc::new(Mutex::new(crate::db::init_db(&":memory:".into()).unwrap())));
    let c = SandwichContext::new("system".into(), "goal".into());
    let mut cp = AgentCheckpoint::new("auto-failed".into(), &c);
    cp.set_execution_mode(PlanExecutionMode::PlanApproval);
    cp.stages.plan_review = Some(true);
    let plan = serde_json::from_value(json!({"plan_summary":"saved unreviewed plan"})).unwrap();
    assert!(crate::agent::plan_submit::record_human_review_dispatch_failure(&mut cp, &plan, 1, vec!["model unavailable".into()]));
    cp.auto_plan_approval_pending = true;
    let ask = automatic_plan_approval_instruction(&cp, &db).unwrap().unwrap();
    assert_eq!(ask.get_tool(), "ask_human");
    let material = human_plan_review_material(&cp, &db).unwrap().unwrap();
    assert!(material.contains("model unavailable"));
    assert!(material.contains("没有 Agent 审查结论"));
    assert!(!cp.plan_phase.is_approved());
    assert!(cp.plan_review_batch_id.is_none());
}

#[test]
fn automatic_approval_wait_survives_checkpoint_reload_without_approval() {
    use crate::agent::checkpoint::{recovery_decision, AgentPhase, RecoveryDecision};
    let db = DbState(Arc::new(Mutex::new(crate::db::init_db(&":memory:".into()).unwrap())));
    let mut context = SandwichContext::new("system".into(), "goal".into());
    let mut cp = AgentCheckpoint::new("approval-restart".into(), &context);
    cp.set_execution_mode(PlanExecutionMode::PlanApproval);
    cp.stages.plan_review = Some(false);
    let plan = serde_json::from_value(json!({"plan_summary":"saved for human"})).unwrap();
    crate::agent::plan_submit::record_plan_without_review(&mut cp, &plan, 1).unwrap();
    cp.auto_plan_approval_pending = true;
    let ask = automatic_plan_approval_instruction(&cp, &db).unwrap().unwrap();
    cp.wait_for_human("approval".into(), &context);
    cp.prepared_instruction = Some(ask);
    save_checkpoint(&db, &mut cp).unwrap();
    let mut loaded = load_checkpoint(&db, "approval-restart").unwrap().unwrap();
    loaded.restore_task_for_entry(&mut context, "");
    loaded.apply_entry_execution_mode(true, None, false);
    assert_eq!(loaded.phase, AgentPhase::WaitUser);
    assert!(matches!(recovery_decision(&loaded), RecoveryDecision::ResumeWaiting));
    assert!(loaded.auto_plan_approval_pending);
    let saved = loaded.prepared_instruction.as_ref().unwrap();
    assert_eq!(saved.get_tool(), "ask_human");
    assert_eq!(saved.get_params()["kind"], "plan_approval");
    assert!(!loaded.plan_phase.is_approved());
    loaded.set_execution_mode(PlanExecutionMode::Auto);
    assert!(!loaded.auto_plan_approval_pending);
}
