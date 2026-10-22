//! 多成员对质回归：临时库 + 本机 HTTP 假模型，走生产续接入口及统一回执消费者。
use super::*;
use crate::agent::child_receipt::{reduce_child_receipt, ReceiptEnv};
use crate::agent::child_reviewer::retirement_tests::MockReviewer;
use crate::agent::child_reviewer::*;
use crate::agent::context::SandwichContext;
use serde_json::{json, Value};
use tokio::sync::watch;

fn verdict(problem: Option<&str>) -> Value {
    json!({"findings":problem.map(|why| vec![json!({"id":"F1","severity":"blocking",
        "location":"src/a.rs:1","why":why,"current":"旧调用","suggestion":"修正调用方",
        "evidence_strength":"supported"})]).unwrap_or_default(),
        "confidence":"supported","evidence_refs":[],"missing_evidence":[],
        "root_cause_verdict":"explains","decision":if problem.is_some() {"rebuttal"}else{"approved"},
        "plan_review_can_finalize":false})
}

fn withdrawn() -> Value {
    let mut v = verdict(None);
    v["objection_verdicts"] = json!([{"finding_id":"F1","verdict":"withdraw",
        "reason":"已核对新参数读取逻辑和调用方测试输出，原先指出的不兼容问题已消除"}]);
    v
}

struct Fixture {
    mock: MockReviewer,
    cp: AgentCheckpoint,
    context: SandwichContext,
    a: String,
    b: String,
    c: String,
    _tx: watch::Sender<u64>,
    rx: watch::Receiver<u64>,
}

impl Fixture {
    async fn new(mock: MockReviewer, consume_b: bool) -> Self {
        Self::with_a_problem(mock, consume_b, Some("A 的权限问题")).await
    }

    async fn with_a_problem(mock: MockReviewer, consume_b: bool, a_problem: Option<&str>) -> Self {
        let context = SandwichContext::new("sys".into(), "修复接口调用并核对已有测试".into());
        let mut cp = AgentCheckpoint::new("state-session".into(), &context);
        cp.plan_mode = PlanExecutionMode::CrossReview;
        cp.submitted_plan = Some(crate::agent::types::PlanSubmission {
            plan_summary: "核对新参数声明与现有调用，更新不兼容调用后执行回归测试".into(),
            ..Default::default()
        });
        cp.plan_phase = PlanPhase::Reviewing;
        cp.plan_reviewer_retry_count = 1;
        cp.set_plan_review_batch("state-batch", 3);
        let a = Self::seed(&mock, verdict(a_problem), "state-batch");
        let b = Self::seed(&mock, verdict(Some("B 的缓存问题")), "state-batch");
        let c = Self::seed(&mock, verdict(None), "state-batch");
        let (tx, rx) = watch::channel(0);
        let mut f = Self {
            mock,
            cp,
            context,
            a,
            b,
            c,
            _tx: tx,
            rx,
        };
        f.consume(&f.a.clone()).await;
        if consume_b {
            f.consume(&f.b.clone()).await;
        }
        f.consume(&f.c.clone()).await;
        f
    }

    fn seed(mock: &MockReviewer, mut result: Value, batch: &str) -> String {
        let mut identity = ChildRunIdentity::fresh("plan_review");
        identity.batch_id = Some(batch.into());
        identity.contract_schema = Some(
            serde_json::to_string(
                &crate::agent::protocol::ContractSchema::new(vec![
                    crate::agent::protocol::ContractFieldDef {
                        name: "root_cause_verdict".into(),
                        field_type: crate::agent::protocol::ContractFieldType::String,
                        description: "病根核对".into(),
                        required: true,
                        allowed_values: None,
                    },
                ])
                .with_strict_closed_set(false),
            )
            .unwrap(),
        );
        let id = begin_child_run_extended(
            &mock.db,
            "state-parent",
            "state-session",
            &mock.root,
            "mock",
            0,
            "review",
            None,
            None,
            None,
            &identity,
        )
        .unwrap();
        result["child_run_id"] = json!(id);
        result["status"] = json!("succeeded");
        result["duration_ms"] = json!(1);
        result["completion_tokens"] = json!(1);
        result["prompt_tokens"] = json!(1);
        let history = json!([{"role":"system","content":"审查同一份方案，返回审查 JSON"},
            {"role":"user","content":"合成证据：接口参数变化，核对权限分支与缓存调用"},
            {"role":"assistant","content":result.to_string()}]);
        finish_child_run_with_history(
            &mock.db,
            &id,
            "succeeded",
            1,
            1,
            1,
            None,
            Some(&result.to_string()),
            Some(&history.to_string()),
        );
        id
    }

    async fn consume(&mut self, id: &str) -> String {
        let receipts =
            poll_completed_delivery_receipts(&self.mock.db, "state-parent", Some(&[id.into()]))
                .unwrap();
        assert_eq!(receipts.len(), 1);
        let env = ReceiptEnv {
            app: None,
            db: &self.mock.db,
            parent_run_id: "state-parent",
            session_id: "state-session",
            workspace_dir: &self.mock.root,
            interrupt_rx: self.rx.clone(),
            reviewer_backup_model: "",
        };
        let effects =
            reduce_child_receipt(&env, &mut self.cp, &mut self.context, &receipts[0]).await;
        self.mock
            .db
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE agent_child_runs SET applied_at='now' WHERE id=?1",
                [id],
            )
            .unwrap();
        effects.model_text
    }

    fn resume(&mut self, target: &str) -> Result<String, ResumeReviewError> {
        spawn_review_resume(
            &self.mock.db,
            "state-parent",
            "state-session",
            &self.mock.root,
            target,
            "已提供新的接口读取代码与测试结果，请核对并处理原有发现。",
            crate::llm::ModelConfig::from_db_state(&self.mock.db, "mock").unwrap(),
            "auto".into(),
            &mut self.cp,
            self.rx.clone(),
            None,
        )
    }

    fn gate(&mut self) -> Option<crate::agent::types::DispatchResult> {
        crate::agent::plan_gate::check_pre_write_gatekeeper(
            &mut self.cp,
            &self.context,
            &self.mock.db,
            "state-parent",
            8,
            false,
        )
    }
}

#[tokio::test]
async fn resume_one_member_preserves_other_blocker_and_closes_only_its_own_f1() {
    let mut f = Fixture::new(MockReviewer::new(vec![withdrawn()]).await, true).await;
    assert_eq!(f.cp.last_review_blocking_count, 2);
    let b = f.b.clone();
    let new = f.resume(&b).unwrap_or_else(|e| panic!("{}", e.message));
    assert_eq!(f.cp.plan_reviewer_retry_count, 2);
    assert_eq!(
        f.cp.last_review_blocking_count, 2,
        "复审在途不能形成放行空窗"
    );
    assert!(f.gate().is_some());
    let old_source = load_run_identity(&f.mock.db, &b).unwrap();
    let new_source = load_run_identity(&f.mock.db, &new).unwrap();
    assert_eq!(old_source.task_id, new_source.task_id);
    assert_eq!(old_source.batch_id, new_source.batch_id);
    assert!(new_source.attempt_id > old_source.attempt_id);
    assert_eq!(f.mock.wait_terminal(&new).await.0, "succeeded");
    f.consume(&new).await;
    assert_eq!(f.cp.plan_review_deliveries.len(), 4, "原始回执仍在历史缓存");
    assert_eq!(f.cp.aggregate_unwithdrawn_blockings().len(), 1);
    assert!(f.cp.aggregate_unwithdrawn_blockings()[0]
        .why
        .contains("A 的权限"));
    assert_eq!(
        f.context
            .todo_list
            .iter()
            .find(|t| t.description.contains("A 的权限"))
            .unwrap()
            .status,
        "pending"
    );
    assert_eq!(
        f.context
            .todo_list
            .iter()
            .find(|t| t.description.contains("B 的缓存"))
            .unwrap()
            .status,
        "done"
    );
    assert!(f.gate().is_some());
    // 真正落库恢复后，身份绑定与当前聚合保持一致。
    f.cp.data = DurableAgentContext::capture(&f.context);
    save_checkpoint(&f.mock.db, &mut f.cp).unwrap();
    f.cp = load_checkpoint(&f.mock.db, "state-session")
        .unwrap()
        .unwrap();
    f.cp.restore_context(&mut f.context);
    assert_eq!(f.cp.last_review_blocking_count, 1);
    assert!(f.gate().is_some());

    // 独立任务只剩 B 有阻塞时，B 明确撤回就能通过，不永久累加其历史意见。
    let mut clean =
        Fixture::with_a_problem(MockReviewer::new(vec![withdrawn()]).await, true, None).await;
    let b = clean.b.clone();
    let new = clean.resume(&b).unwrap_or_else(|e| panic!("{}", e.message));
    clean.mock.wait_terminal(&new).await;
    clean.consume(&new).await;
    assert_eq!(clean.cp.last_review_blocking_count, 0);
    assert!(clean.context.todo_list.iter().all(|t| t.status == "done"));
    assert!(clean.gate().is_none());
}

#[tokio::test]
async fn identical_f1_text_from_two_tasks_is_not_deduplicated_or_withdrawn_together() {
    let mut f = Fixture::with_a_problem(
        MockReviewer::new(vec![withdrawn()]).await,
        true,
        Some("B 的缓存问题"),
    )
    .await;
    assert_eq!(f.context.todo_list.len(), 2);
    let b = f.b.clone();
    let new = f.resume(&b).unwrap_or_else(|e| panic!("{}", e.message));
    f.mock.wait_terminal(&new).await;
    f.consume(&new).await;
    assert_eq!(
        f.context
            .todo_list
            .iter()
            .filter(|t| t.status == "pending")
            .count(),
        1
    );
    assert_eq!(
        f.context
            .todo_list
            .iter()
            .filter(|t| t.status == "done")
            .count(),
        1
    );
    assert_eq!(f.cp.last_review_blocking_count, 1);
}

#[tokio::test]
async fn approved_plan_resume_waits_and_honors_a_new_revision_decision() {
    let mut updated = verdict(Some("续审发现的权限缺口"));
    updated["findings"][0]["id"] = json!("F2");
    let mock = MockReviewer::new(vec![withdrawn(), updated]).await;
    let mut f = Fixture::with_a_problem(mock, true, None).await;
    let b = f.b.clone();
    let resolved = f.resume(&b).unwrap_or_else(|e| panic!("{}", e.message));
    f.mock.wait_terminal(&resolved).await;
    f.consume(&resolved).await;
    assert!(f.gate().is_none());
    assert!(f.cp.plan_phase.is_approved());
    let c = f.c.clone();
    let next = f.resume(&c).unwrap_or_else(|e| panic!("{}", e.message));
    assert_eq!(f.gate().expect("复审在途不能沿用旧批准直接写入").route,
        "plan_gatekeeper:need_wait_reviewer_jobs");
    f.mock.wait_terminal(&next).await;
    f.consume(&next).await;
    f.cp = serde_json::from_value(serde_json::to_value(&f.cp).unwrap()).unwrap();
    assert_eq!(f.gate().expect("复审的新阻塞必须生效").route,
        "plan_gatekeeper:blocking_findings_open");
}

#[tokio::test]
async fn approved_plan_resume_failure_preserves_its_previous_valid_approval() {
    let mut invalid = verdict(None);
    invalid.as_object_mut().unwrap().remove("root_cause_verdict");
    let mut f = Fixture::with_a_problem(MockReviewer::new(vec![withdrawn(), invalid]).await, true, None).await;
    let b = f.b.clone();
    let resolved = f.resume(&b).unwrap_or_else(|e| panic!("{}", e.message));
    f.mock.wait_terminal(&resolved).await;
    f.consume(&resolved).await;
    assert!(f.gate().is_none());
    let c = f.c.clone();
    let next = f.resume(&c).unwrap_or_else(|e| panic!("{}", e.message));
    assert!(f.gate().is_some());
    assert_eq!(f.mock.wait_terminal(&next).await.0, "invalid_output");
    f.consume(&next).await;
    assert_eq!(f.cp.last_review_blocking_count, 0);
    assert!(f.gate().is_none(), "失败尝试不构成撤销已有有效批准的证据");
    assert!(f.cp.plan_phase.is_approved());
}

#[tokio::test]
async fn stage_rebuttal_does_not_reopen_the_approved_plan_review() {
    let mut f = Fixture::new(MockReviewer::new(vec![verdict(None)]).await, true).await;
    f.cp.plan_phase = PlanPhase::Approved;
    f.cp.stage_review_batch_id = Some("stage-batch".into());
    f.mock.db.0.lock().unwrap().execute(
        "UPDATE agent_child_runs SET purpose='stage_review',batch_id='stage-batch' WHERE id=?1",
        [&f.c],
    ).unwrap();
    let c = f.c.clone();
    let next = f.resume(&c).unwrap_or_else(|e| panic!("{}", e.message));
    assert!(f.cp.plan_phase.is_approved(), "阶段复检不能变成方案重审");
    assert_eq!(f.mock.wait_terminal(&next).await.0, "succeeded");
    f.consume(&next).await;
    assert!(f.cp.plan_phase.is_approved());
}

#[tokio::test]
async fn supplementary_review_resume_does_not_create_a_plan_approval_requirement() {
    let mut f = Fixture::new(MockReviewer::new(vec![verdict(None)]).await, true).await;
    f.cp.plan_phase = PlanPhase::Approved;
    f.cp.plan_mode = PlanExecutionMode::Auto;
    f.cp.submitted_plan = None;
    let c = f.c.clone();
    let next = f.resume(&c).unwrap_or_else(|e| panic!("{}", e.message));
    assert!(f.cp.plan_phase.is_approved(), "附加审查不能新建方案审批义务");
    f.mock.wait_terminal(&next).await;
    f.consume(&next).await;
    assert!(f.cp.plan_phase.is_approved());
}

#[tokio::test]
async fn human_waiver_closes_only_bound_current_review_todos_without_forging_withdrawals() {
    let mut f = Fixture::new(MockReviewer::new(vec![verdict(Some("B 新一轮明确维持"))]).await, true).await;
    assert_eq!(f.context.todo_list.len(), 2);
    f.cp.plan_phase = PlanPhase::Approved;
    f.cp.last_review_blocking_count = 0;
    f.cp.record_human_review_waiver(&f.mock.db, &mut f.context,
        "人工批准：这两项已知风险放行，原意见保留供终审参考");
    assert!(crate::agent::finish_guard::open_todo_descriptions(&f.context.todo_list).is_empty(),
        "人工已明确放行，自动审查待办不能继续把交付锁住");
    assert_eq!(f.cp.aggregate_unwithdrawn_blockings().len(), 2, "人工裁决不能改写审查员的原始判断");
    f.cp.sync_review_todos(&f.mock.db, &mut f.context);
    assert!(f.context.todo_list.iter().all(|t| t.status == "done"), "相同观察重放不能重新开启已人工核销的待办");
    f.cp = serde_json::from_value(serde_json::to_value(&f.cp).unwrap()).unwrap();
    let b = f.b.clone();
    let next = f.resume(&b).unwrap_or_else(|e| panic!("{}", e.message));
    // 后来的有效新阻塞不能自动继承对旧问题的核销。
    f.mock.wait_terminal(&next).await;
    f.consume(&next).await;
    assert!(f.context.todo_list.iter().any(|t| t.status == "pending" && t.description.contains("B 新一轮")));
}

#[tokio::test]
async fn human_review_waiver_keeps_user_edits_and_other_batches_open() {
    let mut f = Fixture::new(MockReviewer::new(vec![verdict(None)]).await, true).await;
    let edited = f.context.todo_list[0].id;
    f.context.todo_list[0].description = "用户改写的 F1 任务".into();
    f.context.todo_list.push(TodoItem { id:100, description:"用户自己的 F1 检查".into(), status:"pending".into(), evidence_output_id:None });
    f.context.todo_list.push(TodoItem { id:101, description:"旧批次的 F1 待办".into(), status:"pending".into(), evidence_output_id:None });
    let mut old_binding = f.cp.review_finding_todos.values().next().unwrap().clone();
    old_binding.description = "旧批次的 F1 待办".into();
    old_binding.source.batch_id = Some("old-batch".into());
    f.cp.review_finding_todos.insert(101, old_binding);
    f.cp.record_human_review_waiver(&f.mock.db, &mut f.context, "仅接受当前审查指出的已知风险");
    for id in [edited, 100, 101] {
        assert_eq!(f.context.todo_list.iter().find(|t| t.id == id).unwrap().status, "pending");
    }
    assert!(f.context.todo_list.iter().filter(|t| t.id != edited && t.id != 100 && t.id != 101).all(|t| t.status == "done"));
}

#[tokio::test]
async fn multi_member_receipts_cannot_overwrite_the_shared_plan_even_with_legacy_finalization_flags() {
    for reverse in [false, true] {
        let mut f = Fixture::new(MockReviewer::new(vec![verdict(None)]).await, true).await;
        let original = serde_json::to_value(&f.cp.submitted_plan).unwrap();
        let mut ids = Vec::new();
        for summary in ["A 的独立修订建议", "B 的冲突修订建议"] {
            let mut raw = verdict(None);
            raw["plan_review_can_finalize"] = json!(true);
            raw["revised_plan_summary"] = json!(summary);
            ids.push(Fixture::seed(&f.mock, raw, "manual-team-batch"));
        }
        f.cp.set_plan_review_batch("manual-team-batch", 2);
        if reverse { ids.reverse(); }
        for id in ids {
            let text = f.consume(&id).await;
            assert_eq!(serde_json::to_value(&f.cp.submitted_plan).unwrap(), original,
                "多人批次必须审同一份方案，不能由回执先后决定执行版本");
            assert!(!text.contains("请按定稿版本执行"));
            assert_eq!(f.cp.plan_review_deliveries[&id].extra["plan_review_can_finalize"], false);
        }
        assert!(f.cp.plan_finalized_by.is_none());
    }
}

#[tokio::test]
async fn downgraded_finding_updates_todo_and_no_longer_blocks_finish() {
    for severity in ["suggestion", "question"] {
        let mut updated = verdict(Some("B 的缓存问题"));
        updated["findings"][0]["severity"] = json!(severity);
        updated["decision"] = json!("approved");
        let mut f =
            Fixture::with_a_problem(MockReviewer::new(vec![updated]).await, true, None).await;
        let todo_id = f.context.todo_list[0].id;
        let b = f.b.clone();
        let new = f.resume(&b).unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(f.cp.last_review_blocking_count, 1, "复审在途仍保留旧阻塞");
        assert_eq!(f.mock.wait_terminal(&new).await.0, "succeeded");
        f.consume(&new).await;
        assert_eq!(f.cp.last_review_blocking_count, 0);
        assert_eq!(f.context.todo_list.len(), 1, "更新原待办，不新增参考待办");
        assert_eq!(f.context.todo_list[0].id, todo_id);
        assert_eq!(
            f.context.todo_list[0].status, "done",
            "明确降级应解除旧阻塞待办"
        );
        assert!(!f.context.todo_list[0].description.contains("[BLOCKING]"));
        assert!(
            crate::agent::finish_guard::open_todo_descriptions(&f.context.todo_list).is_empty()
        );
        assert!(f.gate().is_none());

        f.cp.data = DurableAgentContext::capture(&f.context);
        save_checkpoint(&f.mock.db, &mut f.cp).unwrap();
        f.cp = load_checkpoint(&f.mock.db, "state-session")
            .unwrap()
            .unwrap();
        f.cp.restore_context(&mut f.context);
        f.cp.sync_review_todos(&f.mock.db, &mut f.context);
        assert_eq!(f.context.todo_list[0].status, "done");
        assert!(f.cp.aggregate_unwithdrawn_blockings().is_empty());
    }
}

#[tokio::test]
async fn downgraded_finding_survives_omission_and_reopens_only_its_own_todo_on_upgrade() {
    let mut downgraded = verdict(Some("B 的缓存问题"));
    downgraded["findings"][0]["severity"] = json!("suggestion");
    downgraded["decision"] = json!("approved");
    let mut f = Fixture::new(MockReviewer::new(vec![downgraded]).await, true).await;
    let b_todo = f
        .context
        .todo_list
        .iter()
        .find(|t| t.description.contains("B 的缓存"))
        .unwrap()
        .id;
    let mut target = f.b.clone();
    for (index, expected_blocking) in [1, 1, 2].into_iter().enumerate() {
        let new = if index == 0 {
            f.resume(&target)
                .unwrap_or_else(|e| panic!("{}", e.message))
        } else {
            // 后续版本只检验合成历史的消费者语义；不增加或绕过生产续接预算。
            let identity = ChildRunIdentity::inherited(
                &load_run_identity(&f.mock.db, &target).unwrap(),
                Some("plan_review".into()),
            );
            let id = begin_child_run_extended(
                &f.mock.db,
                "state-parent",
                "state-session",
                &f.mock.root,
                "mock",
                0,
                "review",
                None,
                None,
                Some(&target),
                &identity,
            )
            .unwrap();
            let mut result = if index == 1 {
                verdict(None)
            } else {
                verdict(Some("B 的缓存问题"))
            };
            result["child_run_id"] = json!(id);
            result["status"] = json!("succeeded");
            result["duration_ms"] = json!(1);
            result["completion_tokens"] = json!(1);
            result["prompt_tokens"] = json!(1);
            finish_child_run(
                &f.mock.db,
                &id,
                "succeeded",
                1,
                1,
                1,
                None,
                Some(&result.to_string()),
            );
            id
        };
        assert_eq!(f.mock.wait_terminal(&new).await.0, "succeeded");
        f.consume(&new).await;
        assert_eq!(f.cp.last_review_blocking_count, expected_blocking);
        assert_eq!(f.context.todo_list.len(), 2);
        assert_eq!(
            f.context
                .todo_list
                .iter()
                .find(|t| t.id == b_todo)
                .unwrap()
                .status,
            if index == 2 { "pending" } else { "done" }
        );
        assert_eq!(
            f.context
                .todo_list
                .iter()
                .find(|t| t.description.contains("A 的权限"))
                .unwrap()
                .status,
            "pending"
        );
        assert!(f.gate().is_some(), "A 的独立阻塞始终保留");
        target = new;
    }
}

#[tokio::test]
async fn approved_response_that_omits_an_old_issue_is_not_an_explicit_withdrawal() {
    let mut f = Fixture::new(MockReviewer::new(vec![verdict(None)]).await, true).await;
    let b = f.b.clone();
    let new = f.resume(&b).unwrap_or_else(|e| panic!("{}", e.message));
    f.mock.wait_terminal(&new).await;
    f.consume(&new).await;
    assert_eq!(f.cp.last_review_blocking_count, 2);
    assert!(f.context.todo_list.iter().all(|t| t.status == "pending"));
    assert!(f.gate().is_some());
}

#[tokio::test]
async fn late_old_receipt_cannot_restore_withdrawn_blocker_or_old_decision() {
    let mut f = Fixture::new(MockReviewer::new(vec![withdrawn()]).await, false).await;
    let b = f.b.clone();
    let new = f.resume(&b).unwrap_or_else(|e| panic!("{}", e.message));
    f.mock.wait_terminal(&new).await;
    f.consume(&new).await;
    let before = f.cp.plan_gate_projection().blocking_count;
    let text = f.consume(&b).await;
    assert!(text.contains("历史回执"));
    assert_eq!(f.cp.plan_gate_projection().blocking_count, before);
    assert_eq!(before, 1);
    assert_eq!(
        f.cp.current_review_deliveries(Some(&f.mock.db))
            .iter()
            .find(|(_, source, _)| source.task_id
                == load_run_identity(&f.mock.db, &b).unwrap().task_id)
            .unwrap()
            .2
            .get_extra_str("decision"),
        Some("approved")
    );
    assert!(!f
        .context
        .todo_list
        .iter()
        .any(|t| t.description.contains("B 的缓存") && t.status == "pending"));
}

#[tokio::test]
async fn invalid_resume_keeps_both_old_blockers_and_reports_channel_failure() {
    let mut invalid = withdrawn();
    invalid
        .as_object_mut()
        .unwrap()
        .remove("root_cause_verdict");
    let mut f = Fixture::new(MockReviewer::new(vec![invalid]).await, true).await;
    let b = f.b.clone();
    let new = f.resume(&b).unwrap_or_else(|e| panic!("{}", e.message));
    assert_eq!(f.mock.wait_terminal(&new).await.0, "invalid_output");
    f.consume(&new).await;
    assert_eq!(f.cp.last_review_blocking_count, 2);
    assert_eq!(f.cp.plan_review_deliveries.len(), 3);
    assert!(f.context.todo_list.iter().all(|t| t.status == "pending"));
    assert!(f.cp.pending_task_failure.is_none());
    assert!(f.gate().is_some());
}

#[tokio::test]
async fn timed_out_resume_does_not_revoke_any_previous_conclusion() {
    let mut f = Fixture::new(MockReviewer::silent().await, true).await;
    let b = f.b.clone();
    let new = f.resume(&b).unwrap_or_else(|e| panic!("{}", e.message));
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while f.mock.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(601)).await;
    tokio::task::yield_now().await;
    tokio::time::resume();
    assert_eq!(f.mock.wait_terminal(&new).await.0, "timed_out");
    f.consume(&new).await;
    assert_eq!(f.cp.last_review_blocking_count, 2);
    assert!(f.context.todo_list.iter().all(|t| t.status == "pending"));
    assert!(f.cp.pending_task_failure.is_none());
    assert!(f.gate().is_some());
}

#[tokio::test]
async fn new_finding_keeps_its_own_id_and_does_not_reopen_withdrawn_f1() {
    let mut updated = withdrawn();
    updated["decision"] = json!("rebuttal");
    updated["findings"] = json!([{"id":"F2","severity":"blocking","why":"B 新发现的超时问题"}]);
    let mut f = Fixture::new(MockReviewer::new(vec![updated]).await, true).await;
    let b = f.b.clone();
    let new = f.resume(&b).unwrap_or_else(|e| panic!("{}", e.message));
    f.mock.wait_terminal(&new).await;
    f.consume(&new).await;
    assert_eq!(f.cp.last_review_blocking_count, 2);
    assert_eq!(
        f.context
            .todo_list
            .iter()
            .find(|t| t.description.contains("B 的缓存"))
            .unwrap()
            .status,
        "done"
    );
    assert!(f
        .context
        .todo_list
        .iter()
        .any(|t| t.description.contains("[F2]") && t.status == "pending"));
    assert!(f
        .context
        .todo_list
        .iter()
        .any(|t| t.description.contains("A 的权限") && t.status == "pending"));
}

#[tokio::test]
async fn old_checkpoint_adopts_unique_todo_source_but_never_guesses_ambiguous_ones() {
    let mut f = Fixture::new(MockReviewer::new(vec![withdrawn()]).await, true).await;
    let mut snapshot = serde_json::to_value(&f.cp).unwrap();
    snapshot
        .as_object_mut()
        .unwrap()
        .remove("review_delivery_sources");
    snapshot
        .as_object_mut()
        .unwrap()
        .remove("review_finding_todos");
    f.cp = serde_json::from_value(snapshot).unwrap();
    let b = f.b.clone();
    let new = f.resume(&b).unwrap_or_else(|e| panic!("{}", e.message));
    f.mock.wait_terminal(&new).await;
    f.consume(&new).await;
    assert_eq!(
        f.context
            .todo_list
            .iter()
            .find(|t| t.description.contains("B 的缓存"))
            .unwrap()
            .status,
        "done"
    );
    assert_eq!(
        f.context
            .todo_list
            .iter()
            .find(|t| t.description.contains("A 的权限"))
            .unwrap()
            .status,
        "pending"
    );

    // 两位历史成员曾使用完全相同的文字，旧 TODO 无法唯一归属。
    let a_delivery = f.cp.plan_review_deliveries[&f.a].clone();
    f.cp.plan_review_deliveries.insert(f.b.clone(), a_delivery);
    f.context.todo_list.truncate(1);
    f.cp.review_finding_todos.clear();
    assert!(f.cp.sync_review_todos(&f.mock.db, &mut f.context));
    assert_eq!(f.context.todo_list[0].status, "pending");
}

#[tokio::test]
async fn old_batch_withdrawal_cannot_close_current_batch_f1() {
    let mut f = Fixture::new(MockReviewer::new(vec![withdrawn()]).await, true).await;
    let old = Fixture::seed(&f.mock, withdrawn(), "other-batch");
    let before = f.cp.last_review_blocking_count;
    let text = f.consume(&old).await;
    assert!(text.contains("其他批次"));
    assert_eq!(f.cp.last_review_blocking_count, before);
    assert!(f.context.todo_list.iter().all(|t| t.status == "pending"));
}

#[tokio::test]
async fn exhausted_resume_budget_creates_no_orphan_and_preserves_effective_state() {
    let mut f = Fixture::new(MockReviewer::new(vec![withdrawn()]).await, true).await;
    f.cp.plan_reviewer_retry_count = max_plan_reviewer_rounds();
    let b = f.b.clone();
    let error = f.resume(&b).err().unwrap();
    assert_eq!(error.route, "plan_gatekeeper:reviewer_retry_limit");
    assert_eq!(f.cp.last_review_blocking_count, 2);
    let count: i64 = f
        .mock
        .db
        .0
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM agent_child_runs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 3);
    assert!(f.mock.requests.lock().unwrap().is_empty());
}
