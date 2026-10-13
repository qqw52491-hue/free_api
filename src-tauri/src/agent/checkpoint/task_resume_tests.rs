use super::*;
use serde_json::json;

fn failed_fixture() -> (SandwichContext, AgentCheckpoint) {
    let mut ctx = SandwichContext::new("system".into(), "原始缓存目标".into());
    ctx.current_goal = "简洁模式与详细模式保留相同子任务和审查内容".into();
    ctx.review_contract = Some(
        serde_json::from_value(json!({"scope_statement": "验证原任务", "obligations": [{
            "id": "verify_build", "question": "构建通过", "status": "open",
            "verify_command": "cargo check", "closure_form": "command"
        }]}))
        .unwrap(),
    );
    let mut cp = AgentCheckpoint::new("session".into(), &ctx);
    cp.set_execution_mode(PlanExecutionMode::CrossReview);
    cp.triage_verdict = Some(crate::agent::triage::TriageVerdict::fallback("fixture"));
    cp.triage_verdict.as_mut().unwrap().stage_review_mode = "none".into();
    cp.plan_phase = PlanPhase::Reviewing;
    cp.submitted_plan = Some(crate::agent::types::PlanSubmission {
        plan_summary: "保持内容一致，仅简化前端展示样式".into(),
        target_files: vec!["src/components/a.vue".into()],
        evidence_refs: vec!["src/components/a.vue:20".into()],
        final_review_mode: "code_review".into(),
        stage_review_mode: "none".into(),
        ..Default::default()
    });
    cp.approved_target_files = Some(vec!["src/components/a.vue".into()]);
    cp.plan_review_batch_id = Some("cancelled-plan-batch".into());
    cp.plan_review_expected_count = 1;
    cp.plan_reviewer_retry_count = 3;
    cp.plan_reviewer_failover_count = 2;
    cp.stage_review_batch_id = Some("cancelled-stage-batch".into());
    cp.mark_failed("模型凭据失效后用户停止".into(), &ctx);
    (ctx, cp)
}

#[test]
fn stopped_task_restores_goal_contract_and_review_requirements() {
    let (original, mut cp) = failed_fixture();
    let triage = serde_json::to_value(&cp.triage_verdict).unwrap();
    let plan = serde_json::to_value(&cp.submitted_plan).unwrap();
    let mut next = SandwichContext::new("new model prompt".into(), "继续".into());
    cp.restore_task_for_entry(&mut next, "换 Flash，继续");
    cp.apply_entry_execution_mode(true, None, true);
    assert_eq!(next.current_goal, original.current_goal);
    assert_eq!(next.user_goal, original.user_goal);
    assert_eq!(next.review_contract, original.review_contract);
    assert_eq!(serde_json::to_value(&cp.triage_verdict).unwrap(), triage);
    assert_eq!(serde_json::to_value(&cp.submitted_plan).unwrap(), plan);
    assert_eq!(
        cp.approved_target_files.as_ref().unwrap(),
        &vec!["src/components/a.vue".to_string()]
    );
    assert_eq!(cp.plan_mode, PlanExecutionMode::CrossReview);
    assert_eq!(cp.plan_phase, PlanPhase::Reviewing);
    assert!(!cp.plan_phase.is_approved());
    assert_eq!(recovery_decision(&cp), RecoveryDecision::ResumePlanning);
    assert!(cp.prepared_instruction.is_none());
    assert!(cp.plan_review_batch_id.is_none());
    assert!(cp.stage_review_batch_id.is_none());
    assert_eq!(cp.plan_reviewer_retry_count, 0);
    assert!(crate::agent::plan_gate::submit_plan_rejection(
        &cp,
        cp.submitted_plan.as_ref().unwrap()
    )
    .is_none());
    assert!(next.turns_history.iter().any(|m| m
        .content
        .as_str()
        .is_some_and(|s| s.contains("已保存的待审方案"))));
}

#[test]
fn real_rejections_and_stage_blockers_survive_retry() {
    let (_, mut cp) = failed_fixture();
    cp.last_review_blocking_count = 2;
    cp.stage_review_blockers_open = 1;
    cp.stage_review_blocker_source_run_id = Some("real-review".into());
    cp.plan_escalation_required = Some(vec!["security_specialist".into()]);
    let mut next = SandwichContext::new("sys".into(), "继续".into());
    cp.restore_task_for_entry(&mut next, "继续");
    assert_eq!(cp.last_review_blocking_count, 2);
    assert_eq!(cp.stage_review_blockers_open, 1);
    assert_eq!(
        cp.stage_review_blocker_source_run_id.as_deref(),
        Some("real-review")
    );
    assert_eq!(
        cp.plan_escalation_required.as_ref().unwrap(),
        &vec!["security_specialist".to_string()]
    );
    assert!(!cp.plan_phase.is_approved());
}

#[test]
fn approved_plan_stays_approved_but_explicit_mode_change_is_honored() {
    let (_, mut cp) = failed_fixture();
    cp.plan_phase = PlanPhase::Approved;
    cp.approved_plan_summary = Some("人类已批准原方案".into());
    cp.plan_reviewer_consumed = true;
    let mut next = SandwichContext::new("sys".into(), "继续".into());
    cp.restore_task_for_entry(&mut next, "换模型继续");
    cp.apply_entry_execution_mode(true, None, true);
    assert_eq!(cp.plan_phase, PlanPhase::Approved);
    assert_eq!(
        cp.approved_plan_summary.as_deref(),
        Some("人类已批准原方案")
    );
    assert!(cp.plan_reviewer_consumed);
    cp.apply_entry_execution_mode(true, Some("plan_approval"), false);
    assert_eq!(cp.plan_mode, PlanExecutionMode::PlanApproval);
    cp.apply_entry_execution_mode(true, Some("auto"), true);
    assert_eq!(cp.plan_mode, PlanExecutionMode::Auto);
}

#[test]
fn uncertain_write_is_not_replayed_or_erased_on_resume() {
    let (ctx, mut cp) = failed_fixture();
    let inst: AgentInstruction = serde_json::from_value(json!({
        "tool": "local_file", "action": "write", "description": "write file",
        "params": {"action": "write", "path": "a.txt", "content": "hello"}
    }))
    .unwrap();
    cp.pending = Some(PendingToolCall::new(
        inst.clone(),
        RecoveryPolicy::HumanReview,
    ));
    cp.prepared_instruction = Some(inst);
    cp.mark_failed("interrupted write".into(), &ctx);
    assert!(cp.pending.is_some(), "失败不能证明写入没有发生");
    let call_id = cp.pending.as_ref().unwrap().call_id.clone();
    let mut next = SandwichContext::new("sys".into(), "继续".into());
    cp.restore_task_for_entry(&mut next, "继续");
    assert_eq!(cp.pending.as_ref().unwrap().call_id, call_id);
    assert!(cp.prepared_instruction.is_none());
    assert_eq!(recovery_decision(&cp), RecoveryDecision::RequireHumanReview);
}

#[test]
fn new_task_keeps_conversation_but_not_previous_review_state() {
    let (_, cp) = failed_fixture();
    let mut next = SandwichContext::new("sys".into(), "新的独立任务".into());
    cp.data.inherit_filling_into(&mut next);
    let mut fresh = AgentCheckpoint::new("session".into(), &next);
    fresh.apply_entry_execution_mode(false, Some("auto"), true);
    assert_eq!(next.current_goal, "新的独立任务");
    assert!(fresh.triage_verdict.is_none());
    assert!(fresh.submitted_plan.is_none());
    assert!(fresh.data.review_contract.is_none());
    assert!(fresh.task_git_baseline.is_none());
    assert_eq!(fresh.plan_phase, PlanPhase::Exploring);
}

#[test]
fn checkpoint_roundtrip_preserves_task_diff_origin_and_legacy_defaults() {
    let (_, mut cp) = failed_fixture();
    let mut baseline = crate::agent::reviewer_gatekeeper::GitBaseline::non_git();
    baseline.is_git = true;
    baseline.base_head = Some("original-task-head".into());
    baseline
        .pre_dirty
        .insert("user.txt".into(), "original-user-hash".into());
    cp.task_git_baseline = Some(baseline);
    cp.task_started_at = Some(chrono::Utc::now());
    let mut saved = serde_json::to_value(&cp).unwrap();
    let restored: AgentCheckpoint = serde_json::from_value(saved.clone()).unwrap();
    let base = restored.task_git_baseline.unwrap();
    assert_eq!(base.base_head.as_deref(), Some("original-task-head"));
    assert_eq!(
        base.pre_dirty.get("user.txt").unwrap(),
        "original-user-hash"
    );
    assert_eq!(restored.task_started_at, cp.task_started_at);
    saved.as_object_mut().unwrap().remove("task_git_baseline");
    saved.as_object_mut().unwrap().remove("task_started_at");
    let legacy: AgentCheckpoint = serde_json::from_value(saved).unwrap();
    assert!(legacy.task_git_baseline.is_none());
    assert!(legacy.task_started_at.is_none());
}

#[test]
fn resumed_diff_includes_pre_stop_writes_but_excludes_original_user_edits() {
    let dir = std::env::temp_dir().join(format!("resume-diff-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@example.invalid"]);
    git(&["config", "user.name", "Resume regression"]);
    std::fs::write(dir.join("user.txt"), "user base\n").unwrap();
    std::fs::write(dir.join("task.txt"), "task base\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "base"]);
    std::fs::write(dir.join("user.txt"), "original user edit\n").unwrap();
    let (_, mut cp) = failed_fixture();
    cp.task_git_baseline =
        Some(crate::agent::reviewer_gatekeeper::GitBaseline::capture_at_run_start(&dir));
    std::fs::write(dir.join("task.txt"), "agent edit before stop\n").unwrap();
    let before = cp
        .task_git_baseline
        .as_ref()
        .unwrap()
        .collect_git_diff(&dir)
        .unwrap();
    let saved = serde_json::to_string(&cp).unwrap();
    let mut restored: AgentCheckpoint = serde_json::from_str(&saved).unwrap();
    let mut next = SandwichContext::new("new model".into(), "继续".into());
    restored.restore_task_for_entry(&mut next, "换模型继续");
    let after = restored
        .task_git_baseline
        .as_ref()
        .unwrap()
        .collect_git_diff(&dir)
        .unwrap();
    assert_eq!(before, after, "换模型/恢复不能改变审查题面");
    assert!(after.0.contains("task.txt"));
    assert!(!after.0.contains("original user edit"));
    std::fs::write(dir.join("task.txt"), "agent edit after resume\n").unwrap();
    let changed = restored
        .task_git_baseline
        .as_ref()
        .unwrap()
        .collect_git_diff(&dir)
        .unwrap();
    assert_ne!(after.1, changed.1, "实质改动必须改变终审指纹");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cancelled_unconsumed_stage_wait_refunds_only_the_unfinished_round() {
    let dir = std::env::temp_dir().join(format!("resume-stage-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = crate::db::DbState(std::sync::Arc::new(std::sync::Mutex::new(
        crate::db::init_db(&dir.join("t.db")).unwrap(),
    )));
    let (_ctx, mut cp) = failed_fixture();
    cp.stage_review_budget_used = 2;
    cp.stage_review_baseline_side_effects = 7;
    cp.stage_review_shell_baseline = 4;
    db.0.lock().unwrap().execute(
        "INSERT INTO agent_child_runs (id,parent_run_id,parent_session_id,workspace_root,model_id,status,started_at,kind,task_id,attempt_id,purpose,batch_id)
         VALUES ('old-stage','old-run','session','/tmp','model','cancelled','t','review','stage-task',1,'stage_review','cancelled-stage-batch')", [],
    ).unwrap();
    cp.retire_stopped_stage_wait(&db).unwrap();
    assert_eq!(cp.stage_review_budget_used, 1);
    assert_eq!(cp.stage_review_shell_baseline, 0);
    cp.stage_review_budget_used = 2;
    cp.stage_review_blockers_open = 1;
    cp.retire_stopped_stage_wait(&db).unwrap();
    assert_eq!(cp.stage_review_budget_used, 2, "真实问题不能当故障退款");
    cp.stage_review_blockers_open = 0;
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE agent_child_runs SET status='succeeded',consumed_at='t' WHERE id='old-stage'",
            [],
        )
        .unwrap();
    cp.retire_stopped_stage_wait(&db).unwrap();
    assert_eq!(cp.stage_review_budget_used, 2, "真实成功结论不能被退款");
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}
